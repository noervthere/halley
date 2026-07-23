mod backend;
mod cursor;
mod input;
mod wayland;

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode as CalloopMode, PostAction};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
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

use backend::Renderable;
use backend::winit::WinitBackend;
use cursor::CursorImage;
use input::Keyboard;
use input::keybinds::BackendKind;
use input::pointer::Pointer;
use wayland::{ClientState, WaylandState};

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
    /// A second, unrelated `Seat` from the one `Keyboard` already owns
    /// internally - that one turns key events into `Action`s and has no
    /// relationship to any `Display`. This one exists purely to advertise
    /// `wl_seat` capabilities to clients; nothing feeds it real events yet.
    seat: Seat<App>,
    start_time: Instant,
}

fn main() {
    let window_attributes = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_title("halley-wl");

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
        keyboard: Keyboard::new(BackendKind::Winit).expect("failed to set up keyboard input"),
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
    app.wayland.space.map_output(app.backend.output(), (0, 0));

    let socket_name = init_wayland_listener(display, &mut event_loop);
    println!("halley-wl starting, WAYLAND_DISPLAY={socket_name:?}");

    event_loop
        .handle()
        .insert_source(winit_source, move |event, _, app| match event {
            WinitEvent::CloseRequested => {
                app.exit = true;
            }
            WinitEvent::Redraw => {
                let position = app.pointer.position();
                if let Err(err) = app.backend.render(backend::CLEAR_COLOR, &app.cursor, position) {
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
                if let Some(halley_config::Action::Quit) = app.keyboard.process_input_event(event) {
                    app.exit = true;
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
delegate_seat!(App);
delegate_output!(App);
