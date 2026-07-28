use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::camera::OutputCameras;
use crate::window::lifecycle::{MapTransition, Placement, UnmapTransition, WindowLifecycle};

use super::WaylandState;

pub enum CommitOutcome {
    Mapped(MapTransition),
    Unmapped(UnmapTransition),
}

/// Registers a new toplevel as unmapped. Placement, rules, focus, and reveal
/// policy run only after the client attaches a visible buffer.
pub fn new_toplevel(wayland: &mut WaylandState, surface: ToplevelSurface) {
    let wl_surface = surface.wl_surface().clone();
    let window = Window::new_wayland_window(surface);
    wayland.windows.register_xdg(wl_surface, window);
}

/// Removes a destroyed toplevel from lifecycle ownership. Session-level
/// retirement applies the shared scene, focus, and visual cleanup.
pub fn toplevel_destroyed(
    wayland: &mut WaylandState,
    surface: &ToplevelSurface,
) -> Option<UnmapTransition> {
    let key = WindowLifecycle::xdg_key(surface.wl_surface());
    wayland.windows.destroy(&key)
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
) -> Option<CommitOutcome> {
    let key = WindowLifecycle::xdg_key(surface);
    let window = wayland.windows.window(&key)?.clone();
    let toplevel = window.toplevel()?;

    let has_buffer =
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
    if wayland.windows.is_mapped(&key) {
        if has_buffer {
            return None;
        }
        let placement = wayland
            .space
            .element_location(&window)
            .map(|location| Placement {
                location,
                output: super::window_output_name(&window),
            });
        let transition = wayland.windows.unmap(&key, placement)?;
        eventline::debug!("window: XDG surface {} unmapped", surface.id());
        return Some(CommitOutcome::Unmapped(transition));
    }

    if wayland.windows.needs_configure(&key) {
        toplevel.with_pending_state(super::decoration::apply_tiled_hint);
        toplevel.send_configure();
        wayland.windows.mark_configured(&key);
        eventline::debug!("window: XDG surface {} configured", surface.id());
        return None;
    }

    if has_buffer {
        let transition = wayland.windows.begin_map(&key)?;
        let window = transition.window.clone();
        let restored_output = transition
            .placement
            .as_ref()
            .and_then(|placement| placement.output.as_deref())
            .and_then(|name| wayland.space.outputs().find(|output| output.name() == name))
            .cloned();
        let restoring = restored_output.is_some();
        let output = restored_output.or_else(|| super::focus::selected_output(wayland).cloned());
        let location = if restoring {
            transition
                .placement
                .as_ref()
                .map(|placement| placement.location)
                .unwrap_or_else(|| Point::from((0, 0)))
        } else {
            output
                .as_ref()
                .map(|output| centered_location(wayland, cameras, output, &window))
                .unwrap_or_else(|| Point::from((0, 0)))
        };
        if let Some(output) = output.as_ref() {
            super::set_window_output(&window, output);
        }
        wayland.space.map_element(window.clone(), location, false);
        // New windows steal focus - matches most WMs' default behavior.
        // Also raises+activates via `focus_and_raise`, same as clicking a
        // window now does.
        crate::window::focus_and_raise(wayland, &window);
        wayland.windows.update_placement(
            &key,
            Placement {
                location,
                output: super::window_output_name(&window),
            },
        );
        let finalized = wayland
            .windows
            .finalize_map(&key)
            .expect("new XDG map generation must finalize once");
        eventline::debug!(
            "window: XDG surface {} mapped generation={} first={}",
            surface.id(),
            finalized.generation,
            finalized.first_map
        );
        return Some(CommitOutcome::Mapped(finalized));
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
