pub(crate) mod routing;

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;

use crate::wayland::WaylandState;

/// Bottom-to-top order for managed windows.
///
/// Smithay's space also has an element order, but it includes unmanaged
/// surfaces and is changed by presentation mechanics. Window-management
/// policy uses this order instead.
#[derive(Debug, Default)]
pub struct ManagedWindowStack {
    order: Vec<WlSurface>,
}

impl ManagedWindowStack {
    pub fn raise(&mut self, surface: WlSurface) {
        self.remove(&surface);
        self.order.push(surface);
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.order.retain(|candidate| candidate != surface);
    }

    pub fn top_to_bottom(&self) -> impl Iterator<Item = &WlSurface> {
        self.order.iter().rev()
    }

    pub fn contains(&self, surface: &WlSurface) -> bool {
        self.order.iter().any(|candidate| candidate == surface)
    }
}

pub fn focus(wayland: &mut WaylandState, window: &Window, raise: bool) {
    if let Some(surface) = window.wl_surface().map(|surface| surface.into_owned())
        && !crate::xwayland::is_override_redirect(window)
        && raise
    {
        wayland.managed_windows.raise(surface);
    }
    wayland.focused_layer = None;
    for mapped in wayland.space.elements() {
        if mapped.set_activated(mapped == window)
            && let Some(toplevel) = mapped.toplevel()
            && toplevel.is_initial_configure_sent()
        {
            toplevel.send_pending_configure();
        }
    }
    if raise && let Some(location) = wayland.space.element_location(window) {
        wayland.space.map_element(window.clone(), location, true);
    }
    wayland.focused_window = window.wl_surface().map(|surface| surface.into_owned());
}

pub fn focus_and_raise(wayland: &mut WaylandState, window: &Window) {
    focus(wayland, window, true);
}

pub fn close_focused(wayland: &WaylandState) {
    let Some(focused) = wayland.focused_window.as_ref() else {
        return;
    };
    let Some(window) = wayland.space.elements().find(|window| {
        window
            .wl_surface()
            .is_some_and(|surface| surface.as_ref() == focused)
    }) else {
        return;
    };
    if let Some(toplevel) = window.toplevel() {
        toplevel.send_close();
    } else {
        crate::xwayland::close_window(window);
    }
}
