use std::time::Duration;

use halley_config::{WindowCloseAnimation, WindowCloseAnimationType};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CloseVisual {
    pub(crate) scale: f64,
    pub(crate) alpha: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CloseTimeline {
    started_at: Duration,
    duration: Duration,
    animation_type: WindowCloseAnimationType,
    start_alpha: f32,
}

impl CloseTimeline {
    pub(crate) fn new(
        config: WindowCloseAnimation,
        started_at: Duration,
        start_alpha: f32,
    ) -> Self {
        Self {
            started_at,
            duration: Duration::from_millis(u64::from(config.duration_ms)),
            animation_type: config.animation_type,
            start_alpha: start_alpha.clamp(0.0, 1.0),
        }
    }

    pub(crate) fn visual_at(self, now: Duration) -> CloseVisual {
        let progress = if self.duration.is_zero() {
            1.0
        } else {
            now.saturating_sub(self.started_at).as_secs_f64() / self.duration.as_secs_f64()
        }
        .clamp(0.0, 1.0);
        let progress = ease_in_out_cubic(progress);
        match self.animation_type {
            WindowCloseAnimationType::Shrink => CloseVisual {
                scale: 1.0 - progress,
                alpha: self.start_alpha,
            },
            WindowCloseAnimationType::Fade => CloseVisual {
                scale: 1.0,
                alpha: self.start_alpha * (1.0 - progress) as f32,
            },
        }
    }

    pub(crate) fn is_finished_at(self, now: Duration) -> bool {
        now.saturating_sub(self.started_at) >= self.duration
    }
}

fn ease_in_out_cubic(progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress.powi(3)
    } else {
        1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(animation_type: WindowCloseAnimationType) -> WindowCloseAnimation {
        WindowCloseAnimation {
            enabled: true,
            animation_type,
            duration_ms: 200,
        }
    }

    #[test]
    fn shrink_collapses_to_the_center_without_fading() {
        let timeline = CloseTimeline::new(
            config(WindowCloseAnimationType::Shrink),
            Duration::from_secs(1),
            0.7,
        );

        assert_eq!(
            timeline.visual_at(Duration::from_secs(1)),
            CloseVisual {
                scale: 1.0,
                alpha: 0.7,
            }
        );
        assert_eq!(
            timeline.visual_at(Duration::from_millis(1100)),
            CloseVisual {
                scale: 0.5,
                alpha: 0.7,
            }
        );
        assert_eq!(
            timeline.visual_at(Duration::from_millis(1200)),
            CloseVisual {
                scale: 0.0,
                alpha: 0.7,
            }
        );
    }

    #[test]
    fn fade_preserves_scale_and_reduces_live_alpha() {
        let timeline = CloseTimeline::new(
            config(WindowCloseAnimationType::Fade),
            Duration::from_secs(1),
            0.6,
        );

        assert_eq!(
            timeline.visual_at(Duration::from_millis(1100)),
            CloseVisual {
                scale: 1.0,
                alpha: 0.3,
            }
        );
        assert_eq!(
            timeline.visual_at(Duration::from_millis(1200)),
            CloseVisual {
                scale: 1.0,
                alpha: 0.0,
            }
        );
    }

    #[test]
    fn zero_duration_finishes_immediately() {
        let mut config = config(WindowCloseAnimationType::Shrink);
        config.duration_ms = 0;
        let now = Duration::from_secs(1);
        let timeline = CloseTimeline::new(config, now, 1.0);

        assert!(timeline.is_finished_at(now));
        assert_eq!(timeline.visual_at(now).scale, 0.0);
    }
}
