use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
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
/// redraw) prunes it automatically.
pub fn toplevel_destroyed(wayland: &mut WaylandState, surface: &ToplevelSurface) {
    wayland.unmapped.remove(surface.wl_surface());
}

/// The commit-path half of the unmapped -> mapped transition: sends the
/// initial configure for a still-unmapped toplevel, then promotes it into
/// `space` once it has actually attached a buffer. This is the one
/// deliberate deviation from smallvil, which maps a toplevel into `Space`
/// immediately in `new_toplevel` and relies on Smithay's render-element
/// code silently skipping bufferless surfaces - here "mapped" means
/// "actually visible," not "present in `Space` but incidentally invisible."
pub fn handle_commit(wayland: &mut WaylandState, surface: &WlSurface) {
    let Some(window) = wayland.unmapped.get(surface) else {
        return;
    };
    let Some(toplevel) = window.toplevel() else {
        return;
    };

    if !toplevel.is_initial_configure_sent() {
        toplevel.send_configure();
        return;
    }

    let has_buffer =
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
    if has_buffer {
        let window = wayland.unmapped.remove(surface).expect("checked above");
        wayland.space.map_element(window, (0, 0), false);
    }
}
