use std::time::{Duration, Instant};

use calloop::EventLoop;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::{Seat, SeatState};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::wayland::seat::WaylandFocus;

use crate::backend::winit::WinitBackend;
use crate::backend::{self, RenderRequest, Renderable};
use crate::cursor::CursorManager;
use crate::input::keybinds::BackendKind;
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::input::{Keyboard, SuppressedButtons, SuppressedKeys};
use crate::wayland;

use super::{Session, SessionDriver};

struct WinitDriver {
    backend: WinitBackend,
    exit: bool,
    last_camera_tick: Instant,
}

impl crate::ipc::OutputInfoSource for WinitDriver {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo> {
        crate::ipc::OutputInfoSource::output_info(&self.backend)
    }
}

impl SessionDriver for WinitDriver {
    const BACKEND_KIND: BackendKind = BackendKind::Winit;

    fn primary_output(&self) -> &smithay::output::Output {
        self.backend.output()
    }

    fn dmabuf_capabilities(&mut self) -> crate::backend::dmabuf::DmabufCapabilities {
        self.backend.dmabuf_capabilities()
    }

    fn import_dmabuf(&mut self, dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.backend.import_dmabuf(dmabuf)
    }

    fn dmabuf_feedback(
        &self,
        _output: &smithay::output::Output,
    ) -> Option<&crate::backend::dmabuf::SurfaceDmabufFeedback> {
        None
    }

    fn request_redraw(&mut self, _output: Option<&smithay::output::Output>) {
        self.backend.request_redraw();
    }

    fn with_renderer<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> T {
        f(self.backend.renderer())
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

    let mut seat_state = SeatState::new();
    let mut seat: Seat<App> = seat_state.new_wl_seat(&dh, "seat0");
    // Advertises capabilities and creates the wl_keyboard/wl_pointer
    // protocol objects clients expect a seat to offer - not real input
    // forwarding, which stays deferred (see `seat`'s doc comment above).
    let applied_keyboard = crate::input::config::add_keyboard(&mut seat, &runtime_config.input)
        .expect("failed to advertise keyboard capability on the wl_seat");
    seat.add_pointer();

    let winit_backend = WinitBackend::new(backend);
    let _output_global = winit_backend.output().create_global::<App>(&dh);

    let output_size = winit_backend.window_size();
    let mut cameras = crate::camera::OutputCameras::default();
    cameras.insert(winit_backend.output().name(), output_size);

    let mut driver = WinitDriver {
        backend: winit_backend,
        exit: false,
        last_camera_tick: Instant::now(),
    };
    let wayland = App::create_wayland_state(dh.clone(), &mut driver);
    let xwayland = crate::xwayland::State::new::<App>(&dh);
    let mut applied_input = runtime_config.input.clone();
    applied_input.keyboard = applied_keyboard;
    let launch_environment = super::environment::LaunchEnvironment::new(&runtime_config.env);
    let launch_path = launch_environment.path();
    let mut app = App {
        driver,
        keyboard: Keyboard::from_config(
            &runtime_config.keybinds,
            BackendKind::Winit,
            launch_path.as_deref(),
        ),
        launch_environment,
        autostart: super::autostart::Autostart::disabled(),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorManager::new(&runtime_config.cursor),
        cursor_policy: super::cursor::Policy::new(&runtime_config.cursor, event_loop.handle()),
        publish_session_environment: false,
        wayland,
        seat_state,
        seat,
        start_time: Instant::now(),
        input: applied_input,
        decorations: runtime_config.decorations,
        cameras,
        zoom: runtime_config.zoom,
        screenshot: runtime_config.screenshot,
        capture: crate::capture::CaptureState::default(),
        screencast: crate::screencast::ScreencastState::default(),
        grab: crate::input::grab::Grab::None,
        resize_anchor: None,
        suppressed_buttons: SuppressedButtons::default(),
        suppressed_keys: SuppressedKeys::default(),
        wheel_accumulator: WheelAccumulator::default(),
        touch: super::touch::TouchState::default(),
        gestures: super::gesture::GestureState::default(),
        pointer_constraints: super::pointer::PointerConstraintLifecycle::default(),
        keyboard_monitor: None,
        opening_origins: super::opening::OpeningOrigins::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
        window_close_animations: crate::backend::close::WindowCloseAnimations::new(
            runtime_config.animations,
        ),
        fullscreen: crate::wayland::fullscreen::FullscreenManager::new(runtime_config.animations),
        fullscreen_textures:
            crate::backend::fullscreen_texture::FullscreenTextureTransitions::default(),
        xwayland,
    };
    app.wayland
        .space
        .map_output(app.driver.backend.output(), (0, 0));

    let socket_name = super::protocol::init_wayland_listener(display, &mut event_loop);
    eventline::info!("halley (winit) starting, WAYLAND_DISPLAY={socket_name:?}");
    if let Err(err) = crate::xwayland::start(&event_loop.handle(), &mut app, false) {
        eventline::warn!("xwayland: unavailable: {err}");
    }

    if let Err(err) =
        crate::ipc::init_ipc_listener(&event_loop.handle(), |app: &mut App, request| {
            crate::ipc::handle_request(app, request);
        })
    {
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
                    animating |= crate::input::zoom::tick(
                        camera,
                        &app.zoom,
                        app.input.gestures.pan_decay_rate,
                        dt,
                    )
                    .1;
                }
                if view_before != app.cameras.view(&output_name) {
                    super::pointer::update_client_state(
                        app,
                        app.start_time.elapsed().as_millis() as u32,
                    );
                }

                let window_animating = app.wayland.space.elements().any(|window| {
                    window.wl_surface().is_some_and(|surface| {
                        app.window_open_animations
                            .is_animating(surface.as_ref(), target_presentation_time)
                    })
                });
                let output = app.driver.backend.output().clone();
                let fullscreen_animating = app
                    .fullscreen
                    .is_animating_on_output(&output, target_presentation_time);
                let closing_animating = app
                    .window_close_animations
                    .is_animating_on_output(&output, target_presentation_time);
                if fullscreen_animating {
                    super::pointer::update_client_state(
                        app,
                        app.start_time.elapsed().as_millis() as u32,
                    );
                }
                let position = app.pointer.position();
                let show_cursor = super::pointer::cursor_visible(app);
                crate::cursor::surface::refresh_outputs(&app.cursor, &app.wayland.space, position);
                let cursor_animating = show_cursor
                    && app
                        .cursor
                        .current_is_animated(output.current_scale().integer_scale());
                if let Err(err) = app.driver.backend.render(
                    &output,
                    RenderRequest {
                        target_presentation_time,
                        clear: backend::CLEAR_COLOR,
                        cursor: &app.cursor,
                        cursor_position: position,
                        show_cursor,
                        capture_overlay: app.capture.overlay(),
                        space: &app.wayland.space,
                        focused: app.wayland.focused_window.as_ref(),
                        decorations: &app.decorations,
                        cameras: &app.cameras,
                        window_open_animations: &app.window_open_animations,
                        window_close_animations: &mut app.window_close_animations,
                        fullscreen: &app.fullscreen,
                        fullscreen_textures: &mut app.fullscreen_textures,
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
                crate::cursor::surface::send_frame(
                    &app.cursor,
                    &app.wayland.space,
                    &output,
                    app.pointer.position(),
                    elapsed,
                );
                app.wayland.space.refresh();
                wayland::layer_shell::cleanup(&mut app.wayland);
                app.window_open_animations.cleanup(target_presentation_time);
                app.window_close_animations
                    .cleanup(target_presentation_time);
                if app.cleanup_fullscreen(target_presentation_time) {
                    super::sync_keyboard_focus(app, smithay::utils::SERIAL_COUNTER.next_serial());
                    super::pointer::update_client_state(
                        app,
                        app.start_time.elapsed().as_millis() as u32,
                    );
                }

                if animating
                    || window_animating
                    || closing_animating
                    || fullscreen_animating
                    || cursor_animating
                {
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
                let external = app
                    .fullscreen
                    .reconfigure_output(&app.wayland, app.driver.backend.output());
                crate::xwayland::reconfigure_fullscreen(external);
                super::pointer::update_client_state(
                    app,
                    app.start_time.elapsed().as_millis() as u32,
                );
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
