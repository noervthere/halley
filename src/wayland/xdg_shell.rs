use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::camera::OutputCameras;

use super::WaylandState;

/// A new toplevel role was created - stash it as unmapped, nothing shown
/// yet. Deliberately just this: no spawn-rule resolution, no monitor/
/// cluster placement, no reveal animation. Old halley fused exactly this
/// kind of window-manager policy into `XdgShellHandler::new_toplevel`
/// itself; that policy, if and when it exists, belongs downstream of
/// mapping, not inside protocol handling.
pub fn new_toplevel(wayland: &mut WaylandState, surface: ToplevelSurface) {
    let wl_surface = surface.wl_surface().clone();
    let window = Window::new_wayland_window(surface);
    wayland.windows.register_xdg(wl_surface, window);
}

/// Removes a destroyed toplevel from whichever lifecycle state owns it.
///
/// This must explicitly unmap a mapped window: Smithay calls this handler
/// while the underlying `wl_surface` may still be alive, so waiting for
/// `Space::refresh()` can leave Halley's compositor-drawn border in the next
/// frame. Focus clears to `None`; there is no fallback-refocus policy yet.
pub fn toplevel_destroyed(wayland: &mut WaylandState, surface: &ToplevelSurface) {
    if let Some(window) = wayland.windows.destroy_xdg(surface.wl_surface()) {
        wayland.space.unmap_elem(&window);
    }
    if wayland.focused_window.as_ref() == Some(surface.wl_surface()) {
        wayland.focused_window = None;
    }
}

/// The commit-path half of the unmapped -> mapped transition: sends the
/// initial configure for a still-unmapped toplevel, then promotes it into
/// `space` once it has actually attached a buffer. This is the one
/// deliberate deviation from smallvil, which maps a toplevel into `Space`
/// immediately in `new_toplevel` and relies on Smithay's render-element
/// code silently skipping bufferless surfaces - here "mapped" means
/// "actually visible," not "present in `Space` but incidentally invisible."
pub fn handle_commit(
    wayland: &mut WaylandState,
    cameras: &OutputCameras,
    surface: &WlSurface,
) -> Option<WlSurface> {
    let id = wayland.windows.id_for_xdg(surface)?;
    let toplevel = wayland.windows.window(id)?.toplevel()?;

    if !toplevel.is_initial_configure_sent() {
        toplevel.with_pending_state(super::decoration::apply_tiled_hint);
        toplevel.send_configure();
        return None;
    }

    let has_buffer =
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
    if has_buffer {
        wayland.windows.request_map(id);
        wayland.windows.set_surface_ready(id, true);
        let admission = wayland.windows.admit(id)?;
        eventline::debug!(
            "window lifecycle: admitted {:?} generation={} first={}",
            admission.id,
            admission.generation,
            admission.first
        );
        let window = admission.window;
        wayland.windows.set_input_ready(id, true);
        let output = super::focus::selected_output(wayland).cloned();
        let location = output
            .as_ref()
            .map(|output| centered_location(wayland, cameras, output, &window))
            .unwrap_or_else(|| Point::from((0, 0)));
        if let Some(output) = output.as_ref() {
            super::set_window_output(&window, output);
        }
        wayland.space.map_element(window.clone(), location, false);
        // New windows steal focus - matches most WMs' default behavior.
        // Also raises+activates via `focus_and_raise`, same as clicking a
        // window now does.
        crate::window::focus_and_raise(wayland, &window);
        return Some(surface.clone());
    }

    if let Some(window) = wayland.windows.withdraw(id) {
        wayland.space.unmap_elem(&window);
        if wayland.focused_window.as_ref() == Some(surface) {
            wayland.focused_window = None;
        }
    }
    None
}

/// Centers a newly-mapped window on the selected output's live camera.
/// Existing freeform windows stay where they are when the camera later moves.
pub(crate) fn centered_location(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    output: &Output,
    window: &Window,
) -> Point<i32, Logical> {
    let Some(output_geo) = wayland.space.output_geometry(output) else {
        return (0, 0).into();
    };
    let local_camera_center = cameras
        .view(&output.name())
        .map(|view| view.center)
        .unwrap_or_else(|| {
            Point::<f32, Physical>::from((
                output_geo.size.w as f32 / 2.0,
                output_geo.size.h as f32 / 2.0,
            ))
        });
    let center = crate::camera::global_center(local_camera_center, output_geo);
    center_window(center, window.geometry().size)
}

fn center_window(
    center: Point<f32, Physical>,
    window_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    Point::from((
        (center.x - window_size.w as f32 / 2.0).round() as i32,
        (center.y - window_size.h as f32 / 2.0).round() as i32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_location_is_centered_on_global_camera_position() {
        assert_eq!(
            center_window(Point::from((3620.0, 550.0)), Size::from((1000, 700)),),
            Point::from((3120, 200))
        );
    }
}
