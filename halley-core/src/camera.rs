use crate::field::Vec2;
use crate::viewport::Viewport;

/// Consolidated camera dynamics: live position/extent, the target being
/// eased toward, and the two inertia terms (pan velocity, zoom velocity in
/// log space). This didn't exist as a single type in the old design - the
/// equivalent state was scattered across `Halley`/`MonitorSpace`/
/// `RuntimeTuning` in `halley-wl` as five separate fields
/// (`viewport.center`/`viewport.size`/`zoom_ref_size`/`camera_target_center`/
/// `camera_target_view_size`) plus `pan_vel`/`zoom_log_vel`.
///
/// Naming note: `base_size` is the reference size at 1.0x zoom (what zoom
/// bounds are computed against - roughly the monitor's native output size,
/// not itself animated). `view_size` is the live, currently-rendered view
/// extent, which shrinks/grows as the camera zooms. Conflating these two was
/// the actual source of confusion in the old code (it called the base one
/// `viewport.size` and the live one `zoom_ref_size`, with no type tying them
/// together).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Live center, in Field coordinates.
    pub center: Vec2,
    /// Live view extent, in Field coordinates.
    pub view_size: Vec2,
    /// Reference view extent at 1.0x zoom - not animated.
    pub base_size: Vec2,
    /// Center being eased toward.
    pub target_center: Vec2,
    /// View extent being eased toward.
    pub target_view_size: Vec2,
    /// Inertial pan velocity, in world units per second.
    pub pan_vel: Vec2,
    /// Inertial zoom velocity, in log(view-size) units per second.
    pub zoom_log_vel: f32,
}

impl Camera {
    pub fn new(center: Vec2, base_size: Vec2) -> Self {
        Self {
            center,
            view_size: base_size,
            base_size,
            target_center: center,
            target_view_size: base_size,
            pan_vel: Vec2 { x: 0.0, y: 0.0 },
            zoom_log_vel: 0.0,
        }
    }

    /// The live camera as a plain `Viewport`, for consumers that need that
    /// type (e.g. `Field::in_view`/rendering) rather than the full dynamics.
    pub fn viewport(&self) -> Viewport {
        Viewport::new(self.center, self.view_size)
    }

    /// Clamp a candidate view size to the zoom bounds, relative to this
    /// camera's `base_size` (was `clamp_camera_view_size`, which reached
    /// into `st.model.viewport.size` for the same purpose).
    pub fn clamp_view_size(&self, size: Vec2, zoom_min: f32, zoom_max: f32) -> Vec2 {
        let (min_zoom, max_zoom) = zoom_scale_bounds(zoom_min, zoom_max);
        Vec2 {
            x: size.x.clamp(self.base_size.x / max_zoom, self.base_size.x / min_zoom),
            y: size.y.clamp(self.base_size.y / max_zoom, self.base_size.y / min_zoom),
        }
    }
}

/// Clamp a configured zoom-per-step factor to a sane minimum (must be > 1.0
/// to mean anything as a multiplicative step).
pub fn zoom_step(step: f32) -> f32 {
    step.max(1.001)
}

/// Clamp configured zoom min/max into a sane, ordered range.
pub fn zoom_scale_bounds(zoom_min: f32, zoom_max: f32) -> (f32, f32) {
    let min = zoom_min.clamp(0.05, 1.0);
    let max = zoom_max.max(min).clamp(1.0, 16.0);
    (min, max)
}

/// Clamp a configured zoom smoothing rate to a sane range.
pub fn zoom_smooth_rate(rate: f32) -> f32 {
    rate.clamp(0.1, 120.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_step_has_a_floor() {
        assert_eq!(zoom_step(0.5), 1.001);
        assert_eq!(zoom_step(2.0), 2.0);
    }

    #[test]
    fn zoom_scale_bounds_orders_and_clamps() {
        // max below min gets pulled up to min.
        assert_eq!(zoom_scale_bounds(0.5, 0.1), (0.5, 1.0));
        // out-of-range values get clamped into the sane window.
        assert_eq!(zoom_scale_bounds(0.0, 100.0), (0.05, 16.0));
        // ordinary values pass through.
        assert_eq!(zoom_scale_bounds(0.5, 2.0), (0.5, 2.0));
    }

    #[test]
    fn zoom_smooth_rate_clamps_to_sane_range() {
        assert_eq!(zoom_smooth_rate(0.0), 0.1);
        assert_eq!(zoom_smooth_rate(1000.0), 120.0);
        assert_eq!(zoom_smooth_rate(10.0), 10.0);
    }

    #[test]
    fn clamp_view_size_respects_base_size_and_bounds() {
        let cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });

        // Zoomed in too far (size too small) clamps to base/max_zoom.
        let too_small = cam.clamp_view_size(Vec2 { x: 1.0, y: 1.0 }, 0.5, 2.0);
        assert_eq!(too_small, Vec2 { x: 400.0, y: 300.0 });

        // Zoomed out too far (size too large) clamps to base/min_zoom.
        let too_large = cam.clamp_view_size(Vec2 { x: 100_000.0, y: 100_000.0 }, 0.5, 2.0);
        assert_eq!(too_large, Vec2 { x: 1600.0, y: 1200.0 });

        // Within bounds passes through unchanged.
        let within = cam.clamp_view_size(Vec2 { x: 900.0, y: 700.0 }, 0.5, 2.0);
        assert_eq!(within, Vec2 { x: 900.0, y: 700.0 });
    }

    #[test]
    fn new_camera_starts_at_rest_with_targets_matching_live() {
        let center = Vec2 { x: 10.0, y: 20.0 };
        let base_size = Vec2 { x: 800.0, y: 600.0 };
        let cam = Camera::new(center, base_size);

        assert_eq!(cam.center, center);
        assert_eq!(cam.view_size, base_size);
        assert_eq!(cam.base_size, base_size);
        assert_eq!(cam.target_center, center);
        assert_eq!(cam.target_view_size, base_size);
        assert_eq!(cam.pan_vel, Vec2 { x: 0.0, y: 0.0 });
        assert_eq!(cam.zoom_log_vel, 0.0);
    }

    #[test]
    fn viewport_reflects_live_center_and_view_size() {
        let cam = Camera::new(Vec2 { x: 1.0, y: 2.0 }, Vec2 { x: 100.0, y: 50.0 });
        let vp = cam.viewport();
        assert_eq!(vp.center, Vec2 { x: 1.0, y: 2.0 });
        assert_eq!(vp.size, Vec2 { x: 100.0, y: 50.0 });
    }
}
