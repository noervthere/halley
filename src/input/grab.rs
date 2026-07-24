use halley_core::camera::Camera;
use halley_core::field::Vec2;
use smithay::desktop::{Space, Window};
use smithay::utils::{Physical, Point, Size};

/// What's currently being dragged with the left mouse button held, if
/// anything - `None` the rest of the time. Lives on `App`/`TtyApp` next to
/// `pointer`/`camera`, mirroring how each of those was added for one
/// concrete reason.
pub enum Grab {
    None,
    /// `offset` is `window_pos - world_cursor_pos`, captured at grab-start
    /// (both already in world/`Space` coordinates) - every motion event
    /// recomputes the window's position as `world_cursor_pos + offset`, so
    /// the window tracks the cursor exactly without drifting.
    MoveWindow { window: Window, offset: Vec2 },
    /// Left-click-drag on empty desktop. No extra state needed - panning
    /// just feeds `Camera::pan_target` directly each motion event.
    Pan,
}

/// Converts a screen-space (physical-pixel) position into world (`Space`)
/// coordinates, given the camera's live center/zoom state and the output's
/// physical size - the exact inverse of `backend::camera_rect`'s transform.
/// Needed because the pointer's tracked position is always in screen
/// coordinates, but window positions and grab math need to be in world
/// coordinates now that panning is real.
pub fn screen_to_world(screen: (f64, f64), camera: &Camera, output_size: Size<i32, Physical>) -> Vec2 {
    let output_center_x = output_size.w as f32 / 2.0;
    let output_center_y = output_size.h as f32 / 2.0;
    let scale = crate::input::zoom::scale(camera);
    Vec2 {
        x: camera.center.x + (screen.0 as f32 - output_center_x) / scale,
        y: camera.center.y + (screen.1 as f32 - output_center_y) / scale,
    }
}

/// Converts a screen-space motion delta into a world-space delta - used
/// while panning, where only the delta matters (not an absolute position).
/// Scaled by the same factor `screen_to_world` uses, so panning speed stays
/// 1:1 with cursor motion regardless of the current zoom level (matches old
/// halley's own reason for scaling pan deltas by view-size/output-size).
pub fn screen_delta_to_world(dx: f64, dy: f64, camera: &Camera) -> Vec2 {
    let scale = crate::input::zoom::scale(camera);
    Vec2 {
        x: dx as f32 / scale,
        y: dy as f32 / scale,
    }
}

/// Hit-tests a world-space point against every mapped window, returning the
/// front-most match - a thin wrapper over `Space::element_under` (Smithay's
/// own front-to-back element order already matches expectations here;
/// there's no competing stacking/cluster policy to arbitrate, unlike old
/// halley's custom multi-tier hit-test).
pub fn window_under(space: &Space<Window>, world: Vec2) -> Option<(Window, Point<i32, smithay::utils::Logical>)> {
    space
        .element_under((world.x as f64, world.y as f64))
        .map(|(window, loc)| (window.clone(), loc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera_at_rest() -> Camera {
        Camera::new(Vec2 { x: 640.0, y: 400.0 }, Vec2 { x: 1280.0, y: 800.0 })
    }

    #[test]
    fn screen_to_world_is_identity_at_rest() {
        let camera = camera_at_rest();
        let output_size = Size::<i32, Physical>::from((1280, 800));
        // Output center maps to camera center (world origin here).
        assert_eq!(screen_to_world((640.0, 400.0), &camera, output_size), Vec2 { x: 640.0, y: 400.0 });
        // A screen point 100px right/50px down of center is the same world
        // offset when nothing is panned or zoomed.
        let world = screen_to_world((740.0, 450.0), &camera, output_size);
        assert_eq!(world, Vec2 { x: 740.0, y: 450.0 });
    }

    #[test]
    fn screen_to_world_accounts_for_pan() {
        let mut camera = camera_at_rest();
        camera.center = Vec2 { x: 740.0, y: 400.0 };
        let output_size = Size::<i32, Physical>::from((1280, 800));
        // Output center now maps to the panned camera center, not the
        // original world origin.
        assert_eq!(screen_to_world((640.0, 400.0), &camera, output_size), Vec2 { x: 740.0, y: 400.0 });
    }

    #[test]
    fn screen_to_world_accounts_for_zoom() {
        let mut camera = camera_at_rest();
        // Zoomed out to half scale (view_size double base_size).
        camera.view_size = Vec2 { x: 2560.0, y: 1600.0 };
        let output_size = Size::<i32, Physical>::from((1280, 800));
        // At 0.5x scale, a 100px screen offset is a 200px world offset.
        let world = screen_to_world((740.0, 400.0), &camera, output_size);
        assert_eq!(world, Vec2 { x: 840.0, y: 400.0 });
    }

    #[test]
    fn screen_delta_to_world_scales_by_zoom() {
        let mut camera = camera_at_rest();
        assert_eq!(screen_delta_to_world(100.0, 50.0, &camera), Vec2 { x: 100.0, y: 50.0 });

        camera.view_size = Vec2 { x: 2560.0, y: 1600.0 };
        assert_eq!(screen_delta_to_world(100.0, 50.0, &camera), Vec2 { x: 200.0, y: 100.0 });
    }
}
