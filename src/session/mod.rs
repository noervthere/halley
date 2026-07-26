use std::ffi::OsStr;

use halley_config::Action;
use halley_core::camera::Camera;
use smithay::wayland::seat::WaylandFocus;

use crate::spawn;
use crate::wayland::{self, WaylandState};

mod input;
mod pointer;
mod protocol;
mod state;
mod tty_frame;

pub mod environment;
pub mod tty;
pub mod winit;

pub use state::{Session, SessionDriver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionControl {
    Continue,
    Quit,
    Screenshot,
    ToggleFullscreen,
}

/// Interprets every configured action once for both session backends.
/// Backends provide the camera selected by their own output routing and
/// translate the returned quit request into their loop's native mechanism.
fn dispatch_action(
    action: Action,
    wayland: &WaylandState,
    terminal_command: Option<&str>,
    socket_name: &OsStr,
    x11_display: Option<&OsStr>,
    camera: Option<&mut Camera>,
    zoom: &halley_config::Zoom,
) -> SessionControl {
    match action {
        Action::Quit => return SessionControl::Quit,
        Action::CloseFocusedWindow => crate::window::close_focused(wayland),
        Action::ToggleFullscreen => return SessionControl::ToggleFullscreen,
        Action::OpenTerminal => match terminal_command {
            Some(command) => spawn::spawn_detached(command, socket_name, x11_display),
            None => eventline::warn!("keybinds: no terminal configured or found on PATH"),
        },
        Action::ZoomOut => {
            if let Some(camera) = camera {
                crate::input::zoom::zoom_out(camera, zoom);
            }
        }
        Action::ZoomIn => {
            if let Some(camera) = camera {
                crate::input::zoom::zoom_in(camera, zoom);
            }
        }
        Action::ZoomReset => {
            if let Some(camera) = camera {
                camera.reset_zoom_target();
            }
        }
        Action::Screenshot => return SessionControl::Screenshot,
        Action::Spawn(command) => spawn::spawn_detached(&command, socket_name, x11_display),
    }
    SessionControl::Continue
}

fn cancel_grab_for_surface<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    if crate::input::grab::belongs_to_surface(&session.grab, surface) {
        session.grab = crate::input::grab::Grab::None;
        crate::input::grab::forget_resize_anchor(&mut session.resize_anchor, surface);
    }
}

fn toggle_focused_fullscreen<D: SessionDriver>(session: &mut Session<D>) {
    let Some(focused) = session.wayland.focused_window.clone() else {
        return;
    };
    let Some(window) = session
        .wayland
        .space
        .elements()
        .find(|window| {
            window
                .wl_surface()
                .is_some_and(|surface| surface.as_ref() == &focused)
        })
        .cloned()
    else {
        return;
    };

    cancel_grab_for_surface(session, &focused);
    let entering = !session.fullscreen.is_fullscreen_or_pending(&focused);
    if let Some(toplevel) = window.toplevel() {
        if entering {
            session
                .fullscreen
                .request(&mut session.wayland, toplevel, None);
        } else {
            session.fullscreen.unrequest(&session.wayland, toplevel);
        }
    } else {
        crate::xwayland::set_window_fullscreen(session, &window, entering);
    }
    session.request_redraw();
}

pub(crate) fn sync_keyboard_focus<D: SessionDriver>(
    session: &mut Session<D>,
    serial: smithay::utils::Serial,
) {
    let focused = wayland::focus::current(
        &session.wayland,
        &session.fullscreen,
        crate::frame_clock::monotonic_now(),
    )
    .and_then(|focus| match focus {
        wayland::focus::KeyboardFocus::Window(surface) => session
            .wayland
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == &surface)
            })
            .and_then(crate::xwayland::KeyboardFocusTarget::for_window)
            .or_else(|| Some(surface.into())),
        wayland::focus::KeyboardFocus::ExclusiveLayer(surface)
        | wayland::focus::KeyboardFocus::OnDemandLayer(surface) => Some(surface.into()),
    });
    let keyboard = session
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup");
    keyboard.set_focus(session, focused, serial);
}

fn focus_layer<D: SessionDriver>(
    session: &mut Session<D>,
    layer: Option<smithay::desktop::LayerSurface>,
    serial: smithay::utils::Serial,
) {
    wayland::focus::select_layer(&mut session.wayland, layer);
    sync_keyboard_focus(session, serial);
}

pub(crate) fn focus_window<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    serial: smithay::utils::Serial,
) {
    crate::window::focus_and_raise(&mut session.wayland, window);
    session.xwayland.raise_window(window);
    sync_keyboard_focus(session, serial);
}

pub(crate) fn begin_window_resize<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    handle: crate::input::grab::ResizeHandle,
    button: u32,
    cursor: halley_core::field::Vec2,
    serial: smithay::utils::Serial,
) -> bool {
    let Some(start_rect) = session.wayland.space.element_geometry(window) else {
        return false;
    };
    focus_window(session, window, serial);
    session.grab = crate::input::grab::Grab::ResizeWindow(crate::input::grab::ResizeState {
        window: window.clone(),
        handle,
        button,
        start_rect,
        start_cursor: cursor,
    });
    session.resize_anchor = window.toplevel().map(|_| crate::input::grab::ResizeAnchor {
        window: window.clone(),
        handle,
        phase: crate::input::grab::ResizePhase::Ongoing,
        last_configure: None,
        last_size: start_rect.size,
    });
    true
}

pub(crate) fn begin_pointer_resize<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    handle: crate::input::grab::ResizeHandle,
    button: u32,
) -> bool {
    let Some(route) = pointer::route_client(session) else {
        return false;
    };
    if !matches!(
        route.target,
        crate::input::pointer::PointerTarget::Window(ref routed) if routed == window
    ) {
        return false;
    }
    let cursor = halley_core::field::Vec2 {
        x: route.location.x as f32,
        y: route.location.y as f32,
    };
    begin_window_resize(
        session,
        window,
        handle,
        button,
        cursor,
        smithay::utils::SERIAL_COUNTER.next_serial(),
    )
}
