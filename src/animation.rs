use std::collections::HashMap;
use std::time::Duration;

use halley_config::{Animations, WindowOpenAnimationType};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle};

const ELASTIC_PROXY_SIZE: f64 = 220.0;
const ELASTIC_MIN_SCALE: f64 = 0.24;
const ELASTIC_MAX_START_SCALE: f64 = 0.66;
const MAX_OVERSHOOT_SCALE: f64 = 1.08;

mod motion;

pub(crate) use motion::MotionTimeline;

#[derive(Clone, Copy, Debug)]
struct WindowOpenTimeline {
    motion: MotionTimeline,
    animation_type: WindowOpenAnimationType,
}

impl WindowOpenTimeline {
    fn visual_at(self, now: Duration, bounds: Rectangle<i32, Physical>) -> WindowOpenVisual {
        let progress = self.motion.value_at(now);
        let (scale, alpha) = match self.animation_type {
            WindowOpenAnimationType::CenterOut => (progress.clamp(0.0, MAX_OVERSHOOT_SCALE), 1.0),
            WindowOpenAnimationType::Elastic => {
                let width = f64::from(bounds.size.w.max(1));
                let height = f64::from(bounds.size.h.max(1));
                let start_scale = (ELASTIC_PROXY_SIZE / width)
                    .min(ELASTIC_PROXY_SIZE / height)
                    .clamp(ELASTIC_MIN_SCALE, ELASTIC_MAX_START_SCALE);
                let motion = progress.clamp(0.0, MAX_OVERSHOOT_SCALE);
                (
                    start_scale + (1.0 - start_scale) * motion,
                    motion.clamp(0.0, 1.0) as f32,
                )
            }
        };
        WindowOpenVisual { scale, alpha }
    }

    fn is_finished_at(self, now: Duration) -> bool {
        self.motion.is_finished_at(now)
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

    pub fn start(&mut self, surface: WlSurface, now: Duration) -> bool {
        let config = self.config.window_open;
        if !self.config.enabled || !config.enabled {
            return false;
        }

        let std::collections::hash_map::Entry::Vacant(entry) = self.active.entry(surface) else {
            return false;
        };
        entry.insert(WindowOpenTimeline {
            motion: MotionTimeline::new(config.motion, now),
            animation_type: config.animation_type,
        });
        true
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
        bounds: Rectangle<i32, Physical>,
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

    pub fn cleanup(&mut self, now: Duration) -> Vec<WlSurface> {
        let finished = self
            .active
            .iter()
            .filter(|(_, timeline)| timeline.is_finished_at(now))
            .map(|(surface, _)| surface.clone())
            .collect::<Vec<_>>();
        for surface in &finished {
            self.active.remove(surface);
        }
        finished
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
    use halley_config::{AnimationCurve, AnimationMotion, EasingMotion};

    fn timeline(
        animation_type: WindowOpenAnimationType,
        curve: AnimationCurve,
    ) -> WindowOpenTimeline {
        let motion_config = AnimationMotion::Easing(EasingMotion {
            duration_ms: 300,
            curve,
        });
        WindowOpenTimeline {
            motion: MotionTimeline::new(motion_config, Duration::from_secs(1)),
            animation_type,
        }
    }

    fn rect(width: i32, height: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((0, 0).into(), (width, height).into())
    }

    #[test]
    fn timeline_uses_configured_duration() {
        let animation = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear);

        assert_eq!(animation.motion.value_at(Duration::from_secs(1)), 0.0);
        assert_eq!(animation.motion.value_at(Duration::from_millis(1150)), 0.5);
        assert_eq!(animation.motion.value_at(Duration::from_millis(1300)), 1.0);
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
            assert_eq!(motion::apply_curve(curve, 0.0), 0.0);
            assert_eq!(motion::apply_curve(curve, 1.0), 1.0);
        }
    }

    #[test]
    fn ease_out_curves_advance_faster_than_linear() {
        let linear = motion::apply_curve(AnimationCurve::Linear, 0.5);
        assert!(motion::apply_curve(AnimationCurve::EaseOutQuad, 0.5) > linear);
        assert!(motion::apply_curve(AnimationCurve::EaseOutCubic, 0.5) > linear);
        assert!(motion::apply_curve(AnimationCurve::EaseOutExpo, 0.5) > linear);
    }

    #[test]
    fn center_out_expands_from_final_center() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let timeline = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear);

        assert_eq!(
            timeline
                .visual_at(Duration::from_secs(1), bounds)
                .transform_rect(bounds, bounds),
            Rectangle::new((500, 350).into(), (0, 0).into())
        );
        assert_eq!(
            timeline
                .visual_at(Duration::from_millis(1150), bounds)
                .transform_rect(bounds, bounds),
            Rectangle::new((300, 200).into(), (400, 300).into())
        );
        assert_eq!(
            timeline
                .visual_at(Duration::from_millis(1300), bounds)
                .transform_rect(bounds, bounds),
            bounds
        );
    }

    #[test]
    fn surface_tree_rects_share_the_window_center() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let subsurface = Rectangle::new((110, 60).into(), (100, 50).into());
        let visual = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear)
            .visual_at(Duration::from_millis(1150), bounds);

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

        let start = animation.visual_at(Duration::from_secs(1), bounds);
        assert_eq!(start.scale, 0.275);
        assert_eq!(start.alpha(), 0.0);
        assert_eq!(
            start.transform_rect(bounds, bounds),
            Rectangle::new((390, 268).into(), (220, 165).into())
        );
    }

    #[test]
    fn elastic_small_windows_still_have_visible_scale_motion() {
        let animation = timeline(
            WindowOpenAnimationType::Elastic,
            AnimationCurve::EaseOutBack,
        );

        let start = animation.visual_at(Duration::from_secs(1), rect(150, 100));

        assert_eq!(start.scale, ELASTIC_MAX_START_SCALE);
        assert_eq!(start.alpha(), 0.0);
    }

    #[test]
    fn active_opening_uses_updated_final_bounds_without_restarting() {
        let animation = timeline(WindowOpenAnimationType::Elastic, AnimationCurve::Linear);
        let now = Duration::from_millis(1150);

        let windowed = animation.visual_at(now, rect(800, 600));
        let fullscreen = animation.visual_at(now, rect(1920, 1080));

        assert_eq!(animation.motion.value_at(now), 0.5);
        assert_eq!(windowed.alpha(), fullscreen.alpha());
        assert!(fullscreen.scale < windowed.scale);
        assert_eq!(animation.motion.value_at(Duration::from_millis(1300)), 1.0);
    }

    #[test]
    fn elastic_overshoots_before_settling() {
        let bounds = Rectangle::new((0, 0).into(), (800, 600).into());
        let animation = timeline(
            WindowOpenAnimationType::Elastic,
            AnimationCurve::EaseOutBack,
        );

        let middle = animation.visual_at(Duration::from_millis(1150), bounds);
        assert!(middle.scale > 1.0);
        assert_eq!(middle.alpha(), 1.0);

        let end = animation.visual_at(Duration::from_millis(1300), bounds);
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

        assert!(animation.visual_at(middle, rect(800, 600)).scale > 1.0);
        assert!(!animation.is_finished_at(middle));
    }
}
