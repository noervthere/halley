use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, PopupGrab,
    PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy,
    WindowSurfaceType,
};
use smithay::input::pointer::Focus;
use smithay::input::Seat;
use smithay::input::SeatHandler;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Rectangle, Serial};
use smithay::wayland::shell::xdg::{PopupSurface, PositionerState};

use super::WaylandState;

pub fn track(wayland: &mut WaylandState, surface: PopupSurface) {
    let popup = PopupKind::Xdg(surface);
    unconstrain(wayland, &popup);
    if let Err(err) = wayland.popup_manager.track_popup(popup) {
        eprintln!("xdg-shell: failed to track popup: {err}");
    }
}

pub fn unconstrain_surface(wayland: &WaylandState, surface: PopupSurface) {
    unconstrain(wayland, &PopupKind::Xdg(surface));
}

pub fn reposition(
    wayland: &WaylandState,
    surface: PopupSurface,
    positioner: PositionerState,
    token: u32,
) {
    surface.with_pending_state(|state| {
        state.geometry = positioner.get_geometry();
        state.positioner = positioner;
    });
    unconstrain(wayland, &PopupKind::Xdg(surface.clone()));
    surface.send_repositioned(token);
}

/// Constrains a popup in the coordinate system of its root. Windows use
/// their owning output's global `Space` rectangle; layer roots use their
/// output-local `LayerMap` geometry because layers never pass through a
/// workspace camera.
fn unconstrain(wayland: &WaylandState, popup: &PopupKind) {
    let Ok(root) = find_popup_root_surface(popup) else {
        return;
    };

    if let Some(window) = wayland.space.elements().find(|window| {
        window
            .toplevel()
            .is_some_and(|toplevel| toplevel.wl_surface() == &root)
    }) {
        let Some(window_geometry) = wayland.space.element_geometry(window) else {
            return;
        };
        let Some(primary) = wayland.space.outputs().next().cloned() else {
            return;
        };
        let Some(output) = wayland
            .space
            .outputs()
            .find(|output| super::window_is_on_output(window, output, &primary))
        else {
            return;
        };
        let Some(mut target) = wayland.space.output_geometry(output) else {
            return;
        };
        target.loc -= window_geometry.loc;
        target.loc -= get_popup_toplevel_coords(popup);
        set_unconstrained_geometry(popup, target);
        return;
    }

    for output in wayland.space.outputs() {
        let map = layer_map_for_output(output);
        let Some(layer) = map
            .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
            .cloned()
        else {
            continue;
        };
        let Some(layer_geometry) = map.layer_geometry(&layer) else {
            return;
        };
        let Some(output_geometry) = wayland.space.output_geometry(output) else {
            return;
        };
        let mut target = Rectangle::from_size(output_geometry.size);
        target.loc -= layer_geometry.loc;
        target.loc -= get_popup_toplevel_coords(popup);
        set_unconstrained_geometry(popup, target);
        return;
    }
}

fn set_unconstrained_geometry(popup: &PopupKind, target: Rectangle<i32, smithay::utils::Logical>) {
    let PopupKind::Xdg(surface) = popup else {
        return;
    };
    surface.with_pending_state(|state| {
        state.geometry = state.positioner.get_unconstrained_geometry(target);
    });
}

pub fn begin_grab<D>(
    manager: &mut PopupManager,
    seat: &Seat<D>,
    surface: PopupSurface,
    serial: Serial,
) -> Option<PopupGrab<D>>
where
    D: SeatHandler<KeyboardFocus = WlSurface, PointerFocus = WlSurface> + 'static,
{
    let popup = PopupKind::Xdg(surface);
    let root = find_popup_root_surface(&popup).ok()?;
    match manager.grab_popup(root, popup, seat, serial) {
        Ok(grab) => Some(grab),
        Err(err) => {
            eprintln!("xdg-shell: rejected popup grab: {err}");
            None
        }
    }
}

pub fn install_grab<D>(data: &mut D, seat: &Seat<D>, mut grab: PopupGrab<D>, serial: Serial)
where
    D: SeatHandler<KeyboardFocus = WlSurface, PointerFocus = WlSurface> + 'static,
{
    if let Some(keyboard) = seat.get_keyboard() {
        if keyboard.is_grabbed()
            && !(keyboard.has_grab(serial)
                || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
        {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        keyboard.set_focus(data, grab.current_grab(), serial);
        keyboard.set_grab(data, PopupKeyboardGrab::new(&grab), serial);
    }

    if let Some(pointer) = seat.get_pointer() {
        if pointer.is_grabbed()
            && !(pointer.has_grab(serial)
                || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
        {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        pointer.set_grab(data, PopupPointerGrab::new(&grab), serial, Focus::Keep);
    }
}
