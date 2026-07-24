use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode as CalloopMode, PostAction};
use smithay::backend::input::{Event, InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::keyboard::FilterResult;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
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

use crate::backend::winit::WinitBackend;
use crate::backend::{self, Renderable};
use crate::cursor::CursorImage;
use crate::input::Keyboard;
use crate::input::keybinds::BackendKind;
use crate::input::match_bind;
use crate::input::pointer::Pointer;
use crate::terminal;
use crate::wayland::{self, ClientState, WaylandState};

/// Everything this milestone needs: a backend to render into, a way to match
/// keypresses against configured actions, a cursor image to draw plus where
/// to draw it, whether we should stop, and (new) the Wayland protocol state
/// a client needs to connect and show a window. Deliberately not the old
/// `Halley` mega-struct - grows exactly when a real feature needs it to;
/// `wayland`/`seat_state`/`seat` are one field each, added for this one
/// concrete reason, same as `cursor`/`keyboard`/`pointer` before them.
struct App {
    backend: WinitBackend,
    keyboard: Keyboard,
    pointer: Pointer,
    cursor: CursorImage,
    exit: bool,
    wayland: WaylandState,
    seat_state: SeatState<App>,
    seat: Seat<App>,
    start_time: Instant,
    decorations: halley_config::Decorations,
}

/// Runs the nested (winit) session - a real window on the host desktop,
/// standing in for real hardware output. Used when we're not the ones
/// actually driving a display (see `detect_nested_session` in `main.rs`) or
/// when `--winit` is passed explicitly.
pub fn run() {
    let window_attributes = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_title("halley");

    let (backend, winit_source) =
        smithay_winit::init_from_attributes::<GlesRenderer>(window_attributes)
            .expect("failed to initialize winit backend");

    // Winit only guarantees an initial RedrawRequested on some platforms -
    // request one explicitly so the first frame doesn't depend on that.
    backend.window().request_redraw();

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("failed to create event loop");

    let display: Display<App> = Display::new().expect("failed to create wayland display");
    let dh = display.handle();

    let compositor_state = CompositorState::new::<App>(&dh);
    let xdg_shell_state = XdgShellState::new::<App>(&dh);
    let xdg_decoration_state = XdgDecorationState::new::<App>(&dh);
    let shm_state = ShmState::new::<App>(&dh, vec![]);
    let output_manager_state = OutputManagerState::new();

    let mut seat_state = SeatState::new();
    let mut seat: Seat<App> = seat_state.new_wl_seat(&dh, "seat0");
    // Advertises capabilities and creates the wl_keyboard/wl_pointer
    // protocol objects clients expect a seat to offer - not real input
    // forwarding, which stays deferred (see `seat`'s doc comment above).
    seat.add_keyboard(Default::default(), 200, 25)
        .expect("failed to advertise keyboard capability on the wl_seat");
    seat.add_pointer();

    let winit_backend = WinitBackend::new(backend);
    let _output_global = winit_backend.output().create_global::<App>(&dh);

    let mut app = App {
        backend: winit_backend,
        keyboard: Keyboard::new(BackendKind::Winit),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorImage::load(),
        exit: false,
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
        decorations: halley_config::load_decorations(),
    };
    app.wayland.space.map_output(app.backend.output(), (0, 0));

    let socket_name = init_wayland_listener(display, &mut event_loop);
    println!("halley (winit) starting, WAYLAND_DISPLAY={socket_name:?}");

    event_loop
        .handle()
        .insert_source(winit_source, move |event, _, app| match event {
            WinitEvent::CloseRequested => {
                app.exit = true;
            }
            WinitEvent::Redraw => {
                let position = app.pointer.position();
                if let Err(err) = app.backend.render(
                    backend::CLEAR_COLOR,
                    &app.cursor,
                    position,
                    &app.wayland.space,
                    app.wayland.focused.as_ref(),
                    &app.decorations,
                ) {
                    eprintln!("render failed: {err}");
                }

                // Lets clients know their last commit was actually
                // presented, so they schedule their next frame - without
                // this a client's redraw loop just stalls forever.
                let output = app.backend.output().clone();
                let elapsed = app.start_time.elapsed();
                app.wayland.space.elements().for_each(|window| {
                    window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                        Some(output.clone())
                    });
                });
                app.wayland.space.refresh();

                app.backend.request_redraw();
            }
            WinitEvent::Resized { .. } => {
                // No separate size-tracking needed: render() re-queries
                // window_size() every call, and WinitGraphicsBackend::bind()
                // already resizes the EGL surface internally when it
                // differs from the last bound size. Just need a new frame,
                // plus the advertised wl_output mode kept in sync.
                app.backend.update_output_mode();
                app.backend.request_redraw();
            }
            WinitEvent::Input(event) => {
                let output_size = app.backend.window_size().to_logical(1);
                app.pointer.process_input_event(&event, output_size);
                // Motion alone doesn't trigger a Redraw - request one so the
                // cursor visibly follows the mouse instead of only moving on
                // the next unrelated redraw.
                app.backend.request_redraw();

                // Drives the real seat directly (rather than a separate fake
                // one) so that whatever isn't intercepted as a configured
                // bind (`FilterResult::Forward`) actually reaches the
                // focused client - Smithay's own `KeyboardTarget<D> for
                // WlSurface` impl handles that forwarding for free once
                // `App::KeyboardFocus = WlSurface`.
                if let InputEvent::Keyboard { event: key_event } = event {
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
                        Some(halley_config::Action::Quit) => app.exit = true,
                        Some(halley_config::Action::OpenTerminal) => match app.keyboard.terminal_command() {
                            Some(command) => terminal::spawn_detached(command, &socket_name),
                            None => eprintln!("mod+t: no terminal configured or found on PATH"),
                        },
                        _ => {}
                    }
                }
            }
            _ => {}
        })
        .expect("failed to insert winit event source");

    while !app.exit {
        event_loop
            .dispatch(None, &mut app)
            .expect("event loop dispatch failed");
        let _ = app.wayland.display_handle.flush_clients();
    }
}

/// Sets up the listening socket new clients connect to, and the source that
/// actually pumps protocol requests from already-connected clients every
/// loop iteration - without the latter, `Display<App>` just accumulates
/// requests nobody reads.
fn init_wayland_listener(display: Display<App>, event_loop: &mut EventLoop<App>) -> OsString {
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

impl CompositorHandler for App {
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

impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.wayland.shm_state
    }
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.wayland.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        wayland::xdg_shell::new_toplevel(&mut self.wayland, surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        wayland::xdg_shell::toplevel_destroyed(&mut self.wayland, &surface);
    }

    // Popups are required by the trait (no default body) but not supported
    // this round - a client that opens one just never sees it appear. See
    // the plan's explicit scope trim.
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

impl XdgDecorationHandler for App {
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

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

impl OutputHandler for App {}

delegate_compositor!(App);
delegate_shm!(App);
delegate_xdg_shell!(App);
delegate_xdg_decoration!(App);
delegate_seat!(App);
delegate_output!(App);
