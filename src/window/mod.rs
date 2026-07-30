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

/// Override-redirect X11 windows are deliberately outside normal window
/// manager focus policy. ICCCM leaves keyboard focus for those windows to the
/// client (typically via a grab or a managed owner using WM_TAKE_FOCUS).
pub fn accepts_wm_focus(window: &Window) -> bool {
    focus_policy(crate::xwayland::is_override_redirect(window))
}

/// Compositor move/resize grabs are window-management operations. X11 menus,
/// tooltips, and other override-redirect surfaces remain interactive client
/// surfaces, but are never draggable or resizable as managed windows.
pub fn accepts_compositor_grab(window: &Window) -> bool {
    compositor_grab_policy(crate::xwayland::is_override_redirect(window))
}

pub fn focus(wayland: &mut WaylandState, window: &Window, raise: bool) {
    if !accepts_wm_focus(window) {
        return;
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
    if raise {
        raise_managed(wayland, window);
    }
    wayland.focused_window = window.wl_surface().map(|surface| surface.into_owned());
}

pub fn raise_managed(wayland: &mut WaylandState, window: &Window) {
    if !accepts_wm_focus(window) {
        return;
    }
    if let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) {
        wayland.managed_windows.raise(surface);
    }
    if let Some(location) = wayland.space.element_location(window) {
        wayland.space.map_element(window.clone(), location, true);
    }
}

pub fn focus_and_raise(wayland: &mut WaylandState, window: &Window) {
    focus(wayland, window, true);
}

pub fn clear_focus(wayland: &mut WaylandState) {
    wayland.focused_layer = None;
    for mapped in wayland.space.elements() {
        if mapped.set_activated(false)
            && let Some(toplevel) = mapped.toplevel()
            && toplevel.is_initial_configure_sent()
        {
            toplevel.send_pending_configure();
        }
    }
    wayland.focused_window = None;
}

fn focus_policy(override_redirect: bool) -> bool {
    !override_redirect
}

fn compositor_grab_policy(override_redirect: bool) -> bool {
    !override_redirect
}

#[cfg(test)]
mod tests {
    #[test]
    fn override_redirect_policy_is_unmanaged() {
        assert!(!super::focus_policy(true));
        assert!(super::focus_policy(false));
    }

    #[test]
    fn override_redirect_cannot_start_a_compositor_grab() {
        assert!(!super::compositor_grab_policy(true));
        assert!(super::compositor_grab_policy(false));
    }
}
