use std::collections::HashMap;
use std::time::Duration;

use halley_config::{AnimationMotion, Animations, WindowOpenAnimationType};
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
    motion_config: AnimationMotion,
    animation_type: WindowOpenAnimationType,
    geometry: Option<RectTransition>,
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
        WindowOpenVisual {
            scale,
            alpha,
            destination: self.geometry.map(|geometry| geometry.rect_at(now).round()),
        }
    }

    fn is_finished_at(self, now: Duration) -> bool {
        self.motion.is_finished_at(now)
            && self
                .geometry
                .is_none_or(|geometry| geometry.is_finished_at(now))
    }

    fn retarget(
        &mut self,
        now: Duration,
        current_bounds: Rectangle<i32, Physical>,
        target_bounds: Rectangle<i32, Physical>,
    ) {
        self.retarget_with_motion(now, current_bounds, target_bounds, self.motion_config);
    }

    fn retarget_with_motion(
        &mut self,
        now: Duration,
        current_bounds: Rectangle<i32, Physical>,
        target_bounds: Rectangle<i32, Physical>,
        motion: AnimationMotion,
    ) {
        let (current, velocity) = match self.geometry {
            Some(geometry) => (geometry.rect_at(now), geometry.velocity_at(now)),
            None => {
                let scale = self.scale_at(now, current_bounds);
                let scale_velocity = self.scale_velocity_at(now, current_bounds);
                (
                    VisualRect::scaled(current_bounds, scale),
                    VisualRect::scaled_velocity(current_bounds, scale_velocity),
                )
            }
        };
        self.geometry = Some(RectTransition::between(
            motion,
            now,
            current,
            VisualRect::from(target_bounds),
            velocity,
        ));
    }

    fn scale_at(self, now: Duration, bounds: Rectangle<i32, Physical>) -> f64 {
        let motion = self.motion.value_at(now).clamp(0.0, MAX_OVERSHOOT_SCALE);
        match self.animation_type {
            WindowOpenAnimationType::CenterOut => motion,
            WindowOpenAnimationType::Elastic => {
                let width = f64::from(bounds.size.w.max(1));
                let height = f64::from(bounds.size.h.max(1));
                let start = (ELASTIC_PROXY_SIZE / width)
                    .min(ELASTIC_PROXY_SIZE / height)
                    .clamp(ELASTIC_MIN_SCALE, ELASTIC_MAX_START_SCALE);
                start + (1.0 - start) * motion
            }
        }
    }

    fn scale_velocity_at(self, now: Duration, bounds: Rectangle<i32, Physical>) -> f64 {
        let progress = self.motion.value_at(now);
        if !(0.0..MAX_OVERSHOOT_SCALE).contains(&progress) {
            return 0.0;
        }
        let velocity = self.motion.velocity_at(now);
        match self.animation_type {
            WindowOpenAnimationType::CenterOut => velocity,
            WindowOpenAnimationType::Elastic => {
                let width = f64::from(bounds.size.w.max(1));
                let height = f64::from(bounds.size.h.max(1));
                let start = (ELASTIC_PROXY_SIZE / width)
                    .min(ELASTIC_PROXY_SIZE / height)
                    .clamp(ELASTIC_MIN_SCALE, ELASTIC_MAX_START_SCALE);
                (1.0 - start) * velocity
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VisualRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl VisualRect {
    fn scaled(bounds: Rectangle<i32, Physical>, scale: f64) -> Self {
        let bounds = Self::from(bounds);
        let center_x = bounds.x + bounds.width / 2.0;
        let center_y = bounds.y + bounds.height / 2.0;
        let width = bounds.width * scale;
        let height = bounds.height * scale;
        Self {
            x: center_x - width / 2.0,
            y: center_y - height / 2.0,
            width,
            height,
        }
    }

    fn scaled_velocity(bounds: Rectangle<i32, Physical>, scale_velocity: f64) -> Self {
        let width = f64::from(bounds.size.w) * scale_velocity;
        let height = f64::from(bounds.size.h) * scale_velocity;
        Self {
            x: -width / 2.0,
            y: -height / 2.0,
            width,
            height,
        }
    }

    fn round(self) -> Rectangle<i32, Physical> {
        let left = self.x.round() as i32;
        let top = self.y.round() as i32;
        let right = (self.x + self.width).round() as i32;
        let bottom = (self.y + self.height).round() as i32;
        Rectangle::new(
            (left, top).into(),
            ((right - left).max(0), (bottom - top).max(0)).into(),
        )
    }
}

impl From<Rectangle<i32, Physical>> for VisualRect {
    fn from(rect: Rectangle<i32, Physical>) -> Self {
        Self {
            x: f64::from(rect.loc.x),
            y: f64::from(rect.loc.y),
            width: f64::from(rect.size.w),
            height: f64::from(rect.size.h),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RectTransition {
    x: MotionTimeline,
    y: MotionTimeline,
    width: MotionTimeline,
    height: MotionTimeline,
}

impl RectTransition {
    fn between(
        motion: AnimationMotion,
        now: Duration,
        start: VisualRect,
        target: VisualRect,
        velocity: VisualRect,
    ) -> Self {
        Self {
            x: MotionTimeline::between(motion, now, start.x, target.x, velocity.x),
            y: MotionTimeline::between(motion, now, start.y, target.y, velocity.y),
            width: MotionTimeline::between(motion, now, start.width, target.width, velocity.width),
            height: MotionTimeline::between(
                motion,
                now,
                start.height,
                target.height,
                velocity.height,
            ),
        }
    }

    fn rect_at(self, now: Duration) -> VisualRect {
        VisualRect {
            x: self.x.value_at(now),
            y: self.y.value_at(now),
            width: self.width.value_at(now),
            height: self.height.value_at(now),
        }
    }

    fn velocity_at(self, now: Duration) -> VisualRect {
        VisualRect {
            x: self.x.velocity_at(now),
            y: self.y.velocity_at(now),
            width: self.width.velocity_at(now),
            height: self.height.velocity_at(now),
        }
    }

    fn is_finished_at(self, now: Duration) -> bool {
        self.x.is_finished_at(now)
            && self.y.is_finished_at(now)
            && self.width.is_finished_at(now)
            && self.height.is_finished_at(now)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowOpenVisual {
    scale: f64,
    alpha: f32,
    destination: Option<Rectangle<i32, Physical>>,
}

impl Default for WindowOpenVisual {
    fn default() -> Self {
        Self {
            scale: 1.0,
            alpha: 1.0,
            destination: None,
        }
    }
}

impl WindowOpenVisual {
    pub fn transform_rect(
        self,
        rect: Rectangle<i32, Physical>,
        bounds: Rectangle<i32, Physical>,
    ) -> Rectangle<i32, Physical> {
        self.destination
            .map(|destination| map_rect(rect, bounds, destination))
            .unwrap_or_else(|| scale_rect_from_center(rect, bounds, self.scale))
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
            motion_config: config.motion,
            animation_type: config.animation_type,
            geometry: None,
        });
        true
    }

    pub fn retarget(
        &mut self,
        surface: &WlSurface,
        now: Duration,
        current_bounds: Rectangle<i32, Physical>,
        target_bounds: Rectangle<i32, Physical>,
    ) -> bool {
        let Some(timeline) = self.active.get_mut(surface) else {
            return false;
        };
        timeline.retarget(now, current_bounds, target_bounds);
        true
    }

    pub fn retarget_for_fullscreen(
        &mut self,
        surface: &WlSurface,
        now: Duration,
        current_bounds: Rectangle<i32, Physical>,
        target_bounds: Rectangle<i32, Physical>,
    ) -> bool {
        let Some(timeline) = self.active.get_mut(surface) else {
            return false;
        };
        let motion = if self.config.enabled && self.config.fullscreen.enabled {
            self.config.fullscreen.motion
        } else {
            timeline.motion_config
        };
        timeline.retarget_with_motion(now, current_bounds, target_bounds, motion);
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

    pub fn cleanup(&mut self, now: Duration) {
        self.active
            .retain(|_, timeline| !timeline.is_finished_at(now));
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

fn map_rect(
    rect: Rectangle<i32, Physical>,
    source: Rectangle<i32, Physical>,
    destination: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    let source_width = f64::from(source.size.w.max(1));
    let source_height = f64::from(source.size.h.max(1));
    let scale_x = f64::from(destination.size.w) / source_width;
    let scale_y = f64::from(destination.size.h) / source_height;
    let left = f64::from(destination.loc.x) + f64::from(rect.loc.x - source.loc.x) * scale_x;
    let top = f64::from(destination.loc.y) + f64::from(rect.loc.y - source.loc.y) * scale_y;
    let right =
        f64::from(destination.loc.x) + f64::from(rect.loc.x + rect.size.w - source.loc.x) * scale_x;
    let bottom =
        f64::from(destination.loc.y) + f64::from(rect.loc.y + rect.size.h - source.loc.y) * scale_y;
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
            motion_config,
            animation_type,
            geometry: None,
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

    #[test]
    fn geometry_retarget_preserves_the_current_presentation() {
        let current_bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let now = Duration::from_millis(1150);
        let mut animation = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear);
        let before = animation
            .visual_at(now, current_bounds)
            .transform_rect(current_bounds, current_bounds);

        animation.retarget(now, current_bounds, target);
        let after = animation
            .visual_at(now, target)
            .transform_rect(target, target);

        assert_eq!(after, before);
        assert_eq!(
            animation
                .visual_at(Duration::from_millis(1450), target)
                .transform_rect(target, target),
            target
        );
    }

    #[test]
    fn repeated_retarget_starts_from_the_live_intermediate_rect() {
        let windowed = Rectangle::new((100, 50).into(), (800, 600).into());
        let fullscreen = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let restored = Rectangle::new((200, 100).into(), (900, 700).into());
        let mut animation = timeline(WindowOpenAnimationType::CenterOut, AnimationCurve::Linear);

        animation.retarget(Duration::from_millis(1100), windowed, fullscreen);
        let now = Duration::from_millis(1200);
        let before = animation
            .visual_at(now, fullscreen)
            .transform_rect(fullscreen, fullscreen);
        animation.retarget(now, fullscreen, restored);
        let after = animation
            .visual_at(now, restored)
            .transform_rect(restored, restored);

        assert_eq!(after, before);
    }

    #[test]
    fn retarget_keeps_the_original_alpha_timeline() {
        let windowed = rect(800, 600);
        let fullscreen = rect(1920, 1080);
        let now = Duration::from_millis(1150);
        let mut animation = timeline(WindowOpenAnimationType::Elastic, AnimationCurve::Linear);
        let alpha = animation.visual_at(now, windowed).alpha();

        animation.retarget(now, windowed, fullscreen);

        assert_eq!(animation.visual_at(now, fullscreen).alpha(), alpha);
    }

    #[test]
    fn fullscreen_retarget_can_use_fullscreen_motion_without_restarting_alpha() {
        let windowed = rect(800, 600);
        let fullscreen = rect(1920, 1080);
        let started = Duration::from_secs(1);
        let mut animation = timeline(WindowOpenAnimationType::Elastic, AnimationCurve::Linear);
        let alpha = animation.visual_at(started, windowed).alpha();
        let fullscreen_motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 100,
            curve: AnimationCurve::Linear,
        });

        animation.retarget_with_motion(started, windowed, fullscreen, fullscreen_motion);

        assert_eq!(animation.visual_at(started, fullscreen).alpha(), alpha);
        assert_eq!(
            animation
                .visual_at(started + Duration::from_millis(100), fullscreen)
                .transform_rect(fullscreen, fullscreen),
            fullscreen
        );
    }
}
