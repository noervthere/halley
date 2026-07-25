use std::ffi::OsStr;

use halley_config::Action;
use halley_core::camera::Camera;

use crate::spawn;
use crate::wayland::{self, WaylandState};

pub mod tty;
pub mod winit;

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
            None => eprintln!("keybinds: no terminal configured or found on PATH"),
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
