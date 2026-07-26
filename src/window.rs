use smithay::desktop::Window;
use smithay::wayland::seat::WaylandFocus;

use crate::wayland::WaylandState;

pub fn focus_and_raise(wayland: &mut WaylandState, window: &Window) {
    wayland.focused_layer = None;
    for mapped in wayland.space.elements() {
        if mapped.set_activated(mapped == window)
            && let Some(toplevel) = mapped.toplevel()
            && toplevel.is_initial_configure_sent()
        {
            toplevel.send_pending_configure();
        }
    }
    if let Some(location) = wayland.space.element_location(window) {
        wayland.space.map_element(window.clone(), location, true);
    }
    wayland.focused_window = window.wl_surface().map(|surface| surface.into_owned());
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
