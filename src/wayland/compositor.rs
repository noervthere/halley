use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{get_parent, is_sync_subsurface};

use super::WaylandState;
use super::{layer_shell, xdg_shell};

/// Shared `CompositorHandler::commit` body, called identically from `App`
/// and `TtyApp`. Deliberately narrow: buffer import, refreshing whichever
/// window owns the committed surface tree, and the unmapped -> mapped
/// transition - nothing else. Old halley's equivalent special-cased cursor
/// surfaces directly inside this same path; that doesn't happen here, since
/// there is no cursor-surface concept yet at all.
pub fn commit<D: 'static>(wayland: &mut WaylandState, surface: &WlSurface) {
    on_commit_buffer_handler::<D>(surface);

    if is_sync_subsurface(surface) {
        return;
    }

    // Subsurface commits should still refresh/potentially-map the root
    // toplevel they belong to, not just direct toplevel-surface commits.
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }

    wayland.popup_manager.commit(surface);

    if layer_shell::handle_commit(wayland, &root) {
        return;
    }

    let owning_window = wayland
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
        .or_else(|| wayland.unmapped.get(&root));
    if let Some(window) = owning_window {
        window.on_commit();
    }

    xdg_shell::handle_commit(wayland, &root);
}
