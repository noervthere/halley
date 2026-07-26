use std::collections::HashMap;
use std::time::Duration;

use halley_config::{AnimationCurve, Animations, WindowOpenAnimationType};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle, Size};

const ELASTIC_PROXY_SIZE: f64 = 220.0;
const ELASTIC_MIN_SCALE: f64 = 0.24;
const MAX_OVERSHOOT_SCALE: f64 = 1.08;

#[derive(Clone, Copy, Debug)]
struct WindowOpenTimeline {
    started_at: Duration,
    duration: Duration,
    curve: AnimationCurve,
    animation_type: WindowOpenAnimationType,
}

impl WindowOpenTimeline {
    fn linear_progress_at(self, now: Duration) -> f64 {
        if now <= self.started_at {
            return 0.0;
        }
        if self.duration.is_zero() {
            return 1.0;
        }

        let elapsed = now.saturating_sub(self.started_at);
        (elapsed.as_secs_f64() / self.duration.as_secs_f64()).clamp(0.0, 1.0)
    }

    fn visual_at(self, now: Duration, bounds: Size<i32, Physical>) -> WindowOpenVisual {
        let progress = apply_curve(self.curve, self.linear_progress_at(now));
        match self.animation_type {
            WindowOpenAnimationType::CenterOut => WindowOpenVisual {
                scale: progress.clamp(0.0, MAX_OVERSHOOT_SCALE),
                alpha: 1.0,
            },
            WindowOpenAnimationType::Elastic => {
                let width = f64::from(bounds.w.max(1));
                let height = f64::from(bounds.h.max(1));
                let start_scale = (ELASTIC_PROXY_SIZE / width)
                    .min(ELASTIC_PROXY_SIZE / height)
                    .clamp(ELASTIC_MIN_SCALE, 1.0);
                let motion = progress.clamp(0.0, MAX_OVERSHOOT_SCALE);
                WindowOpenVisual {
                    scale: start_scale + (1.0 - start_scale) * motion,
                    alpha: motion.clamp(0.0, 1.0) as f32,
                }
            }
        }
    }

    fn is_finished_at(self, now: Duration) -> bool {
        now.saturating_sub(self.started_at) >= self.duration
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowOpenVisual {
    scale: f64,
    alpha: f32,
}

impl Default for WindowOpenVisual {
    fn default() -> Self {
        Self {
            scale: 1.0,
            alpha: 1.0,
        }
    }
}

impl WindowOpenVisual {
    pub fn transform_rect(
        self,
        rect: Rectangle<i32, Physical>,
        bounds: Rectangle<i32, Physical>,
    ) -> Rectangle<i32, Physical> {
        scale_rect_from_center(rect, bounds, self.scale)
    }

    pub fn alpha(self) -> f32 {
        self.alpha
    }
}

pub struct WindowOpenAnimations {
    config: Animations,
    active: HashMap<WlSurface, WindowOpenTimeline>,
}

impl WindowOpenAnimations {
    pub fn new(config: Animations) -> Self {
        Self {
            config,
            active: HashMap::new(),
        }
    }

    pub fn start(&mut self, surface: WlSurface, now: Duration) {
        let config = self.config.window_open;
        if !self.config.enabled || !config.enabled || config.duration_ms == 0 {
            return;
        }

        self.active.insert(
            surface,
            WindowOpenTimeline {
                started_at: now,
                duration: Duration::from_millis(config.duration_ms.into()),
                curve: config.curve,
                animation_type: config.animation_type,
            },
        );
    }

    /// Updates policy for future windows without disturbing animations
    /// already in flight.
    pub fn reload(&mut self, config: Animations) {
        self.config = config;
    }

    pub fn visual(
        &self,
        surface: &WlSurface,
        now: Duration,
        bounds: Size<i32, Physical>,
    ) -> Option<WindowOpenVisual> {
        self.active
            .get(surface)
            .map(|timeline| timeline.visual_at(now, bounds))
    }

    pub fn is_animating(&self, surface: &WlSurface, now: Duration) -> bool {
        self.active
            .get(surface)
            .is_some_and(|timeline| !timeline.is_finished_at(now))
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.active.remove(surface);
    }

    pub fn cleanup(&mut self, now: Duration) {
        self.active
            .retain(|_, timeline| !timeline.is_finished_at(now));
    }
}

fn apply_curve(curve: AnimationCurve, progress: f64) -> f64 {
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

fn scale_rect_from_center(
    rect: Rectangle<i32, Physical>,
    bounds: Rectangle<i32, Physical>,
    scale: f64,
) -> Rectangle<i32, Physical> {
    let scale = scale.clamp(0.0, MAX_OVERSHOOT_SCALE);
    if scale == 1.0 {
        return rect;
    }

    let center_x = bounds.loc.x as f64 + bounds.size.w as f64 / 2.0;
    let center_y = bounds.loc.y as f64 + bounds.size.h as f64 / 2.0;
    let left = center_x + (rect.loc.x as f64 - center_x) * scale;
    let top = center_y + (rect.loc.y as f64 - center_y) * scale;
    let right = center_x + (rect.loc.x as f64 + rect.size.w as f64 - center_x) * scale;
    let bottom = center_y + (rect.loc.y as f64 + rect.size.h as f64 - center_y) * scale;

    let left = left.round() as i32;
    let top = top.round() as i32;
    let right = right.round() as i32;
    let bottom = bottom.round() as i32;
    Rectangle::new(
        (left, top).into(),
        ((right - left).max(0), (bottom - top).max(0)).into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline(
        animation_type: WindowOpenAnimationType,
        curve: AnimationCurve,
    ) -> WindowOpenTimeline {
        WindowOpenTimeline {
            started_at: Duration::from_secs(1),
            duration: Duration::from_millis(300),
            curve,
            animation_type,
        }
    }

    #[test]
    fn timeline_uses_configured_duration() {
        let animation = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear);

        assert_eq!(animation.linear_progress_at(Duration::from_secs(1)), 0.0);
        assert_eq!(
            animation.linear_progress_at(Duration::from_millis(1150)),
            0.5
        );
        assert_eq!(
            animation.linear_progress_at(Duration::from_millis(1300)),
            1.0
        );
    }

    #[test]
    fn easing_curves_preserve_endpoints() {
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
    fn ease_out_curves_advance_faster_than_linear() {
        let linear = apply_curve(AnimationCurve::Linear, 0.5);
        assert!(apply_curve(AnimationCurve::EaseOutQuad, 0.5) > linear);
        assert!(apply_curve(AnimationCurve::EaseOutCubic, 0.5) > linear);
        assert!(apply_curve(AnimationCurve::EaseOutExpo, 0.5) > linear);
    }

    #[test]
    fn center_out_expands_from_final_center() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let timeline = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear);

        assert_eq!(
            timeline
                .visual_at(Duration::from_secs(1), bounds.size)
                .transform_rect(bounds, bounds),
            Rectangle::new((500, 350).into(), (0, 0).into())
        );
        assert_eq!(
            timeline
                .visual_at(Duration::from_millis(1150), bounds.size)
                .transform_rect(bounds, bounds),
            Rectangle::new((300, 200).into(), (400, 300).into())
        );
        assert_eq!(
            timeline
                .visual_at(Duration::from_millis(1300), bounds.size)
                .transform_rect(bounds, bounds),
            bounds
        );
    }

    #[test]
    fn surface_tree_rects_share_the_window_center() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let subsurface = Rectangle::new((110, 60).into(), (100, 50).into());
        let visual = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear)
            .visual_at(Duration::from_millis(1150), bounds.size);

        assert_eq!(
            visual.transform_rect(subsurface, bounds),
            Rectangle::new((305, 205).into(), (50, 25).into())
        );
    }

    #[test]
    fn elastic_starts_at_proxy_scale_and_fades_in() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let animation = timeline(
            WindowOpenAnimationType::Elastic,
            AnimationCurve::EaseOutBack,
        );

        let start = animation.visual_at(Duration::from_secs(1), bounds.size);
        assert_eq!(start.scale, 0.275);
        assert_eq!(start.alpha(), 0.0);
        assert_eq!(
            start.transform_rect(bounds, bounds),
            Rectangle::new((390, 268).into(), (220, 165).into())
        );
    }

    #[test]
    fn elastic_overshoots_before_settling() {
        let bounds = Rectangle::new((0, 0).into(), (800, 600).into());
        let animation = timeline(
            WindowOpenAnimationType::Elastic,
            AnimationCurve::EaseOutBack,
        );

        let middle = animation.visual_at(Duration::from_millis(1150), bounds.size);
        assert!(middle.scale > 1.0);
        assert_eq!(middle.alpha(), 1.0);

        let end = animation.visual_at(Duration::from_millis(1300), bounds.size);
        assert_eq!(end, WindowOpenVisual::default());
        assert!(animation.is_finished_at(Duration::from_millis(1300)));
    }

    #[test]
    fn overshoot_does_not_finish_the_timeline_early() {
        let animation = timeline(
            WindowOpenAnimationType::Elastic,
            AnimationCurve::EaseOutBack,
        );
        let middle = Duration::from_millis(1150);

        assert!(animation.visual_at(middle, Size::from((800, 600))).scale > 1.0);
        assert!(!animation.is_finished_at(middle));
    }
}
