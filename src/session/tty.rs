use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode as CalloopMode, PostAction, RegistrationToken};
use smithay::backend::drm::DrmEvent;
use smithay::backend::input::{ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::input::keyboard::FilterResult;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_primary_selection,
    delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
};

use crate::backend::tty::TtyBackend;
use crate::backend::{CLEAR_COLOR, Renderable};
use crate::cursor::CursorImage;
use crate::input::Keyboard;
use crate::input::keybinds::BackendKind;
use crate::input::match_bind;
use crate::input::pointer::Pointer;
use crate::ipc::OutputInfoSource;
use crate::terminal;
use crate::wayland::{self, ClientState, WaylandState};

/// From `<linux/input-event-codes.h>` - the left mouse button's raw code,
/// used instead of `PointerButtonEvent::button()`'s `MouseButton` enum
/// because libinput's own event type has an inherent `button()` returning a
/// raw `u32` that shadows the trait method of the same name.
const BTN_LEFT: u32 = 0x110;

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

/// Mirrors `session::winit`'s `App` shape (backend + keyboard + pointer +
/// cursor + wayland/seat_state/seat). Still a separate struct in a separate
/// session module; full winit/tty unification is later, explicitly-deferred
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
    decorations: halley_config::Decorations,
    redraw_state: RedrawState,
    /// Whether a VT switch away is currently in effect - `redraw()` must
    /// not attempt to render while DRM master is dropped.
    paused: bool,
    camera: halley_core::camera::Camera,
    zoom: halley_config::Zoom,
    last_camera_tick: Instant,
    grab: crate::input::grab::Grab,
}

/// Runs the real-hardware (DRM/KMS) session - takes over the seat and a
/// free VT. Returns (rather than panicking) if `TtyBackend::new()` fails,
/// since that's expected when nested under a host compositor that already
/// holds exclusive session control.
pub fn run() {
    let (backend, session_notifier, drm_notifier) = match TtyBackend::new() {
        Ok(parts) => parts,
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
    let data_device_state = DataDeviceState::new::<TtyApp>(&dh);
    let primary_selection_state = PrimarySelectionState::new::<TtyApp>(&dh);

    let mut seat_state = SeatState::new();
    let mut seat: Seat<TtyApp> = seat_state.new_wl_seat(&dh, "seat0");
    seat.add_keyboard(Default::default(), 200, 25)
        .expect("failed to advertise keyboard capability on the wl_seat");
    seat.add_pointer();

    let _output_global = backend.primary_output().create_global::<TtyApp>(&dh);

    let output_size = backend.output_size();
    let camera = halley_core::camera::Camera::new(
        // Starts centered on the output - the baseline pan offsets are
        // measured from, and exactly where a never-panned camera needs to
        // sit so rendering/hit-testing reduce to "no pan" correctly.
        halley_core::field::Vec2 {
            x: output_size.w as f32 / 2.0,
            y: output_size.h as f32 / 2.0,
        },
        halley_core::field::Vec2 {
            x: output_size.w as f32,
            y: output_size.h as f32,
        },
    );

    let mut app = TtyApp {
        backend,
        keyboard: Keyboard::new(BackendKind::Tty),
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
            data_device_state,
            primary_selection_state,
        ),
        seat_state,
        seat,
        start_time: Instant::now(),
        decorations: halley_config::load_decorations(),
        redraw_state: RedrawState::default(),
        paused: false,
        camera,
        zoom: halley_config::load_zoom(),
        last_camera_tick: Instant::now(),
        grab: crate::input::grab::Grab::None,
    };
    app.wayland
        .space
        .map_output(app.backend.primary_output(), (0, 0));

    let socket_name = init_wayland_listener(display, &mut event_loop);
    println!("wayland socket ready, WAYLAND_DISPLAY={socket_name:?}");

    if let Err(err) = crate::ipc::init_ipc_listener(&event_loop.handle(), |app: &TtyApp| app.backend.output_info()) {
        eprintln!("ipc: failed to start listener: {err}");
    }

    // First frame: queue then perform directly, matching redraw()'s own
    // invariant (only ever called on Queued/WaitingForEstimatedVBlankAndQueued)
    // rather than special-casing "the very first one".
    queue_redraw(&mut app);
    redraw(&mut app, &loop_handle);

    event_loop
        .handle()
        .insert_source(libinput_backend, move |event, _, app| {
            let output_size = app.backend.output_size().to_logical(1);
            let output_size_physical = app.backend.output_size();
            let position_before = app.pointer.position();
            app.pointer.process_input_event(&event, output_size);
            let position_after = app.pointer.position();
            queue_redraw(app);

            // Apply whatever's being dragged, if anything - reuses the
            // delta `Pointer` already computed (handles relative and
            // absolute motion, and clamping, uniformly) rather than
            // re-deriving it from the raw event per grab kind.
            match &app.grab {
                crate::input::grab::Grab::MoveWindow { window, offset } => {
                    let world = crate::input::grab::screen_to_world(position_after, &app.camera, output_size_physical);
                    let new_location = Point::<i32, Logical>::from((
                        (world.x + offset.x).round() as i32,
                        (world.y + offset.y).round() as i32,
                    ));
                    app.wayland.space.map_element(window.clone(), new_location, false);
                }
                crate::input::grab::Grab::Pan => {
                    let dx = position_after.0 - position_before.0;
                    let dy = position_after.1 - position_before.1;
                    let delta = crate::input::grab::screen_delta_to_world(dx, dy, &app.camera);
                    // Negated - content follows the cursor ("natural drag"),
                    // matching old halley's own pan convention.
                    app.camera.pan_target(halley_core::field::Vec2 { x: -delta.x, y: -delta.y });
                }
                crate::input::grab::Grab::None => {}
            }

            if let InputEvent::PointerButton { event: button_event } = &event
                && button_event.button_code() == BTN_LEFT
            {
                match button_event.state() {
                    ButtonState::Pressed => {
                        let mods = app
                            .seat
                            .get_keyboard()
                            .expect("keyboard capability added at seat setup")
                            .modifier_state();
                        let mod_held = crate::input::mod_key_held(&mods, app.keyboard.effective_mod);
                        let world =
                            crate::input::grab::screen_to_world(position_after, &app.camera, output_size_physical);
                        match crate::input::grab::window_under(&app.wayland.space, world) {
                            Some((window, window_loc)) if mod_held => {
                                let offset = halley_core::field::Vec2 {
                                    x: window_loc.x as f32 - world.x,
                                    y: window_loc.y as f32 - world.y,
                                };
                                wayland::xdg_shell::focus_and_raise(&mut app.wayland, &window);
                                app.grab = crate::input::grab::Grab::MoveWindow { window, offset };
                            }
                            Some((window, _)) => {
                                wayland::xdg_shell::focus_and_raise(&mut app.wayland, &window);
                            }
                            None => {
                                app.grab = crate::input::grab::Grab::Pan;
                            }
                        }
                    }
                    ButtonState::Released => {
                        app.grab = crate::input::grab::Grab::None;
                    }
                }
            }

            // Drives the real seat directly (rather than a separate fake
            // one) so that whatever isn't intercepted as a configured bind
            // (`FilterResult::Forward`) actually reaches the focused client
            // - Smithay's own `KeyboardTarget<D> for WlSurface` impl handles
            // that forwarding for free once `App::KeyboardFocus = WlSurface`.
            if let InputEvent::Keyboard { event: key_event } = &event {
                let keycode = key_event.key_code();
                let state = key_event.state();
                let time = key_event.time_msec();
                let keyboard = app
                    .seat
                    .get_keyboard()
                    .expect("keyboard capability added at seat setup");
                let action = keyboard.input::<halley_config::Action, _>(
                    app,
                    keycode,
                    state,
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |data, mods, handle| {
                        if state != KeyState::Pressed {
                            return FilterResult::Forward;
                        }
                        let Some(keysym) = handle.raw_latin_sym_or_raw_current_sym() else {
                            return FilterResult::Forward;
                        };
                        match match_bind(&data.keyboard.binds, mods, keysym) {
                            Some(action) => FilterResult::Intercept(action),
                            None => FilterResult::Forward,
                        }
                    },
                );

                match action {
                    Some(halley_config::Action::Quit) => app.loop_signal.stop(),
                    Some(halley_config::Action::CloseFocusedWindow) => {
                        wayland::xdg_shell::close_focused(&app.wayland);
                    }
                    Some(halley_config::Action::OpenTerminal) => match app.keyboard.terminal_command() {
                        Some(command) => terminal::spawn_detached(command, &socket_name),
                        None => println!("mod+t: no terminal configured or found on PATH"),
                    },
                    Some(halley_config::Action::ZoomOut) => {
                        crate::input::zoom::zoom_out(&mut app.camera, &app.zoom);
                    }
                    Some(halley_config::Action::ZoomIn) => {
                        crate::input::zoom::zoom_in(&mut app.camera, &app.zoom);
                    }
                    Some(halley_config::Action::ZoomReset) => app.camera.reset_zoom_target(),
                    _ => {}
                }
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
/// session happens, including the very first one.
fn redraw(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    let now = Instant::now();
    let dt = now.duration_since(app.last_camera_tick).as_secs_f32();
    app.last_camera_tick = now;
    let (zoom_scale, animating) = crate::input::zoom::tick(&mut app.camera, &app.zoom, dt);
    let camera_center = Point::<f32, Physical>::from((app.camera.center.x, app.camera.center.y));

    let cursor_position = app.pointer.position();
    if let Err(err) = app.backend.render(
        CLEAR_COLOR,
        &app.cursor,
        cursor_position,
        &app.wayland.space,
        app.wayland.focused.as_ref(),
        &app.decorations,
        camera_center,
        zoom_scale,
    ) {
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
            // While the zoom camera is still easing toward its target,
            // request another redraw once this frame's VBlank lands -
            // otherwise the damage-driven scheduler stops redrawing after a
            // single frame and the animation freezes mid-ease.
            redraw_needed: animating,
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
/// loop iteration - mirrors `session::winit`'s `init_wayland_listener`.
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

        // Re-asserted every commit rather than only when it changes -
        // `set_focus` no-ops internally when the focus is already what's
        // requested, so this is cheap and avoids needing separate
        // change-tracking on top of `wayland.focused`.
        let focused = self.wayland.focused.clone();
        let keyboard = self
            .seat
            .get_keyboard()
            .expect("keyboard capability added at seat setup");
        keyboard.set_focus(self, focused, SERIAL_COUNTER.next_serial());
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

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = self.wayland.display_handle.clone();
        crate::wayland::selection::sync_selection_focus(&dh, seat, focused);
    }
}

impl OutputHandler for TtyApp {}

/// See `App`'s own impl (`session::winit`) - `()` for the same reason: no
/// compositor-owned selections exist yet for user data to hang off.
impl SelectionHandler for TtyApp {
    type SelectionUserData = ();
}

impl DataDeviceHandler for TtyApp {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.wayland.data_device_state
    }
}

impl WaylandDndGrabHandler for TtyApp {
    fn dnd_requested<S: smithay::input::dnd::Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: smithay::input::dnd::GrabType,
    ) {
        let dh = self.wayland.display_handle.clone();
        crate::wayland::selection::start_dnd_grab(self, &dh, source, seat, serial, type_);
    }
}

impl smithay::input::dnd::DndGrabHandler for TtyApp {}

impl PrimarySelectionHandler for TtyApp {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.wayland.primary_selection_state
    }
}

delegate_compositor!(TtyApp);
delegate_shm!(TtyApp);
delegate_xdg_shell!(TtyApp);
delegate_xdg_decoration!(TtyApp);
delegate_seat!(TtyApp);
delegate_output!(TtyApp);
delegate_data_device!(TtyApp);
delegate_primary_selection!(TtyApp);
