use std::time::{Duration, Instant};

use calloop::EventLoop;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::{Seat, SeatState};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;

use crate::backend::winit::WinitBackend;
use crate::backend::{self, RenderRequest, Renderable};
use crate::cursor::CursorImage;
use crate::input::{Keyboard, SuppressedButtons};
use crate::input::keybinds::BackendKind;
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::ipc::OutputInfoSource;
use crate::wayland::{self, WaylandState};

use super::{Session, SessionDriver};

struct WinitDriver {
    backend: WinitBackend,
    exit: bool,
    last_camera_tick: Instant,
}

impl SessionDriver for WinitDriver {
    const BACKEND_KIND: BackendKind = BackendKind::Winit;

    fn primary_output(&self) -> &smithay::output::Output {
        self.backend.output()
    }

    fn request_redraw(&mut self, _output: Option<&smithay::output::Output>) {
        self.backend.request_redraw();
    }

    fn stop(&mut self) {
        self.exit = true;
    }
}

type App = Session<WinitDriver>;

fn apply_runtime_config(app: &mut App, config: halley_config::RuntimeConfig) {
    app.apply_common_config(&config);
}

/// Runs the nested (winit) session - a real window on the host desktop,
/// standing in for real hardware output. Used when we're not the ones
/// actually driving a display (see `detect_nested_session` in `main.rs`) or
/// when `--winit` is passed explicitly.
pub fn run() {
    let (config_path, runtime_config) = crate::config::load_initial();
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
    let layer_shell_state = WlrLayerShellState::new::<App>(&dh);
    let xdg_decoration_state = XdgDecorationState::new::<App>(&dh);
    let shm_state = ShmState::new::<App>(&dh, vec![]);
    let output_manager_state = OutputManagerState::new();
    let data_device_state = DataDeviceState::new::<App>(&dh);
    let primary_selection_state = PrimarySelectionState::new::<App>(&dh);

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

    let output_size = winit_backend.window_size();
    let mut cameras = crate::camera::OutputCameras::default();
    cameras.insert(winit_backend.output().name(), output_size);

    let mut app = App {
        driver: WinitDriver {
            backend: winit_backend,
            exit: false,
            last_camera_tick: Instant::now(),
        },
        keyboard: Keyboard::from_config(&runtime_config.keybinds, BackendKind::Winit),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorImage::load(),
        wayland: WaylandState::new(
            dh,
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
        ),
        seat_state,
        seat,
        start_time: Instant::now(),
        decorations: runtime_config.decorations,
        cameras,
        zoom: runtime_config.zoom,
        grab: crate::input::grab::Grab::None,
        resize_anchor: None,
        suppressed_buttons: SuppressedButtons::default(),
        wheel_accumulator: WheelAccumulator::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
    };
    app.wayland
        .space
        .map_output(app.driver.backend.output(), (0, 0));

    let socket_name = super::protocol::init_wayland_listener(display, &mut event_loop);
    eventline::info!("halley (winit) starting, WAYLAND_DISPLAY={socket_name:?}");

    if let Err(err) = crate::ipc::init_ipc_listener(&event_loop.handle(), |app: &App| {
        app.driver.backend.output_info()
    }) {
        eventline::error!("ipc: failed to start listener: {err}");
    }
    if let Some(path) = config_path
        && let Err(err) = crate::config::watch(&event_loop.handle(), path, apply_runtime_config)
    {
        eventline::warn!("config: failed to start watcher: {err}");
    }

    event_loop
        .handle()
        .insert_source(winit_source, move |event, _, app| match event {
            WinitEvent::CloseRequested => {
                app.driver.exit = true;
            }
            WinitEvent::Redraw => {
                let now = Instant::now();
                let target_presentation_time = crate::frame_clock::monotonic_now();
                let dt = now
                    .duration_since(app.driver.last_camera_tick)
                    .as_secs_f32();
                app.driver.last_camera_tick = now;
                let output_name = app.driver.backend.output().name();
                let view_before = app.cameras.view(&output_name);
                let mut animating = false;
                for camera in app.cameras.iter_mut() {
                    animating |= crate::input::zoom::tick(camera, &app.zoom, dt).1;
                }
                if view_before != app.cameras.view(&output_name) {
                    super::input::update_client_pointer_focus(
                        app,
                        app.start_time.elapsed().as_millis() as u32,
                    );
                    let pointer = app
                        .seat
                        .get_pointer()
                        .expect("pointer capability added at seat setup");
                    pointer.frame(app);
                }

                let window_animating = app.wayland.space.elements().any(|window| {
                    window.toplevel().is_some_and(|toplevel| {
                        app.window_open_animations
                            .is_animating(toplevel.wl_surface(), target_presentation_time)
                    })
                });
                let position = app.pointer.position();
                let output = app.driver.backend.output().clone();
                if let Err(err) = app.driver.backend.render(
                    &output,
                    RenderRequest {
                        target_presentation_time,
                        clear: backend::CLEAR_COLOR,
                        cursor: &app.cursor,
                        cursor_position: position,
                        space: &app.wayland.space,
                        focused: app.wayland.focused_window.as_ref(),
                        decorations: &app.decorations,
                        cameras: &app.cameras,
                        window_open_animations: &app.window_open_animations,
                    },
                ) {
                    eventline::error!("render failed: {err}");
                }

                // Lets clients know their last commit was actually
                // presented, so they schedule their next frame - without
                // this a client's redraw loop just stalls forever.
                let output = app.driver.backend.output().clone();
                let elapsed = app.start_time.elapsed();
                app.wayland.space.elements().for_each(|window| {
                    window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                        Some(output.clone())
                    });
                });
                wayland::layer_shell::send_frames(&output, elapsed);
                app.wayland.space.refresh();
                wayland::layer_shell::cleanup(&mut app.wayland);
                app.window_open_animations
                    .cleanup(target_presentation_time);

                if animating || window_animating {
                    app.request_redraw();
                }
            }
            WinitEvent::Resized { .. } => {
                // No separate size-tracking needed: render() re-queries
                // window_size() every call, and WinitGraphicsBackend::bind()
                // already resizes the EGL surface internally when it
                // differs from the last bound size. Just need a new frame,
                // plus the advertised wl_output mode kept in sync.
                app.driver.backend.update_output_mode();
                smithay::desktop::layer_map_for_output(app.driver.backend.output()).arrange();
                // Simplification: snap zoom and pan back to rest at the new
                // size rather than preserving the current state across a
                // resize - resizing mid-zoom/pan is a rare dev-only edge
                // case, not worth the extra math.
                let output_size = app.driver.backend.window_size();
                app.cameras
                    .reset(app.driver.backend.output().name(), output_size);
                super::input::update_client_pointer_focus(
                    app,
                    app.start_time.elapsed().as_millis() as u32,
                );
                let pointer = app
                    .seat
                    .get_pointer()
                    .expect("pointer capability added at seat setup");
                pointer.frame(app);
                app.request_redraw();
            }
            WinitEvent::Input(event) => super::input::handle(app, &event, &socket_name),
            _ => {}
        })
        .expect("failed to insert winit event source");

    while !app.driver.exit {
        event_loop
            .dispatch(None, &mut app)
            .expect("event loop dispatch failed");
        let _ = app.wayland.display_handle.flush_clients();
    }
}
