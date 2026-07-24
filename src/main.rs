use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode as CalloopMode, PostAction, RegistrationToken};
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
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::{
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_decoration,
    delegate_xdg_shell,
};

mod backend;
mod cursor;
mod input;
mod terminal;
mod wayland;

use backend::CLEAR_COLOR;
use backend::Renderable;
use backend::tty::TtyBackend;
use cursor::CursorImage;
use input::Keyboard;
use input::keybinds::BackendKind;
use input::pointer::Pointer;
use wayland::{ClientState, WaylandState};

/// Mirrors niri's `RedrawState` (`niri/src/niri.rs`) - structurally the same
/// machine old halley's `TtyRedrawState` also independently arrived at,
/// which is a strong signal this is the actually-necessary shape for DRM
/// correctness, not incidental complexity. DRM only produces a VBlank event
/// in response to a page flip it was actually asked to do, so a scene that
/// settles into "nothing changed" naturally stops generating any further
/// VBlank at all - and the kernel occasionally just drops a promised VBlank
/// notification outright (a known amdgpu quirk area, per this project's own
/// freeze history). This tracks whether a redraw is owed and whether one is
/// already in flight, so a request is never silently lost either way.
#[derive(Debug, Default)]
enum RedrawState {
    #[default]
    Idle,
    /// A redraw was requested and nothing is in flight.
    Queued,
    /// A frame was submitted; waiting for its VBlank. `redraw_needed`
    /// remembers whether another redraw was requested since.
    WaitingForVBlank { redraw_needed: bool },
    /// The last redraw attempt submitted nothing (no damage, or an error),
    /// so a timer was armed for the estimated next VBlank instead of
    /// waiting on a real one that might never come.
    WaitingForEstimatedVBlank(RegistrationToken),
    /// A redraw was requested on top of the above.
    WaitingForEstimatedVBlankAndQueued(RegistrationToken),
}

impl RedrawState {
    fn queue_redraw(self) -> Self {
        match self {
            RedrawState::Idle => RedrawState::Queued,
            RedrawState::WaitingForEstimatedVBlank(token) => {
                RedrawState::WaitingForEstimatedVBlankAndQueued(token)
            }
            // A redraw is already queued, one way or another.
            value @ (RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued(_)) => value,
            RedrawState::WaitingForVBlank { .. } => {
                RedrawState::WaitingForVBlank { redraw_needed: true }
            }
        }
    }
}

/// Mirrors `bin/halley-winit.rs`'s `App` shape (backend + keyboard + pointer
/// + cursor + wayland/seat_state/seat). Still a separate struct in a
/// separate binary; full winit/tty unification is later, explicitly-deferred
/// work.
/// `loop_signal`/`redraw_state` replace winit's `App`'s plain `exit: bool` +
/// direct `render()` calls - the tty backend's redraw needs real scheduling
/// (see `RedrawState`), which `event_loop.run()` + `LoopSignal` (matching
/// smallvil's and niri's own structure) is built to support cleanly.
struct TtyApp {
    backend: TtyBackend,
    keyboard: Keyboard,
    pointer: Pointer,
    cursor: CursorImage,
    loop_signal: LoopSignal,
    wayland: WaylandState,
    seat_state: SeatState<TtyApp>,
    seat: Seat<TtyApp>,
    start_time: Instant,
    redraw_state: RedrawState,
    /// Whether a VT switch away is currently in effect - `redraw()` must
    /// not attempt to render while DRM master is dropped.
    paused: bool,
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
    let loop_signal = event_loop.get_signal();
    let loop_handle = event_loop.handle();

    let display: Display<TtyApp> = Display::new().expect("failed to create wayland display");
    let dh = display.handle();

    let compositor_state = CompositorState::new::<TtyApp>(&dh);
    let xdg_shell_state = XdgShellState::new::<TtyApp>(&dh);
    let xdg_decoration_state = XdgDecorationState::new::<TtyApp>(&dh);
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
        loop_signal,
        wayland: WaylandState::new(
            dh,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
        ),
        seat_state,
        seat,
        start_time: Instant::now(),
        redraw_state: RedrawState::default(),
        paused: false,
    };
    app.wayland
        .space
        .map_output(app.backend.primary_output(), (0, 0));

    let socket_name = init_wayland_listener(display, &mut event_loop);
    println!("wayland socket ready, WAYLAND_DISPLAY={socket_name:?}");

    // First frame: queue then perform directly, matching redraw()'s own
    // invariant (only ever called on Queued/WaitingForEstimatedVBlankAndQueued)
    // rather than special-casing "the very first one".
    queue_redraw(&mut app);
    redraw(&mut app, &loop_handle);

    event_loop
        .handle()
        .insert_source(libinput_backend, move |event, _, app| {
            let output_size = app.backend.output_size().to_logical(1);
            app.pointer.process_input_event(&event, output_size);
            queue_redraw(app);

            match app.keyboard.process_input_event(event) {
                Some(halley_config::Action::Quit) => app.loop_signal.stop(),
                Some(halley_config::Action::OpenTerminal) => match app.keyboard.terminal_command() {
                    Some(command) => terminal::spawn_detached(command, &socket_name),
                    None => println!("mod+t: no terminal configured or found on PATH"),
                },
                _ => {}
            }
        })
        .expect("failed to insert libinput source");

    event_loop
        .handle()
        .insert_source(session_notifier, {
            let loop_handle = loop_handle.clone();
            move |event, _, app| match event {
                SessionEvent::PauseSession => {
                    println!("session event: pause");
                    app.paused = true;
                    app.backend.pause();
                }
                SessionEvent::ActivateSession => {
                    println!("session event: activate");
                    app.paused = false;
                    match app.backend.resume() {
                        // The whole DRM pipeline (and any frame that was in
                        // flight before the switch away) is gone - reset
                        // clean rather than trusting whatever redraw_state
                        // said before the switch.
                        Ok(()) => reset_redraw_state(app, &loop_handle),
                        Err(err) => println!("resume failed: {err}"),
                    }
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
                // Secondary outputs never redraw beyond their first frame
                // (see TtyBackend::render's doc comment) - only the primary
                // output's VBlank cadence drives redraw_state.
                if crtc != app.backend.primary_crtc() {
                    return;
                }

                // Lets mapped clients know their last commit was actually
                // presented, so they schedule their next frame - regardless
                // of whether we redraw again ourselves.
                let output = app.backend.primary_output().clone();
                let elapsed = app.start_time.elapsed();
                app.wayland.space.elements().for_each(|window| {
                    window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                        Some(output.clone())
                    });
                });
                app.wayland.space.refresh();

                let redraw_needed = match std::mem::take(&mut app.redraw_state) {
                    RedrawState::WaitingForVBlank { redraw_needed } => redraw_needed,
                    // Can happen when resuming from a VT switch, or a rogue
                    // VBlank the kernel wasn't asked for - redraw defensively
                    // rather than silently dropping the frame.
                    other => {
                        println!(
                            "unexpected redraw state on vblank (expected WaitingForVBlank): {other:?}"
                        );
                        true
                    }
                };

                if redraw_needed {
                    queue_redraw(app);
                }
            }
            DrmEvent::Error(err) => println!("drm event: error {err:?}"),
        })
        .expect("failed to insert drm notifier");

    println!("dispatching - switch to this VT to see a solid color fill the screen, press the Quit chord to exit");
    event_loop
        .run(None, &mut app, |app| {
            if !app.paused
                && matches!(
                    app.redraw_state,
                    RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued(_)
                )
            {
                redraw(app, &loop_handle);
            }
            let _ = app.wayland.display_handle.flush_clients();
        })
        .expect("event loop run failed");
    println!("quit requested, exiting cleanly");
}

/// Pure state transition - safe to call from any input/event handler with
/// no event-loop access. The actual rendering only ever happens in
/// `redraw()`, called from the `run()` tail once a redraw is actually
/// `Queued`.
fn queue_redraw(app: &mut TtyApp) {
    app.redraw_state = std::mem::take(&mut app.redraw_state).queue_redraw();
}

/// Performs one redraw attempt and updates `redraw_state` accordingly.
/// Callers must only invoke this when `redraw_state` is `Queued` or
/// `WaitingForEstimatedVBlankAndQueued` (mirrors niri's own `Niri::redraw`
/// invariant) - `queue_redraw` followed by this is how every redraw in this
/// binary happens, including the very first one.
fn redraw(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    let cursor_position = app.pointer.position();
    if let Err(err) = app
        .backend
        .render(CLEAR_COLOR, &app.cursor, cursor_position, &app.wayland.space)
    {
        println!("render failed: {err}");
    }

    if app.backend.primary_frame_in_flight() {
        // A frame was actually queued to DRM - a real VBlank is coming, so
        // drop any estimated-vblank timer we might have been relying on
        // instead.
        if let RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) = std::mem::take(&mut app.redraw_state)
        {
            loop_handle.remove(token);
        }
        app.redraw_state = RedrawState::WaitingForVBlank {
            redraw_needed: false,
        };
        return;
    }

    // Nothing was submitted (no damage, or the render failed) - arm a
    // fallback timer for the estimated next VBlank so a redraw request
    // isn't lost if a real VBlank never explains why.
    queue_estimated_vblank_timer(app, loop_handle);
}

/// Arms (or keeps) a timer for the estimated next VBlank - mirrors niri's
/// `queue_estimated_vblank_timer`. Only called when nothing was actually
/// submitted to DRM this redraw attempt.
fn queue_estimated_vblank_timer(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    match std::mem::take(&mut app.redraw_state) {
        RedrawState::Idle | RedrawState::WaitingForVBlank { .. } => {
            unreachable!("queue_estimated_vblank_timer called from an unexpected redraw state")
        }
        RedrawState::Queued => {}
        // Already waiting on a timer - keep it rather than stacking a
        // second one for the same missing VBlank.
        RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
            app.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
            return;
        }
    }

    let interval = app.backend.refresh_interval();
    let token = loop_handle
        .insert_source(Timer::from_duration(interval), |_, _, app| {
            on_estimated_vblank_timer(app);
            TimeoutAction::Drop
        })
        .expect("failed to arm estimated-vblank timer");
    app.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
}

fn on_estimated_vblank_timer(app: &mut TtyApp) {
    match std::mem::take(&mut app.redraw_state) {
        RedrawState::WaitingForEstimatedVBlank(_) => {}
        // A redraw was requested while we were waiting - the next run()
        // tail will pick it up now that we're back to Queued.
        RedrawState::WaitingForEstimatedVBlankAndQueued(_) => {
            app.redraw_state = RedrawState::Queued;
        }
        other => {
            println!("unexpected redraw state on estimated-vblank timer: {other:?}");
        }
    }
}

/// Discards whatever `redraw_state` was (cancelling any armed timer first)
/// and starts fresh - used after resuming from a VT switch, since the
/// entire DRM pipeline (and anything that was in flight) is gone by then.
fn reset_redraw_state(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    if let RedrawState::WaitingForEstimatedVBlank(token)
    | RedrawState::WaitingForEstimatedVBlankAndQueued(token) = std::mem::take(&mut app.redraw_state)
    {
        loop_handle.remove(token);
    }
    app.redraw_state = RedrawState::Queued;
}

/// Sets up the listening socket new clients connect to, and the source that
/// actually pumps protocol requests from already-connected clients every
/// loop iteration - mirrors `bin/halley-winit.rs`'s `init_wayland_listener`.
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

impl XdgDecorationHandler for TtyApp {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        wayland::decoration::new_decoration(toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        wayland::decoration::request_mode(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        wayland::decoration::unset_mode(toplevel);
    }
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
delegate_xdg_decoration!(TtyApp);
delegate_seat!(TtyApp);
delegate_output!(TtyApp);
