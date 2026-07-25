use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::{Window, layer_map_for_output};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::shell::xdg::ToplevelSurface;

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
    wayland.unmapped.insert(wl_surface, window);
}

/// A toplevel was destroyed - only needs handling if it never made it past
/// `unmapped`. Once a window is in `space`, `Space::refresh()` (called every
/// redraw) prunes it automatically. If the destroyed window was the focused
/// one, focus just clears to `None` - no fallback-refocus-another-window
/// logic this round.
pub fn toplevel_destroyed(wayland: &mut WaylandState, surface: &ToplevelSurface) {
    wayland.unmapped.remove(surface.wl_surface());
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
pub fn handle_commit(wayland: &mut WaylandState, surface: &WlSurface) -> Option<WlSurface> {
    let Some(window) = wayland.unmapped.get(surface) else {
        return None;
    };
    let Some(toplevel) = window.toplevel() else {
        return None;
    };

    if !toplevel.is_initial_configure_sent() {
        toplevel.send_configure();
        return None;
    }

    let has_buffer =
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
    if has_buffer {
        let window = wayland.unmapped.remove(surface).expect("checked above");
        let location = centered_location(wayland, &window);
        if let Some(output) = wayland.space.outputs().next() {
            super::set_window_output(&window, output);
        }
        wayland.space.map_element(window.clone(), location, false);
        // New windows steal focus - matches most WMs' default behavior.
        // Also raises+activates via `focus_and_raise`, same as clicking a
        // window now does.
        focus_and_raise(wayland, &window);
        return Some(surface.clone());
    }

    None
}

/// Marks `window` as focused, raises it to the top of the stack, and sets
/// Smithay's own "activated" xdg-toplevel state on it (clearing that state
/// from every other window) - shared by the new-window-steals-focus path
/// above and `input::grab`'s click-to-focus/grab-start paths. `window` must
/// already be mapped into `wayland.space`.
pub fn focus_and_raise(wayland: &mut WaylandState, window: &Window) {
    wayland.focused_layer = None;
    if let Some(location) = wayland.space.element_location(window) {
        wayland.space.map_element(window.clone(), location, true);
    }
    if let Some(toplevel) = window.toplevel() {
        wayland.focused_window = Some(toplevel.wl_surface().clone());
    }
}

/// Sends a close request to the focused window's toplevel, if any - the
/// client decides whether/how to actually close (an unsaved-changes prompt,
/// etc.), matching every other close keybind/button on any desktop. A no-op
/// if nothing is focused or the focused surface somehow isn't in `space`.
pub fn close_focused(wayland: &WaylandState) {
    let Some(focused) = wayland.focused_window.as_ref() else {
        return;
    };
    let Some(window) = wayland
        .space
        .elements()
        .find(|window| window.toplevel().is_some_and(|t| t.wl_surface() == focused))
    else {
        return;
    };
    if let Some(toplevel) = window.toplevel() {
        toplevel.send_close();
    }
}

/// Where a newly-mapped window should land: centered in the primary
/// output's layer-shell work area rather than underneath an exclusive panel.
/// Existing freeform windows are not moved when that work area changes.
fn centered_location(wayland: &WaylandState, window: &Window) -> Point<i32, Logical> {
    let Some(output) = wayland.space.outputs().next() else {
        return (0, 0).into();
    };
    let Some(output_geo) = wayland.space.output_geometry(output) else {
        return (0, 0).into();
    };
    let usable = layer_map_for_output(output).non_exclusive_zone();
    let offset = (usable.size - window.geometry().size) / 2;
    output_geo.loc + usable.loc + offset
}
