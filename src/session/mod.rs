use std::borrow::Cow;
use std::ffi::OsStr;
use std::time::Duration;

use calloop::LoopHandle;
use calloop::timer::{TimeoutAction, Timer};
use halley_config::Action;
use halley_core::camera::Camera;
use smithay::wayland::seat::WaylandFocus;

use crate::wayland;

mod autostart;
pub(crate) mod closing;
mod cursor;
mod focus;
mod gesture;
mod input;
mod lifecycle;
pub(crate) mod opening;
mod pointer;
mod protocol;
mod spawn;
mod state;
mod touch;
mod tty_frame;

pub mod environment;
pub mod tty;
pub mod winit;

pub(crate) use focus::focus_window;
pub use state::{Session, SessionDriver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionControl {
    Continue,
    Quit,
    CloseFocusedWindow,
    Screenshot,
    ToggleFullscreen,
    ToggleState,
    Apogee,
    FocusCycle(halley_config::FocusCycleDirection),
    BearingsShow,
    BearingsToggle,
}

#[derive(Clone, Copy)]
struct SpawnContext<'a> {
    socket_name: &'a OsStr,
    x11_display: Option<&'a OsStr>,
    cursor_theme: &'a str,
    cursor_size: u8,
    environment: &'a environment::LaunchEnvironment,
}

/// Interprets every configured action once for both session backends.
/// Backends provide the camera selected by their own output routing and
/// translate the returned quit request into their loop's native mechanism.
fn dispatch_action(
    action: Action,
    terminal_command: Option<&str>,
    spawn_context: SpawnContext<'_>,
    camera: Option<&mut Camera>,
    zoom: &halley_config::Zoom,
) -> SessionControl {
    match action {
        Action::Quit => return SessionControl::Quit,
        Action::CloseFocusedWindow => return SessionControl::CloseFocusedWindow,
        Action::ToggleFullscreen => return SessionControl::ToggleFullscreen,
        Action::ToggleState => return SessionControl::ToggleState,
        Action::Apogee => return SessionControl::Apogee,
        Action::FocusCycle(direction) => return SessionControl::FocusCycle(direction),
        Action::BearingsShow => return SessionControl::BearingsShow,
        Action::BearingsToggle => return SessionControl::BearingsToggle,
        Action::OpenTerminal => match terminal_command {
            Some(command) => spawn::spawn_detached(
                command,
                spawn_context.socket_name,
                spawn_context.x11_display,
                spawn_context.cursor_theme,
                spawn_context.cursor_size,
                spawn_context.environment,
            ),
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
        Action::Spawn(command) => spawn::spawn_detached(
            &command,
            spawn_context.socket_name,
            spawn_context.x11_display,
            spawn_context.cursor_theme,
            spawn_context.cursor_size,
            spawn_context.environment,
        ),
    }
    SessionControl::Continue
}

pub(crate) fn cancel_grab_for_surface<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    if crate::input::grab::belongs_to_surface(&session.grab, surface) {
        session.grab = crate::input::grab::Grab::None;
        session.cursor.set_override(None);
        crate::input::grab::forget_resize_anchor(&mut session.resize_anchor, surface);
    }
}

pub(crate) use lifecycle::{finish_window_unmap, prepare_window_unmap};

fn install_node_decay_timer<D: SessionDriver>(
    handle: &LoopHandle<'_, Session<D>>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            |_, _, session| {
                crate::nodes::tick_decay(session);
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )
        .map(|_| ())
        .map_err(Into::into)
}

fn install_apogee_preview_timer<D: SessionDriver>(
    handle: &LoopHandle<'_, Session<D>>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(8)),
            |_, _, session| {
                if session.apogee.take_live_redraw_due(
                    crate::frame_clock::monotonic_now(),
                    session.apogee_config.preview_max_fps,
                ) {
                    session.request_redraw();
                }
                TimeoutAction::ToDuration(Duration::from_millis(8))
            },
        )
        .map(|_| ())
        .map_err(Into::into)
}

pub(crate) fn reconcile_pointer_constraints<D: SessionDriver>(session: &mut Session<D>) {
    pointer::reconcile_state(session);
}

pub(crate) fn has_active_pointer_confinement<D: SessionDriver>(session: &Session<D>) -> bool {
    pointer::has_active_confinement(session)
}

pub(crate) fn cursor_visible<D: SessionDriver>(session: &Session<D>) -> bool {
    pointer::cursor_visible(session)
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
    pointer::reconcile_state(session);
    session.request_redraw();
}

pub(crate) fn sync_keyboard_focus<D: SessionDriver>(
    session: &mut Session<D>,
    serial: smithay::utils::Serial,
) {
    wayland::focus::refresh_selected_layer(&mut session.wayland);
    if let Some(surface) = session.wayland.focused_window.clone() {
        session
            .nodes
            .focus_surface(&surface, session.start_time.elapsed().as_millis() as u64);
    } else if session
        .nodes
        .focused()
        .and_then(|id| session.nodes.record(id))
        .is_some_and(|record| !record.collapsed)
    {
        session.nodes.focus(None, 0);
    }
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
    let next_constraint_root = focused
        .as_ref()
        .and_then(|target| target.wl_surface().map(Cow::into_owned))
        .filter(|surface| session.wayland.focused_window.as_ref() == Some(surface));
    pointer::prepare_keyboard_focus_change(session, next_constraint_root.as_ref());
    keyboard.set_focus(session, focused, serial);
    pointer::reconcile_state(session);
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
    focus::focus_window_from_pointer(session, window, serial);
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
