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

    /// Caps how much quick repeated zooming can pile up (the accel ceiling).
    pub const ZOOM_VEL_STACK_MAX: f32 = 6.0;
    /// Zoom velocity (log-space) below which the glide snaps to rest.
    pub const ZOOM_VEL_EPS: f32 = 0.02;
    /// Pan velocity below which a coasting flick snaps to rest.
    pub const PAN_VEL_EPS: f32 = 4.0;
    /// Caps how fast a flick can launch the viewport (world units/sec).
    pub const PAN_VEL_MAX: f32 = 12000.0;

    /// Offset the pan target by `delta` (was `pan_camera_target`, minus the
    /// `request_maintenance()` scheduling call, which is a `wl`-side concern).
    pub fn pan_target(&mut self, delta: Vec2) {
        self.target_center.x += delta.x;
        self.target_center.y += delta.y;
    }

    /// Set the target view size, clamped to the zoom bounds (was
    /// `set_camera_target_view_size`).
    pub fn set_target_view_size(&mut self, size: Vec2, zoom_min: f32, zoom_max: f32) {
        self.target_view_size = self.clamp_view_size(size, zoom_min, zoom_max);
    }

    /// Snap both targets to the current live values - used when an external
    /// animation (e.g. a deliberate pan-to) is about to take over, so the
    /// easing doesn't fight it (was `snap_camera_targets_to_live`).
    pub fn snap_targets_to_live(&mut self) {
        self.target_center = self.center;
        self.target_view_size = self.view_size;
    }

    /// Seed inertial pan with a release velocity (world units/sec), clamped
    /// to `PAN_VEL_MAX`. A subsequent `tick` coasts the target and decays
    /// the velocity with friction.
    pub fn fling_pan(&mut self, vel: Vec2) {
        let speed = vel.x.hypot(vel.y);
        self.pan_vel = if speed > Self::PAN_VEL_MAX {
            Vec2 {
                x: vel.x / speed * Self::PAN_VEL_MAX,
                y: vel.y / speed * Self::PAN_VEL_MAX,
            }
        } else {
            vel
        };
    }

    /// Instant (non-inertial) zoom: jump the target size directly by
    /// `steps` multiplicative zoom-steps, no velocity involved (the
    /// non-smooth branch of the old `zoom_by_steps`).
    pub fn zoom_instant_by_steps(&mut self, steps: f32, zoom_step_cfg: f32, zoom_min: f32, zoom_max: f32) {
        let steps = steps.clamp(-4.0, 4.0);
        if steps.abs() < f32::EPSILON {
            return;
        }
        let factor = zoom_step(zoom_step_cfg).powf(steps);
        let new_size = Vec2 {
            x: self.target_view_size.x / factor,
            y: self.target_view_size.y / factor,
        };
        self.set_target_view_size(new_size, zoom_min, zoom_max);
    }

    /// Inertial/lens zoom: inject velocity in log space (the smooth branch
    /// of the old `zoom_by_steps`). Repeating in the same direction stacks
    /// velocity (an accelerating ramp); the opposite direction bleeds it off
    /// or reverses.
    pub fn zoom_inject_velocity(&mut self, steps: f32, zoom_step_cfg: f32, smooth_rate_cfg: f32) {
        let steps = steps.clamp(-4.0, 4.0);
        if steps.abs() < f32::EPSILON {
            return;
        }
        let friction = zoom_smooth_rate(smooth_rate_cfg);
        let step_impulse = friction * zoom_step(zoom_step_cfg).ln();
        let cap = step_impulse * Self::ZOOM_VEL_STACK_MAX;
        // zoom-in (+steps) shrinks the view -> negative log velocity.
        self.zoom_log_vel = (self.zoom_log_vel - steps * step_impulse).clamp(-cap, cap);
    }

    /// Reset the zoom target back to `base_size` and clear zoom velocity
    /// (was the guard-free tail of `reset_zoom`). Setting directly to
    /// `base_size` rather than going through `set_target_view_size` is
    /// deliberate: `base_size` is always within the zoom bounds by
    /// construction (`zoom_scale_bounds` guarantees min <= 1.0 <= max), so
    /// the clamp would always be a no-op anyway.
    pub fn reset_zoom_target(&mut self) {
        self.zoom_log_vel = 0.0;
        self.target_view_size = self.base_size;
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
    fn pan_target_offsets_target_center_only() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.pan_target(Vec2 { x: 10.0, y: -5.0 });
        assert_eq!(cam.target_center, Vec2 { x: 10.0, y: -5.0 });
        // Live center is untouched - that's tick()'s job.
        assert_eq!(cam.center, Vec2 { x: 0.0, y: 0.0 });
    }

    #[test]
    fn snap_targets_to_live_matches_current_live_values() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.target_center = Vec2 { x: 999.0, y: 999.0 };
        cam.target_view_size = Vec2 { x: 1.0, y: 1.0 };
        cam.snap_targets_to_live();
        assert_eq!(cam.target_center, cam.center);
        assert_eq!(cam.target_view_size, cam.view_size);
    }

    #[test]
    fn fling_pan_caps_speed_at_max() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.fling_pan(Vec2 {
            x: Camera::PAN_VEL_MAX * 10.0,
            y: 0.0,
        });
        assert_eq!(cam.pan_vel.x, Camera::PAN_VEL_MAX);

        cam.fling_pan(Vec2 { x: 50.0, y: 0.0 });
        assert_eq!(cam.pan_vel, Vec2 { x: 50.0, y: 0.0 });
    }

    #[test]
    fn zoom_instant_by_steps_shrinks_target_when_zooming_in() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.zoom_instant_by_steps(1.0, 1.1, 0.1, 4.0);
        assert!(cam.target_view_size.x < 800.0);
        assert!(cam.target_view_size.y < 600.0);
    }

    #[test]
    fn zoom_instant_by_steps_ignores_near_zero_steps() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.zoom_instant_by_steps(0.0, 1.1, 0.1, 4.0);
        assert_eq!(cam.target_view_size, Vec2 { x: 800.0, y: 600.0 });
    }

    #[test]
    fn zoom_inject_velocity_stacks_in_same_direction() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.zoom_inject_velocity(1.0, 1.1, 8.0);
        let after_one = cam.zoom_log_vel;
        assert_ne!(after_one, 0.0);

        cam.zoom_inject_velocity(1.0, 1.1, 8.0);
        // Repeating in the same direction stacks (grows) the velocity magnitude.
        assert!(cam.zoom_log_vel.abs() > after_one.abs());
    }

    #[test]
    fn reset_zoom_target_returns_to_base_size_and_clears_velocity() {
        let mut cam = Camera::new(Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 800.0, y: 600.0 });
        cam.target_view_size = Vec2 { x: 1234.0, y: 1234.0 };
        cam.zoom_log_vel = 42.0;
        cam.reset_zoom_target();
        assert_eq!(cam.target_view_size, cam.base_size);
        assert_eq!(cam.zoom_log_vel, 0.0);
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
