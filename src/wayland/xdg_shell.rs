use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::camera::OutputCameras;

use super::WaylandState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToplevelCommit {
    None,
    Mapped(WlSurface),
    Unmapped(WlSurface),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MappingTransition {
    StayMapped,
    Unmap,
    Map,
    StayUnmapped,
}

fn mapping_transition(currently_mapped: bool, has_buffer: bool) -> MappingTransition {
    match (currently_mapped, has_buffer) {
        (true, true) => MappingTransition::StayMapped,
        (true, false) => MappingTransition::Unmap,
        (false, true) => MappingTransition::Map,
        (false, false) => MappingTransition::StayUnmapped,
    }
}

pub fn will_unmap(wayland: &WaylandState, surface: &WlSurface) -> bool {
    wayland.space.elements().any(|window| {
        window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == surface)
    }) && !with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false)
}

/// A new toplevel role was created - stash it as unmapped, nothing shown
/// yet. Deliberately just this: no spawn-rule resolution, no monitor/
/// cluster placement, no reveal animation. Old halley fused exactly this
/// kind of window-manager policy into `XdgShellHandler::new_toplevel`
/// itself; that policy, if and when it exists, belongs downstream of
/// mapping, not inside protocol handling.
pub fn new_toplevel(wayland: &mut WaylandState, surface: ToplevelSurface) {
    let wl_surface = surface.wl_surface().clone();
    let window = Window::new_wayland_window(surface);
    wayland.unmapped.insert(wl_surface, window);
}

/// Removes a destroyed toplevel from whichever lifecycle state owns it.
///
/// This must explicitly unmap a mapped window: Smithay calls this handler
/// while the underlying `wl_surface` may still be alive, so waiting for
/// `Space::refresh()` can leave Halley's compositor-drawn border in the next
/// frame. Focus clears to `None`; there is no fallback-refocus policy yet.
pub fn toplevel_destroyed(wayland: &mut WaylandState, surface: &ToplevelSurface) {
    wayland.unmapped.remove(surface.wl_surface());
    wayland.unmapped_locations.remove(surface.wl_surface());
    let mapped = wayland
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface.wl_surface())
        })
        .cloned();
    if let Some(window) = mapped {
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
) -> ToplevelCommit {
    let has_buffer =
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
    let mapped = {
        wayland
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .cloned()
    };
    if let Some(window) = mapped {
        match mapping_transition(true, has_buffer) {
            MappingTransition::StayMapped => return ToplevelCommit::None,
            MappingTransition::Unmap => {}
            MappingTransition::Map | MappingTransition::StayUnmapped => unreachable!(),
        }
        if let Some(location) = wayland.space.element_location(&window) {
            wayland.unmapped_locations.insert(surface.clone(), location);
        }
        wayland.space.unmap_elem(&window);
        wayland.unmapped.insert(surface.clone(), window);
        if wayland.focused_window.as_ref() == Some(surface) {
            wayland.focused_window = None;
        }
        return ToplevelCommit::Unmapped(surface.clone());
    }

    let Some(window) = wayland.unmapped.get(surface) else {
        return ToplevelCommit::None;
    };
    let Some(toplevel) = window.toplevel() else {
        return ToplevelCommit::None;
    };

    if !toplevel.is_initial_configure_sent() {
        toplevel.with_pending_state(super::decoration::apply_tiled_hint);
        toplevel.send_configure();
        return ToplevelCommit::None;
    }

    if mapping_transition(false, has_buffer) == MappingTransition::Map {
        let window = wayland.unmapped.remove(surface).expect("checked above");
        let output = super::focus::selected_output(wayland).cloned();
        let location = wayland
            .unmapped_locations
            .remove(surface)
            .or_else(|| {
                output
                    .as_ref()
                    .map(|output| centered_location(wayland, cameras, output, &window))
            })
            .unwrap_or_else(|| Point::from((0, 0)));
        if let Some(output) = output.as_ref() {
            super::set_window_output(&window, output);
        }
        wayland.space.map_element(window.clone(), location, false);
        // New windows steal focus - matches most WMs' default behavior.
        // Also raises+activates via `focus_and_raise`, same as clicking a
        // window now does.
        crate::window::focus_and_raise(wayland, &window);
        return ToplevelCommit::Mapped(surface.clone());
    }

    ToplevelCommit::None
}

/// Centers a newly-mapped window on the selected output's live camera.
/// Existing freeform windows stay where they are when the camera later moves.
pub(crate) fn centered_location(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    output: &Output,
    window: &Window,
) -> Point<i32, Logical> {
    centered_location_for_size(wayland, cameras, output, window.geometry().size)
}

pub(crate) fn centered_location_for_size(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    output: &Output,
    window_size: Size<i32, Logical>,
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
    center_window(center, window_size)
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

    #[test]
    fn client_opening_size_is_centered_independently_of_its_buffer() {
        assert_eq!(
            center_window(Point::from((1280.0, 720.0)), Size::from((640, 480))),
            Point::from((960, 480))
        );
    }

    #[test]
    fn mapped_null_buffer_unmaps_and_a_later_buffer_remaps() {
        assert_eq!(mapping_transition(true, false), MappingTransition::Unmap);
        assert_eq!(
            mapping_transition(false, false),
            MappingTransition::StayUnmapped
        );
        assert_eq!(mapping_transition(false, true), MappingTransition::Map);
        assert_eq!(
            mapping_transition(true, true),
            MappingTransition::StayMapped
        );
    }
}
