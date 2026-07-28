use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
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
        crate::window::place_mapping(wayland, cameras, &transition);
        // New windows steal focus - matches most WMs' default behavior.
        // Also raises+activates via `focus_and_raise`, same as clicking a
        // window now does.
        crate::window::focus_and_raise(wayland, &window);
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
