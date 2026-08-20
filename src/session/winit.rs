use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::{Seat, SeatState};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::wayland::seat::WaylandFocus;

use crate::backend::winit::WinitBackend;
use crate::backend::{self, Renderable};
use crate::cursor::CursorManager;
use crate::input::Keyboard;
use crate::input::keybinds::BackendKind;
use crate::input::pointer::Pointer;
use crate::render::{
    self, CursorContext, DesktopContext, FrameContext, OverlayContext, RenderRequest, VisualContext,
};
use crate::wayland;

use super::Session;

struct WinitDriver {
    backend: WinitBackend,
    loop_handle: LoopHandle<'static, App>,
    exit: bool,
    last_camera_tick: Instant,
}

impl crate::ipc::OutputInfoSource for WinitDriver {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo> {
        crate::ipc::OutputInfoSource::output_info(&self.backend)
    }
}

impl super::RenderDriver for WinitDriver {
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

    fn schedule_render_completion(
        &mut self,
        sync: smithay::backend::renderer::sync::SyncPoint,
        completion: Box<dyn FnOnce() + 'static>,
    ) -> Result<(), String> {
        let Some(fence) = sync.export() else {
            completion();
            return Ok(());
        };
        let mut completion = Some(completion);
        self.loop_handle
            .insert_source(
                Generic::new(fence, Interest::READ, Mode::OneShot),
                move |_, _, _| {
                    if let Some(completion) = completion.take() {
                        completion();
                    }
                    Ok(PostAction::Remove)
                },
            )
            .map(|_| ())
            .map_err(|err| format!("failed to watch render fence: {err}"))
    }
}

impl super::OutputDriver for WinitDriver {
    fn primary_output(&self) -> &smithay::output::Output {
        self.backend.output()
    }

    fn output_states(&self) -> Vec<super::output::OutputState> {
        let output = self.backend.output();
        vec![super::output::OutputState {
            output: output.clone(),
            enabled: true,
            mode: output
                .current_mode()
                .expect("winit output always has a current mode"),
            location: output.current_location(),
            transform: output.current_transform(),
            scale: output.current_scale().fractional_scale(),
            adaptive_sync: false,
            adaptive_sync_supported: false,
        }]
    }

    fn test_output_configuration(
        &mut self,
        configuration: &[super::output::OutputConfiguration],
    ) -> Result<(), String> {
        let current = self.output_states();
        super::output::validate_complete_configuration(&current, configuration)?;
        let requested = &configuration[0];
        let state = &current[0];
        if !requested.enabled
            || requested.mode != state.mode
            || requested.location != state.location
            || requested.transform != state.transform
            || requested.adaptive_sync
        {
            return Err("the nested output is controlled by the host compositor".into());
        }
        Ok(())
    }

    fn apply_output_configuration(
        &mut self,
        configuration: &[super::output::OutputConfiguration],
    ) -> Result<Vec<super::output::OutputChange>, String> {
        self.test_output_configuration(configuration)?;
        Ok(Vec::new())
    }
}

impl super::SessionDriver for WinitDriver {
    const BACKEND_KIND: BackendKind = BackendKind::Winit;

    fn stop(&mut self) {
        self.exit = true;
    }
}

type App = Session<WinitDriver>;

fn apply_runtime_config(app: &mut App, reload: crate::config::ConfigReload) {
    match reload {
        crate::config::ConfigReload::Loaded(config) => {
            let config = *config;
            app.apply_common_config(&config);
            app.clear_config_reload_error();
        }
        crate::config::ConfigReload::Rejected(diagnostic) => {
            eventline::debug!("config: rejected reload for {:?}", diagnostic.path);
            app.show_config_reload_error();
        }
    }
}

/// Runs the nested (winit) session - a real window on the host desktop,
/// standing in for real hardware output. Used when we're not the ones
/// actually driving a display (see `detect_nested_session` in `main.rs`) or
/// when `--winit` is passed explicitly.
pub fn run(explicit_config_path: Option<std::path::PathBuf>) {
    let initial = crate::config::load_initial(explicit_config_path);
    let config_path = initial.path;
    let runtime_config = initial.config;
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

    let output_size = winit_backend.window_size();
    let mut cameras = crate::presentation::camera::OutputCameras::default();
    cameras.insert(winit_backend.output().name(), output_size);

    let mut driver = WinitDriver {
        backend: winit_backend,
        loop_handle: event_loop.handle(),
        exit: false,
        last_camera_tick: Instant::now(),
    };
    let mut wayland = App::create_wayland_state(dh.clone(), &mut driver);
    wayland.ensure_output_global::<App>(driver.backend.output());
    let idle_notifier_state =
        smithay::wayland::idle_notify::IdleNotifierState::new(&dh, event_loop.handle());
    let presentation_state = smithay::wayland::presentation::PresentationState::new::<App>(
        &dh,
        smithay::utils::Clock::<smithay::utils::Monotonic>::new().id() as u32,
    );
    let session_lock = crate::wayland::session_lock::State::new::<WinitDriver>(&dh);
    let xwayland = crate::xwayland::State::<WinitDriver>::new(&dh, event_loop.handle());
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
        idle_notifier_state,
        presentation_state,
        drm_syncobj_state: None,
        session_lock,
        start_time: Instant::now(),
        config_path: config_path.clone(),
        startup_config_diagnostic: initial.diagnostic,
        shell: crate::shell::state::ShellState::new(&runtime_config),
        settings: super::RuntimeSettings::new(&runtime_config, applied_input),
        nodes: crate::nodes::NodesState::new(&runtime_config),
        clusters: crate::clusters::ClusterSystem::new(
            runtime_config.clusters,
            runtime_config.animations.cluster,
        ),
        pending_pointer_warp: None,
        window_rules: crate::window::rules::WindowRulesState::new(
            runtime_config.window_rules.clone(),
        ),
        cameras,
        capture: crate::capture::CaptureState::default(),
        pending_captures: std::collections::HashMap::new(),
        screenshot_encoder: None,
        screencast: crate::capture::screencast::ScreencastState::default(),
        interactions: super::InteractionState::default(),
        touch: super::touch::TouchState::default(),
        gestures: super::gesture::GestureState::default(),
        window_trace: super::trace::WindowTrace::from_env(),
        keyboard_monitor: None,
        opening_origins: super::opening::OpeningOrigins::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
        render: crate::render::resources::RenderState::new(
            runtime_config.animations,
            &runtime_config.font,
        ),
        fullscreen: crate::wayland::fullscreen::FullscreenManager::new(runtime_config.animations),
        maximize: crate::presentation::maximize::FieldMaximizeManager::new(
            runtime_config.field,
            runtime_config.animations,
        ),
        xwayland,
    };
    app.wayland
        .space
        .map_output(app.driver.backend.output(), (0, 0));
    app.initialize_config_notification();

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

    match crate::capture::encoder::ScreenshotEncoder::spawn(
        &event_loop.handle(),
        |app: &mut App, done| crate::capture::finish_encode(app, done),
    ) {
        Ok(encoder) => app.screenshot_encoder = Some(encoder),
        Err(err) => eventline::error!("screenshot: failed to start encoder: {err}"),
    }
    if let Some(path) = config_path
        && let Err(err) = crate::config::watch(&event_loop.handle(), path, apply_runtime_config)
    {
        eventline::warn!("config: failed to start watcher: {err}");
    }
    if let Err(err) = super::install_node_decay_timer(&event_loop.handle()) {
        eventline::warn!("nodes: failed to start decay timer: {err}");
    }
    if let Err(err) = super::install_apogee_preview_timer(&event_loop.handle()) {
        eventline::warn!("apogee: failed to start preview timer: {err}");
    }
    if let Err(err) = super::install_overlay_timer(&event_loop.handle()) {
        eventline::warn!("overlays: failed to start lifecycle timer: {err}");
    }

    event_loop
        .handle()
        .insert_source(winit_source, move |event, _, app| match event {
            WinitEvent::CloseRequested => {
                app.show_exit_confirmation();
            }
            WinitEvent::Redraw => {
                let now = Instant::now();
                let target_presentation_time = crate::frame_clock::monotonic_now();
                let physics_animating = crate::nodes::tick_physics(app, target_presentation_time);
                let dt = now
                    .duration_since(app.driver.last_camera_tick)
                    .as_secs_f32();
                app.driver.last_camera_tick = now;
                let output = app.driver.backend.output().clone();
                let output_name = output.name();
                super::reconcile_cluster_surfaces(app, &output_name);
                let view_before = app.cameras.view(&output_name);
                let cluster_camera_changed =
                    super::sync_cluster_camera(app, &output_name, target_presentation_time);
                let mut animating = cluster_camera_changed
                    | app.sync_fullscreen_camera(&output, target_presentation_time);
                for camera in app.cameras.iter_mut() {
                    animating |= crate::input::zoom::tick(
                        camera,
                        &app.settings.zoom,
                        app.settings.input.gestures.pan_decay_rate,
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
                let _ = crate::shell::focus_cycle::finish_pending_pointer_warp(app);
                let fullscreen_animating = app
                    .fullscreen
                    .is_animating_on_output(&output, target_presentation_time);
                let maximize_animating = app.maximize.is_animating(target_presentation_time);
                let closing_animating = app
                    .render
                    .window_close_animations
                    .is_animating_on_output(&output, target_presentation_time);
                let node_animating = app
                    .nodes
                    .is_animating_on_output(&output.name(), target_presentation_time);
                let bearings_animating = app
                    .shell
                    .bearings
                    .tick(&output.name(), target_presentation_time);
                let focus_cycle_animating = app.shell.focus_cycle.tick(target_presentation_time);
                let apogee_animating = crate::shell::apogee::tick(app, target_presentation_time);
                let background_animating =
                    app.background_animates_on_output(&output, target_presentation_time);
                let cluster_animating = app
                    .clusters
                    .is_animating_on_output(&output.name(), target_presentation_time)
                    || app
                        .clusters
                        .bloom_is_animating_on_output(&output.name(), target_presentation_time)
                    || app
                        .clusters
                        .labels_animating_on_output(&output.name(), app.nodes.config.show_labels);
                let pointer_time = app.start_time.elapsed().as_millis() as u32;
                if fullscreen_animating || maximize_animating {
                    super::pointer::update_client_state(app, pointer_time);
                } else if cluster_animating {
                    super::pointer::refresh_client_focus(app, pointer_time);
                }
                super::trace::snapshot(app);
                let position = app.pointer.position();
                let show_cursor = super::pointer::cursor_visible(app);
                let cursor_override = super::pointer::cursor_override(app);
                crate::cursor::surface::refresh_outputs(&app.cursor, &app.wayland.space, position);
                let next_cursor_frame = show_cursor
                    .then(|| {
                        app.cursor.current_next_frame_in_with_override(
                            output.current_scale().integer_scale(),
                            target_presentation_time,
                            cursor_override,
                        )
                    })
                    .flatten();
                app.cursor_policy
                    .schedule_animation(&output, next_cursor_frame);
                let outcome = app.driver.backend.render(
                    &output,
                    RenderRequest {
                        frame: FrameContext {
                            target_presentation_time,
                            vrr_auto_eligible: false,
                            clear: render::CLEAR_COLOR,
                        },
                        desktop: DesktopContext {
                            session_lock: &app.session_lock,
                            space: &app.wayland.space,
                            focused: app.wayland.focused_window.as_ref(),
                            cameras: &app.cameras,
                            window_open_animations: &app.window_open_animations,
                            fullscreen: &app.fullscreen,
                            maximize: &app.maximize,
                            nodes: &app.nodes,
                            clusters: &app.clusters,
                            window_rules: &app.window_rules,
                            layer_rules: &app.settings.layer_rules,
                            node_grab_active: app.interactions.grab.landmark_active(),
                            titlebar_hovered: app.interactions.titlebar_hovered.as_ref(),
                            titlebar_pressed: app.interactions.titlebar_pressed.as_ref(),
                        },
                        cursor: CursorContext {
                            cursor: &app.cursor,
                            cursor_position: position,
                            show_cursor,
                            cursor_override,
                        },
                        overlays: OverlayContext {
                            capture_overlay: app.capture.overlay(),
                            bearings: &app.shell.bearings,
                            focus_cycle: &app.shell.focus_cycle,
                            apogee: &app.shell.apogee,
                            apogee_config: app.settings.apogee,
                            overlays: &app.shell.overlays,
                            overlay_config: &app.settings.overlays,
                        },
                        visuals: VisualContext {
                            decorations: &app.settings.decorations,
                            font: &app.settings.font,
                            blur: app.settings.effects.blur,
                            shadows: app.settings.effects.shadows,
                            background: &app.settings.background,
                            background_base: app
                                .config_path
                                .as_deref()
                                .and_then(std::path::Path::parent),
                        },
                        resources: crate::render::resources::RenderResources::from(&mut app.render),
                    },
                );
                let (submitted, element_states) = match outcome {
                    Ok(outcome) if outcome.status() == backend::RenderStatus::Submitted => {
                        if let Some(generation) = app.session_lock.frame_generation() {
                            app.session_lock.presented(&output, generation);
                        }
                        (true, outcome.element_states().cloned())
                    }
                    Ok(outcome) => (false, outcome.element_states().cloned()),
                    Err(err) => {
                        eventline::error!("render failed: {err}");
                        (false, None)
                    }
                };
                if let Some(element_states) = element_states {
                    app.update_idle_inhibit_visibility(&output, &element_states);
                }
                if submitted {
                    app.service_screencopy(&output);
                }

                // Lets clients know their last commit was actually
                // presented, so they schedule their next frame - without
                // this a client's redraw loop just stalls forever.
                let output = app.driver.backend.output().clone();
                let elapsed = app.start_time.elapsed();
                if app.session_lock.active() {
                    if submitted {
                        crate::wayland::session_lock::send_frames(
                            &app.session_lock,
                            &output,
                            elapsed,
                        );
                    }
                } else if app.shell.apogee.is_active() {
                    if app.shell.apogee.take_callback_due(
                        &output.name(),
                        target_presentation_time,
                        app.settings.apogee.preview_max_fps,
                    ) {
                        crate::shell::apogee::send_preview_frames(
                            &app.shell.apogee,
                            &app.nodes,
                            &output,
                            elapsed,
                        );
                    }
                } else {
                    app.wayland.space.elements().for_each(|window| {
                        window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                            Some(output.clone())
                        });
                    });
                    crate::nodes::send_hover_preview_frame(
                        &app.nodes,
                        &output,
                        elapsed,
                        target_presentation_time,
                    );
                }
                wayland::layer_shell::send_frames(&output, elapsed);
                crate::cursor::surface::send_frame(
                    &app.cursor,
                    &app.wayland.space,
                    &output,
                    app.pointer.position(),
                    elapsed,
                );
                app.wayland.space.refresh();
                if crate::xwayland::sync_positions(app) {
                    super::pointer::refresh_desktop_client_focus(
                        app,
                        app.start_time.elapsed().as_millis() as u32,
                    );
                }
                crate::xwayland::sync_stacking_order(app);
                wayland::layer_shell::cleanup(&mut app.wayland);
                app.window_open_animations.cleanup(target_presentation_time);
                app.render
                    .window_close_animations
                    .cleanup(target_presentation_time);
                if app.cleanup_fullscreen(target_presentation_time) {
                    // Cleanup retires the transition and drops its crossfade
                    // textures, so the scene this frame rendered is the last
                    // one drawn from those textures. One more frame is owed to
                    // swap back to the live surfaces; without it the swap waits
                    // on unrelated damage and lands as a pop.
                    animating = true;
                    super::sync_keyboard_focus(app, smithay::utils::SERIAL_COUNTER.next_serial());
                    super::pointer::update_client_state(
                        app,
                        app.start_time.elapsed().as_millis() as u32,
                    );
                }

                if animating
                    || physics_animating
                    || window_animating
                    || closing_animating
                    || fullscreen_animating
                    || maximize_animating
                    || node_animating
                    || bearings_animating
                    || focus_cycle_animating
                    || apogee_animating
                    || background_animating
                    || cluster_animating
                    || app.shell.overlays.animating(target_presentation_time)
                    || app.render.node_renderer.has_pending_icons()
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
                crate::wayland::session_lock::configure_surfaces(app);
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
                app.notify_output_management();
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
