use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{get_parent, is_sync_subsurface};
use smithay::wayland::seat::WaylandFocus;

use super::WaylandState;
use super::{layer_shell, popup, xdg_shell};

/// Shared role-neutral commit pipeline for both session drivers.
///
/// This imports the committed buffer, resolves a subsurface to its root,
/// updates the shared popup tree, then delegates role-specific lifecycle to
/// `layer_shell` or `xdg_shell`. Rendering, focus policy, and backend redraw
/// scheduling remain outside this protocol callback.
pub fn commit<D: 'static>(
    wayland: &mut WaylandState,
    cameras: &crate::camera::OutputCameras,
    surface: &WlSurface,
) -> Option<WlSurface> {
    on_commit_buffer_handler::<D>(surface);

    if is_sync_subsurface(surface) {
        return None;
    }

    // Subsurface commits should still refresh/potentially-map the root
    // toplevel they belong to, not just direct toplevel-surface commits.
    let root = root_surface(surface);

    popup::handle_commit(&mut wayland.popup_manager, surface);

    if layer_shell::handle_commit(wayland, &root) {
        return None;
    }

    let owning_window = wayland
        .space
        .elements()
        .find(|window| {
            window
                .wl_surface()
                .is_some_and(|surface| surface.as_ref() == &root)
        })
        .or_else(|| wayland.windows.window_for_wl_surface(&root));
    if let Some(window) = owning_window {
        window.on_commit();
    }

    xdg_shell::handle_commit(wayland, cameras, &root)
}

pub fn root_surface(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    root
}
