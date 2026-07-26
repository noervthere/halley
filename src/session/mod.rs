use std::ffi::OsStr;

use halley_config::Action;
use halley_core::camera::Camera;

use crate::spawn;
use crate::wayland::{self, WaylandState};

mod protocol;
mod state;
mod input;
mod tty_frame;

pub mod tty;
pub mod winit;

pub use state::{Session, SessionDriver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionControl {
    Continue,
    Quit,
}

/// Interprets every configured action once for both session backends.
/// Backends provide the camera selected by their own output routing and
/// translate the returned quit request into their loop's native mechanism.
fn dispatch_action(
    action: Action,
    wayland: &WaylandState,
    terminal_command: Option<&str>,
    socket_name: &OsStr,
    camera: Option<&mut Camera>,
    zoom: &halley_config::Zoom,
) -> SessionControl {
    match action {
        Action::Quit => return SessionControl::Quit,
        Action::CloseFocusedWindow => wayland::xdg_shell::close_focused(wayland),
        Action::OpenTerminal => match terminal_command {
            Some(command) => spawn::spawn_detached(command, socket_name),
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
        Action::Spawn(command) => spawn::spawn_detached(&command, socket_name),
    }
    SessionControl::Continue
}

fn sync_keyboard_focus<D: SessionDriver>(session: &mut Session<D>, serial: smithay::utils::Serial) {
    let focused = wayland::focus::current(&session.wayland).map(|focus| focus.surface());
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

fn focus_window<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    serial: smithay::utils::Serial,
) {
    wayland::xdg_shell::focus_and_raise(&mut session.wayland, window);
    sync_keyboard_focus(session, serial);
}
