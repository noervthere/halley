use std::time::Duration;

use halley_config::{AnimationCurve, AnimationMotion, SpringMotion};

const MAX_SPRING_DURATION: Duration = Duration::from_secs(10);
const SPRING_DURATION_STEP: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug)]
pub struct MotionTimeline {
    started_at: Duration,
    motion: AnimationMotion,
    duration: Duration,
}

impl MotionTimeline {
    pub fn new(motion: AnimationMotion, started_at: Duration) -> Self {
        let duration = match motion {
            AnimationMotion::Easing(easing) => Duration::from_millis(u64::from(easing.duration_ms)),
            AnimationMotion::Spring(spring) => spring_duration(spring),
        };
        Self {
            started_at,
            motion,
            duration,
        }
    }

    pub fn value_at(self, now: Duration) -> f64 {
        let elapsed = now.saturating_sub(self.started_at);
        match self.motion {
            AnimationMotion::Easing(easing) => {
                if self.duration.is_zero() {
                    return 1.0;
                }
                let linear = (elapsed.as_secs_f64() / self.duration.as_secs_f64()).clamp(0.0, 1.0);
                apply_curve(easing.curve, linear)
            }
            AnimationMotion::Spring(spring) => {
                if elapsed >= self.duration {
                    1.0
                } else {
                    spring_value(spring, elapsed.as_secs_f64())
                }
            }
        }
    }

    pub fn is_finished_at(self, now: Duration) -> bool {
        now.saturating_sub(self.started_at) >= self.duration
    }
}

pub fn apply_curve(curve: AnimationCurve, progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    match curve {
        AnimationCurve::Linear => progress,
        AnimationCurve::EaseOutQuad => 1.0 - (1.0 - progress).powi(2),
        AnimationCurve::EaseOutCubic => 1.0 - (1.0 - progress).powi(3),
        AnimationCurve::EaseOutExpo if progress == 1.0 => 1.0,
        AnimationCurve::EaseOutExpo => 1.0 - 2.0_f64.powf(-10.0 * progress),
        AnimationCurve::EaseOutBack => {
            let overshoot = 1.42;
            let shifted = progress - 1.0;
            1.0 + shifted * shifted * ((overshoot + 1.0) * shifted + overshoot)
        }
    }
}

fn spring_value(spring: SpringMotion, seconds: f64) -> f64 {
    let omega = spring.stiffness.sqrt();
    let damping = spring.damping_ratio;
    let displacement = if (damping - 1.0).abs() < 1e-7 {
        (1.0 + omega * seconds) * (-omega * seconds).exp()
    } else if damping < 1.0 {
        let damped = omega * (1.0 - damping * damping).sqrt();
        let coefficient = damping * omega / damped;
        (-damping * omega * seconds).exp()
            * ((damped * seconds).cos() + coefficient * (damped * seconds).sin())
    } else {
        let root = (damping * damping - 1.0).sqrt();
        let first = -omega * (damping - root);
        let second = -omega * (damping + root);
        let first_coefficient = -second / (first - second);
        let second_coefficient = first / (first - second);
        first_coefficient * (first * seconds).exp() + second_coefficient * (second * seconds).exp()
    };

    let value = 1.0 - displacement;
    if value.is_finite() { value } else { 1.0 }
}

fn spring_duration(spring: SpringMotion) -> Duration {
    let mut elapsed = Duration::ZERO;
    let mut last_unsettled = Duration::ZERO;
    while elapsed <= MAX_SPRING_DURATION {
        if (1.0 - spring_value(spring, elapsed.as_secs_f64())).abs() > spring.epsilon {
            last_unsettled = elapsed;
        }
        elapsed += SPRING_DURATION_STEP;
    }

    (last_unsettled + SPRING_DURATION_STEP).min(MAX_SPRING_DURATION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use halley_config::{EasingMotion, SpringMotion};

    #[test]
    fn easing_preserves_endpoints() {
        for curve in [
            AnimationCurve::Linear,
            AnimationCurve::EaseOutQuad,
            AnimationCurve::EaseOutCubic,
            AnimationCurve::EaseOutExpo,
            AnimationCurve::EaseOutBack,
        ] {
            assert_eq!(apply_curve(curve, 0.0), 0.0);
            assert_eq!(apply_curve(curve, 1.0), 1.0);
        }
    }

    #[test]
    fn critical_spring_converges_and_finishes() {
        let timeline = MotionTimeline::new(
            AnimationMotion::Spring(SpringMotion::default()),
            Duration::from_secs(1),
        );

        assert_eq!(timeline.value_at(Duration::from_secs(1)), 0.0);
        assert!(timeline.value_at(Duration::from_millis(1100)) > 0.7);
        assert_eq!(timeline.value_at(Duration::from_secs(2)), 1.0);
        assert!(timeline.is_finished_at(Duration::from_secs(2)));
    }

    #[test]
    fn underdamped_spring_can_overshoot() {
        let spring = SpringMotion {
            damping_ratio: 0.4,
            ..SpringMotion::default()
        };
        assert!(
            (1..500)
                .map(|millis| spring_value(spring, f64::from(millis) / 1000.0))
                .any(|value| value > 1.0)
        );
    }

    #[test]
    fn overdamped_spring_remains_finite() {
        let spring = SpringMotion {
            damping_ratio: 4.0,
            stiffness: 100_000.0,
            epsilon: 0.00001,
        };
        for millis in 0..1_000 {
            assert!(spring_value(spring, f64::from(millis) / 1000.0).is_finite());
        }
    }

    #[test]
    fn zero_duration_easing_snaps_to_end() {
        let timeline = MotionTimeline::new(
            AnimationMotion::Easing(EasingMotion {
                duration_ms: 0,
                curve: AnimationCurve::Linear,
            }),
            Duration::ZERO,
        );
        assert_eq!(timeline.value_at(Duration::ZERO), 1.0);
        assert!(timeline.is_finished_at(Duration::ZERO));
    }
}
