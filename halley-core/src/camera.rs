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
}

#[cfg(test)]
mod tests {
    use super::*;

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
