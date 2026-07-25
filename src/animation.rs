use std::collections::HashMap;
use std::time::Duration;

use halley_config::{AnimationCurve, Animations};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle};

#[derive(Clone, Copy, Debug)]
struct WindowOpenTimeline {
    started_at: Duration,
    duration: Duration,
    curve: AnimationCurve,
}

impl WindowOpenTimeline {
    fn progress_at(self, now: Duration) -> f64 {
        if now <= self.started_at {
            return 0.0;
        }
        if self.duration.is_zero() {
            return 1.0;
        }

        let elapsed = now.saturating_sub(self.started_at);
        let linear = (elapsed.as_secs_f64() / self.duration.as_secs_f64()).clamp(0.0, 1.0);
        apply_curve(self.curve, linear)
    }

    fn is_finished_at(self, now: Duration) -> bool {
        now.saturating_sub(self.started_at) >= self.duration
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
        if self.config.off || config.off || config.duration_ms == 0 {
            return;
        }

        self.active.insert(
            surface,
            WindowOpenTimeline {
                started_at: now,
                duration: Duration::from_millis(config.duration_ms.into()),
                curve: config.curve,
            },
        );
    }

    pub fn progress(&self, surface: &WlSurface, now: Duration) -> Option<f64> {
        self.active
            .get(surface)
            .map(|timeline| timeline.progress_at(now))
    }

    pub fn is_animating(&self, surface: &WlSurface, now: Duration) -> bool {
        self.progress(surface, now)
            .is_some_and(|progress| progress < 1.0)
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
    }
}

/// Scales `rect` around the center of the window's final `bounds`.
///
/// Every surface in the toplevel tree - subsurfaces and popups alike - uses
/// the same origin, so the whole window expands from its center as one
/// piece. This mirrors newm, where the open animation interpolates a single
/// per-view `box` from `(center, 0x0)` to the final rect and pywm renders
/// the entire surface tree scaled into it (`newm/view.py::_show_tiled` /
/// `_show_floating`, `newm/interpolation.py::ViewDownstreamInterpolation`).
pub fn window_open_rect(
    rect: Rectangle<i32, Physical>,
    bounds: Rectangle<i32, Physical>,
    progress: f64,
) -> Rectangle<i32, Physical> {
    let progress = progress.clamp(0.0, 1.0);
    if progress == 1.0 {
        return rect;
    }

    let center_x = bounds.loc.x as f64 + bounds.size.w as f64 / 2.0;
    let center_y = bounds.loc.y as f64 + bounds.size.h as f64 / 2.0;
    let left = center_x + (rect.loc.x as f64 - center_x) * progress;
    let top = center_y + (rect.loc.y as f64 - center_y) * progress;
    let right =
        center_x + (rect.loc.x as f64 + rect.size.w as f64 - center_x) * progress;
    let bottom =
        center_y + (rect.loc.y as f64 + rect.size.h as f64 - center_y) * progress;

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

    fn timeline(curve: AnimationCurve) -> WindowOpenTimeline {
        WindowOpenTimeline {
            started_at: Duration::from_secs(1),
            duration: Duration::from_millis(300),
            curve,
        }
    }

    #[test]
    fn linear_timeline_uses_newm_duration() {
        let animation = timeline(AnimationCurve::Linear);

        assert_eq!(animation.progress_at(Duration::from_secs(1)), 0.0);
        assert_eq!(
            animation.progress_at(Duration::from_millis(1150)),
            0.5
        );
        assert_eq!(animation.progress_at(Duration::from_millis(1300)), 1.0);
    }

    #[test]
    fn easing_curves_preserve_endpoints() {
        for curve in [
            AnimationCurve::Linear,
            AnimationCurve::EaseOutQuad,
            AnimationCurve::EaseOutCubic,
            AnimationCurve::EaseOutExpo,
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
    fn opening_rect_expands_from_final_center() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());

        assert_eq!(
            window_open_rect(bounds, bounds, 0.0),
            Rectangle::new((500, 350).into(), (0, 0).into())
        );
        assert_eq!(
            window_open_rect(bounds, bounds, 0.5),
            Rectangle::new((300, 200).into(), (400, 300).into())
        );
        assert_eq!(window_open_rect(bounds, bounds, 1.0), bounds);
    }

    #[test]
    fn surface_tree_rects_share_the_window_center() {
        let bounds = Rectangle::new((100, 50).into(), (800, 600).into());
        let subsurface = Rectangle::new((110, 60).into(), (100, 50).into());

        assert_eq!(
            window_open_rect(subsurface, bounds, 0.5),
            Rectangle::new((305, 205).into(), (50, 25).into())
        );
    }
}
