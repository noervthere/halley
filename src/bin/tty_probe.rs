use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::generic::Generic;
use calloop::{Interest, Mode as CalloopMode, PostAction};
use smithay::backend::drm::DrmEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::{delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell};

// src/bin/*.rs binaries are separate crates from main.rs and can't import
// its modules directly - reuse the same source tree via #[path] rather than
// touching main.rs (out of scope for this plan) to share it.
#[path = "../backend/mod.rs"]
mod backend;
#[path = "../cursor.rs"]
mod cursor;
#[path = "../input/mod.rs"]
mod input;
#[path = "../wayland/mod.rs"]
mod wayland;

use backend::CLEAR_COLOR;
use backend::Renderable;
use backend::tty::TtyBackend;
use cursor::CursorImage;
use input::Keyboard;
use input::keybinds::BackendKind;
use input::pointer::Pointer;
use wayland::{ClientState, WaylandState};

/// Mirrors main.rs's `App` shape (backend + keyboard + pointer + cursor +
/// exit flag + wayland/seat_state/seat). Still a separate struct in a
/// separate binary; full winit/tty unification is later, explicitly-deferred
/// work.
struct TtyApp {
    backend: TtyBackend,
    keyboard: Keyboard,
    pointer: Pointer,
    cursor: CursorImage,
    exit: bool,
    wayland: WaylandState,
    seat_state: SeatState<TtyApp>,
    seat: Seat<TtyApp>,
    start_time: Instant,
}

fn main() {
    let (backend, session_notifier, drm_notifier) = match TtyBackend::new() {
        Ok(parts) => parts,
        // Expected nested under a host compositor (niri already holds
        // exclusive session control) - confirmed since step 3. Real success
        // needs a free VT.
        Err(err) => {
            println!("TtyBackend::new() failed: {err}");
            return;
        }
    };
    println!("TtyBackend constructed successfully");

    let mut libinput_context = Libinput::new_with_udev::<LibinputSessionInterface<_>>(
        backend.session().into(),
    );
    libinput_context
        .udev_assign_seat(&backend.session().seat())
        .expect("failed to assign udev seat for libinput");
    let libinput_backend = LibinputInputBackend::new(libinput_context);

    let mut event_loop: EventLoop<TtyApp> = EventLoop::try_new().expect("failed to create event loop");

    let display: Display<TtyApp> = Display::new().expect("failed to create wayland display");
    let dh = display.handle();

    let compositor_state = CompositorState::new::<TtyApp>(&dh);
    let xdg_shell_state = XdgShellState::new::<TtyApp>(&dh);
    let shm_state = ShmState::new::<TtyApp>(&dh, vec![]);
    let output_manager_state = OutputManagerState::new();

    let mut seat_state = SeatState::new();
    let mut seat: Seat<TtyApp> = seat_state.new_wl_seat(&dh, "seat0");
    seat.add_keyboard(Default::default(), 200, 25)
        .expect("failed to advertise keyboard capability on the wl_seat");
    seat.add_pointer();

    let _output_global = backend.primary_output().create_global::<TtyApp>(&dh);

    let mut app = TtyApp {
        backend,
        keyboard: Keyboard::new(BackendKind::Tty).expect("failed to set up keyboard input"),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorImage::load(),
        exit: false,
        wayland: WaylandState::new(
            dh,
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
        ),
        seat_state,
        seat,
        start_time: Instant::now(),
    };
    app.wayland
        .space
        .map_output(app.backend.primary_output(), (0, 0));

    let socket_name = init_wayland_listener(display, &mut event_loop);
    println!("wayland socket ready, WAYLAND_DISPLAY={socket_name:?}");

    let cursor_position = app.pointer.position();
    match app
        .backend
        .render(CLEAR_COLOR, &app.cursor, cursor_position, &app.wayland.space)
    {
        Ok(()) => println!("first render() succeeded"),
        Err(err) => println!("first render() failed (same caveat as initialize_output): {err}"),
    }

    event_loop
        .handle()
        .insert_source(libinput_backend, |event, _, app| {
            let output_size = app.backend.output_size().to_logical(1);
            app.pointer.process_input_event(&event, output_size);
            if let Some(halley_config::Action::Quit) = app.keyboard.process_input_event(event) {
                app.exit = true;
            }
        })
        .expect("failed to insert libinput source");

    event_loop
        .handle()
        .insert_source(session_notifier, |event, _, app| match event {
            SessionEvent::PauseSession => {
                println!("session event: pause");
                app.backend.pause();
            }
            SessionEvent::ActivateSession => {
                println!("session event: activate");
                match app.backend.resume() {
                    Ok(()) => {
                        let cursor_position = app.pointer.position();
                        if let Err(err) = app.backend.render(
                            CLEAR_COLOR,
                            &app.cursor,
                            cursor_position,
                            &app.wayland.space,
                        ) {
                            println!("post-resume render failed: {err}");
                        }
                    }
                    Err(err) => println!("resume failed: {err}"),
                }
            }
        })
        .expect("failed to insert session notifier");

    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, app| match event {
            DrmEvent::VBlank(crtc) => {
                if let Err(err) = app.backend.frame_submitted(crtc) {
                    println!("frame_submitted failed for {crtc:?}: {err}");
                    return;
                }
                let cursor_position = app.pointer.position();
                if let Err(err) = app.backend.render(
                    CLEAR_COLOR,
                    &app.cursor,
                    cursor_position,
                    &app.wayland.space,
                ) {
                    println!("render failed: {err}");
                }

                // Lets clients know their last commit was actually
                // presented, so they schedule their next frame.
                let output = app.backend.primary_output().clone();
                let elapsed = app.start_time.elapsed();
                app.wayland.space.elements().for_each(|window| {
                    window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                        Some(output.clone())
                    });
                });
                app.wayland.space.refresh();
            }
            DrmEvent::Error(err) => println!("drm event: error {err:?}"),
        })
        .expect("failed to insert drm notifier");

    println!("dispatching - switch to this VT to see a solid color fill the screen, press the Quit chord to exit");
    while !app.exit {
        event_loop
            .dispatch(None, &mut app)
            .expect("event loop dispatch failed");
        let _ = app.wayland.display_handle.flush_clients();
    }
    println!("quit requested, exiting cleanly");
}

/// Sets up the listening socket new clients connect to, and the source that
/// actually pumps protocol requests from already-connected clients every
/// loop iteration - mirrors main.rs's `init_wayland_listener`.
fn init_wayland_listener(display: Display<TtyApp>, event_loop: &mut EventLoop<TtyApp>) -> OsString {
    let listening_socket =
        ListeningSocketSource::new_auto().expect("failed to create wayland listening socket");
    let socket_name = listening_socket.socket_name().to_os_string();

    event_loop
        .handle()
        .insert_source(listening_socket, move |client_stream, _, app| {
            if let Err(err) = app
                .wayland
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                eprintln!("failed to insert new wayland client: {err}");
            }
        })
        .expect("failed to insert wayland listening socket source");

    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, app| {
                // Safety: `display` is owned by this source for the event
                // loop's lifetime and is never dropped out from under it.
                unsafe {
                    display.get_mut().dispatch_clients(app)?;
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("failed to insert wayland display dispatch source");

    socket_name
}

impl CompositorHandler for TtyApp {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.wayland.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        wayland::compositor::commit::<Self>(&mut self.wayland, surface);
    }
}

impl BufferHandler for TtyApp {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for TtyApp {
    fn shm_state(&self) -> &ShmState {
        &self.wayland.shm_state
    }
}

impl XdgShellHandler for TtyApp {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.wayland.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        wayland::xdg_shell::new_toplevel(&mut self.wayland, surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        wayland::xdg_shell::toplevel_destroyed(&mut self.wayland, &surface);
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
}

impl SeatHandler for TtyApp {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

impl OutputHandler for TtyApp {}

delegate_compositor!(TtyApp);
delegate_shm!(TtyApp);
delegate_xdg_shell!(TtyApp);
delegate_seat!(TtyApp);
delegate_output!(TtyApp);
