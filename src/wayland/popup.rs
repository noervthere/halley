use smithay::desktop::{
    PopupGrab, PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy,
    WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
};
use smithay::input::Seat;
use smithay::input::SeatHandler;
use smithay::input::pointer::Focus;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Rectangle, Serial};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{PopupSurface, PositionerState};

use crate::presentation::camera::OutputCameras;

use super::WaylandState;

pub fn track(wayland: &mut WaylandState, cameras: &OutputCameras, surface: PopupSurface) {
    let popup = PopupKind::Xdg(surface);
    unconstrain(wayland, cameras, &popup);
    if let Err(err) = wayland.popup_manager.track_popup(popup) {
        eventline::warn!("xdg-shell: failed to track popup: {err}");
    }
}

/// Updates the tracked popup tree and completes the initial configure
/// handshake once an XDG popup has made its first surface commit.
pub fn handle_commit(manager: &mut PopupManager, surface: &WlSurface) {
    manager.commit(surface);

    if let Some(PopupKind::Xdg(popup)) = manager.find_popup(surface)
        && !popup.is_initial_configure_sent()
    {
        popup
            .send_configure()
            .expect("initial popup configure failed");
    }
}

pub fn unconstrain_surface(wayland: &WaylandState, cameras: &OutputCameras, surface: PopupSurface) {
    unconstrain(wayland, cameras, &PopupKind::Xdg(surface));
}

pub fn reposition(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    surface: PopupSurface,
    positioner: PositionerState,
    token: u32,
) {
    surface.with_pending_state(|state| {
        state.geometry = positioner.get_geometry();
        state.positioner = positioner;
    });
    unconstrain(wayland, cameras, &PopupKind::Xdg(surface.clone()));
    surface.send_repositioned(token);
}

pub fn update_reactive_for_window(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    window: &smithay::desktop::Window,
) {
    let Some(toplevel) = window.toplevel() else {
        return;
    };
    for (popup, _) in PopupManager::popups_for_surface(toplevel.wl_surface()) {
        let PopupKind::Xdg(surface) = &popup else {
            continue;
        };
        if !surface.with_pending_state(|state| state.positioner.reactive) {
            continue;
        }
        unconstrain(wayland, cameras, &popup);
        if let Err(err) = surface.send_pending_configure() {
            eventline::warn!("xdg-shell: failed to reconfigure reactive popup: {err}");
        }
    }
}

/// Constrains a popup in the coordinate system of its root. Windows use
/// their owning output camera's visible world rectangle; layer roots use
/// their output-local `LayerMap` geometry because layers never pass through
/// a workspace camera.
fn unconstrain(wayland: &WaylandState, cameras: &OutputCameras, popup: &PopupKind) {
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
        let Some(output_geometry) = wayland.space.output_geometry(output) else {
            return;
        };
        let Some(view) = cameras.view(&output.name()) else {
            return;
        };
        let mut target = crate::presentation::camera::world_viewport(view, output_geometry);
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
    D: SeatHandler<PointerFocus = WlSurface> + 'static,
    D::KeyboardFocus: WaylandFocus + From<WlSurface> + From<PopupKind>,
    WlSurface: From<D::KeyboardFocus>,
{
    let popup = PopupKind::Xdg(surface);
    let root = find_popup_root_surface(&popup).ok()?.into();
    match manager.grab_popup(root, popup, seat, serial) {
        Ok(grab) => Some(grab),
        Err(err) => {
            eventline::warn!("xdg-shell: rejected popup grab: {err}");
            None
        }
    }
}

pub fn install_grab<D>(data: &mut D, seat: &Seat<D>, mut grab: PopupGrab<D>, serial: Serial)
where
    D: SeatHandler<PointerFocus = WlSurface> + 'static,
    D::KeyboardFocus: WaylandFocus + From<WlSurface> + From<PopupKind>,
    WlSurface: From<D::KeyboardFocus>,
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
