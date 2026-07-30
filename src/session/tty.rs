use std::collections::HashMap;
use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, LoopSignal};
use smithay::backend::drm::{DrmEvent, DrmEventMetadata, DrmEventTime};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::drm::control::crtc;
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::compositor::CompositorHandler;
use smithay::wayland::drm_syncobj::DrmSyncPointSource;
use smithay::wayland::seat::WaylandFocus;

use crate::backend::tty::TtyBackend;
use crate::backend::{CLEAR_COLOR, RenderOutcome, RenderRequest, RenderStatus, Renderable};
use crate::cursor::CursorManager;
use crate::input::keybinds::BackendKind;
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::input::{Keyboard, SuppressedButtons, SuppressedKeys};
use crate::wayland;

use super::SessionDriver;
use super::tty_frame::{EstimatedVblankTimer, OutputFrameState};

struct TtyDriver {
    backend: TtyBackend,
    physical_input: crate::input::devices::PhysicalInputDevices,
    loop_handle: LoopHandle<'static, TtyApp>,
    loop_signal: LoopSignal,
    output_frames: HashMap<Output, OutputFrameState>,
    paused: bool,
    pending_output_config: Option<Vec<halley_config::OutputConfig>>,
}

impl crate::ipc::OutputInfoSource for TtyDriver {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo> {
        crate::ipc::OutputInfoSource::output_info(&self.backend)
    }
}

impl super::SessionDriver for TtyDriver {
    const BACKEND_KIND: BackendKind = BackendKind::Tty;

    fn primary_output(&self) -> &Output {
        self.backend.primary_output()
    }

    fn dmabuf_capabilities(&mut self) -> crate::backend::dmabuf::DmabufCapabilities {
        self.backend.dmabuf_capabilities()
    }

    fn import_dmabuf(&mut self, dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.backend.import_dmabuf(dmabuf)
    }

    fn dmabuf_feedback(
        &self,
        output: &Output,
    ) -> Option<&crate::backend::dmabuf::SurfaceDmabufFeedback> {
        self.backend.dmabuf_feedback(output)
    }

    fn request_redraw(&mut self, output: Option<&Output>) {
        if let Some(output) = output {
            if self.backend.output_dpms_enabled(output)
                && let Some(state) = self.output_frames.get_mut(output)
            {
                state.queue_redraw();
            }
            return;
        }
        for (output, state) in &mut self.output_frames {
            if self.backend.output_dpms_enabled(output) {
                state.queue_redraw();
            }
        }
    }

    fn with_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut smithay::backend::renderer::gles::GlesRenderer) -> T,
    ) -> T {
        f(self.backend.renderer())
    }

    fn register_drm_syncobj_source(&mut self, client: Client, source: DrmSyncPointSource) -> bool {
        self.loop_handle
            .insert_source(source, move |_, _, app| {
                let dh = app.wayland.display_handle.clone();
                app.client_compositor_state(&client)
                    .blocker_cleared(app, &dh);
                Ok(())
            })
            .map(|_| true)
            .unwrap_or_else(|err| {
                eventline::warn!("explicit sync: failed to register acquire-point source: {err}");
                false
            })
    }

    fn apply_dpms(
        &mut self,
        command: halley_ipc::DpmsCommand,
        output: Option<&str>,
    ) -> Result<(), String> {
        self.apply_dpms_command(command, output)
    }

    fn output_requires_lock_frame(&self, output: &Output) -> bool {
        self.backend.output_dpms_enabled(output)
    }

    fn stop(&mut self) {
        self.loop_signal.stop();
    }
}

type TtyApp = super::Session<TtyDriver>;

impl TtyDriver {
    fn apply_dpms_command(
        &mut self,
        command: halley_ipc::DpmsCommand,
        output: Option<&str>,
    ) -> Result<(), String> {
        let applied = self.backend.apply_dpms(command, output)?;
        let now = crate::frame_clock::monotonic_now();
        for change in applied.changes {
            let Some(state) = self.output_frames.get_mut(&change.output) else {
                continue;
            };
            if change.enabled {
                state.resume(now);
            } else if let Some(token) = state.suspend(now) {
                self.loop_handle.remove(token);
            }
        }
        match applied.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn wake_dpms_on_input(&mut self, target: Option<&Output>) {
        let output = if !self.backend.any_output_dpms_enabled() {
            None
        } else {
            let Some(target) = target else {
                return;
            };
            if self.backend.output_dpms_enabled(target) {
                return;
            }
            Some(target.name())
        };
        if let Err(err) = self.apply_dpms_command(halley_ipc::DpmsCommand::On, output.as_deref()) {
            eventline::warn!("tty dpms: failed to wake on input: {err}");
        }
    }
}

/// Runs the real-hardware (DRM/KMS) session - takes over the seat and a
/// free VT. Returns (rather than panicking) if `TtyBackend::new()` fails,
/// since that's expected when nested under a host compositor that already
/// holds exclusive session control.
pub fn run(explicit_config_path: Option<std::path::PathBuf>) {
    let initial = crate::config::load_initial(explicit_config_path);
    let config_path = initial.path;
    let runtime_config = initial.config;
    let (backend, session_notifier, drm_notifier) = match TtyBackend::new(&runtime_config.outputs) {
        Ok(parts) => parts,
        Err(err) => {
            eventline::error!("TtyBackend::new() failed: {err}");
            return;
        }
    };
    eventline::info!("TtyBackend constructed successfully");

    let mut libinput_context =
        Libinput::new_with_udev::<LibinputSessionInterface<_>>(backend.session().into());
    libinput_context
        .udev_assign_seat(&backend.session().seat())
        .expect("failed to assign udev seat for libinput");
    let libinput_backend = LibinputInputBackend::new(libinput_context);

    let mut event_loop: EventLoop<TtyApp> =
        EventLoop::try_new().expect("failed to create event loop");
    let loop_signal = event_loop.get_signal();
    let loop_handle = event_loop.handle();

    let display: Display<TtyApp> = Display::new().expect("failed to create wayland display");
    let dh = display.handle();

    let mut seat_state = SeatState::new();
    let mut seat: Seat<TtyApp> = seat_state.new_wl_seat(&dh, "seat0");
    let applied_keyboard = crate::input::config::add_keyboard(&mut seat, &runtime_config.input)
        .expect("failed to advertise keyboard capability on the wl_seat");
    seat.add_pointer();

    let outputs: Vec<_> = backend.outputs().cloned().collect();
    let _output_globals: Vec<_> = outputs
        .iter()
        .map(|output| output.create_global::<TtyApp>(&dh))
        .collect();

    // Smithay's `Output` is its stable identity handle and is the key used
    // throughout its own per-output state maps despite containing an Arc.
    #[allow(clippy::mutable_key_type)]
    let output_frames = outputs
        .iter()
        .cloned()
        .map(|output| {
            let interval = backend.refresh_interval_for_output(&output);
            (output, OutputFrameState::new(interval))
        })
        .collect();

    let mut driver = TtyDriver {
        backend,
        physical_input: crate::input::devices::PhysicalInputDevices::default(),
        loop_handle: loop_handle.clone(),
        loop_signal,
        output_frames,
        paused: false,
        pending_output_config: None,
    };
    let wayland = TtyApp::create_wayland_state(dh.clone(), &mut driver);
    let idle_notifier_state =
        smithay::wayland::idle_notify::IdleNotifierState::new(&dh, loop_handle.clone());
    let presentation_state = smithay::wayland::presentation::PresentationState::new::<TtyApp>(
        &dh,
        smithay::utils::Clock::<smithay::utils::Monotonic>::new().id() as u32,
    );
    let session_lock = crate::wayland::session_lock::State::new::<TtyDriver>(&dh);
    let drm_syncobj_state = if smithay::wayland::drm_syncobj::supports_syncobj_eventfd(
        driver.backend.drm_device_fd(),
    ) {
        eventline::info!("explicit sync: linux-drm-syncobj-v1 available on the primary DRM device");
        Some(
            smithay::wayland::drm_syncobj::DrmSyncobjState::new::<TtyApp>(
                &dh,
                driver.backend.drm_device_fd().clone(),
            ),
        )
    } else {
        eventline::info!(
            "explicit sync: primary DRM device lacks syncobj eventfd support; protocol disabled"
        );
        None
    };
    let xwayland = crate::xwayland::State::<TtyDriver>::new(&dh, loop_handle.clone());
    let keyboard_monitor = match crate::accessibility::KeyboardMonitorService::start() {
        Ok(service) => Some(service),
        Err(err) => {
            eventline::warn!("accessibility: keyboard monitor unavailable: {err}");
            None
        }
    };
    let mut applied_input = runtime_config.input.clone();
    applied_input.keyboard = applied_keyboard;
    let launch_environment = super::environment::LaunchEnvironment::new(&runtime_config.env);
    let launch_path = launch_environment.path();
    let mut app = TtyApp {
        driver,
        keyboard: Keyboard::from_config(
            &runtime_config.keybinds,
            BackendKind::Tty,
            launch_path.as_deref(),
        ),
        launch_environment,
        autostart: super::autostart::Autostart::enabled(),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorManager::new(&runtime_config.cursor),
        cursor_policy: super::cursor::Policy::new(&runtime_config.cursor, loop_handle.clone()),
        publish_session_environment: true,
        wayland,
        seat_state,
        seat,
        idle_notifier_state,
        presentation_state,
        drm_syncobj_state,
        session_lock,
        start_time: Instant::now(),
        config_path: config_path.clone(),
        startup_config_diagnostic: initial.diagnostic,
        overlays: crate::overlay::OverlayManager::default(),
        overlay_config: runtime_config.overlays,
        nodes: crate::nodes::NodesState::new(&runtime_config),
        bearings: crate::bearings::BearingsState::new(runtime_config.bearings),
        focus_cycle: crate::focus_cycle::FocusCycleState::default(),
        pending_pointer_warp: None,
        apogee: crate::apogee::ApogeeState::default(),
        apogee_config: runtime_config.apogee,
        input: applied_input,
        decorations: runtime_config.decorations,
        cameras: crate::camera::OutputCameras::default(),
        field_config: runtime_config.field,
        zoom: runtime_config.field.zoom,
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
        keyboard_monitor,
        opening_origins: super::opening::OpeningOrigins::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
        window_close_animations: crate::backend::close::WindowCloseAnimations::new(
            runtime_config.animations,
        ),
        fullscreen: crate::wayland::fullscreen::FullscreenManager::new(runtime_config.animations),
        maximize: crate::wayland::maximize::FieldMaximizeManager::new(
            runtime_config.field,
            runtime_config.animations,
        ),
        fullscreen_textures:
            crate::backend::fullscreen_texture::FullscreenTextureTransitions::default(),
        overlay_previews: crate::backend::overlay_preview::OverlayPreviewCache::default(),
        node_renderer: crate::backend::node::NodeRenderer::default(),
        window_decoration_renderer:
            crate::backend::window_decoration::WindowDecorationRenderer::default(),
        backdrop_blur_renderer: crate::backend::backdrop_blur::BackdropBlurRenderer::default(),
        ui_text: crate::backend::text::UiTextRenderer::new(&runtime_config.font),
        xwayland,
    };
    for output in outputs {
        app.wayland
            .space
            .map_output(&output, output.current_location());
        let geometry = app
            .wayland
            .space
            .output_geometry(&output)
            .expect("mapped tty output has geometry");
        app.cameras
            .insert(output.name(), geometry.size.to_physical(1));
    }
    app.initialize_config_notification();

    let socket_name = super::protocol::init_wayland_listener(display, &mut event_loop);
    eventline::info!("wayland socket ready, WAYLAND_DISPLAY={socket_name:?}");
    app.arm_autostart_once(&socket_name, runtime_config.autostart.once.clone());
    super::environment::activate_session(&socket_name, &runtime_config.cursor);
    if let Err(err) = crate::xwayland::start(&event_loop.handle(), &mut app, true) {
        eventline::warn!("xwayland: unavailable: {err}");
        app.run_autostart_once();
    }

    if let Err(err) =
        crate::ipc::init_ipc_listener(&event_loop.handle(), |app: &mut TtyApp, request| {
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
    if let Err(err) = super::install_node_decay_timer(&event_loop.handle()) {
        eventline::warn!("nodes: failed to start decay timer: {err}");
    }
    if let Err(err) = super::install_apogee_preview_timer(&event_loop.handle()) {
        eventline::warn!("apogee: failed to start preview timer: {err}");
    }
    if let Err(err) = super::install_overlay_timer(&event_loop.handle()) {
        eventline::warn!("overlays: failed to start lifecycle timer: {err}");
    }

    // Queue every output's first frame through the same state machine used
    // for all later redraws.
    queue_redraw(&mut app);
    redraw_queued_outputs(&mut app, &loop_handle);

    event_loop
        .handle()
        .insert_source(libinput_backend, move |event, _, app| {
            let wake_before = match &event {
                smithay::backend::input::InputEvent::Keyboard { .. } => {
                    crate::wayland::focus::selected_output(&app.wayland).cloned()
                }
                smithay::backend::input::InputEvent::PointerButton { .. }
                | smithay::backend::input::InputEvent::PointerAxis { .. } => output_at_pointer(app),
                _ => None,
            };
            if matches!(
                &event,
                smithay::backend::input::InputEvent::Keyboard { .. }
                    | smithay::backend::input::InputEvent::PointerButton { .. }
                    | smithay::backend::input::InputEvent::PointerAxis { .. }
            ) {
                app.driver.wake_dpms_on_input(wake_before.as_ref());
            }
            match &event {
                smithay::backend::input::InputEvent::DeviceAdded { device } => {
                    let first_touch = app.driver.physical_input.added(device.clone(), &app.input);
                    if first_touch && app.seat.get_touch().is_none() {
                        app.seat.add_touch();
                    }
                }
                smithay::backend::input::InputEvent::DeviceRemoved { device }
                    if app.driver.physical_input.removed(device) =>
                {
                    super::touch::cancel_all(app);
                    app.seat.remove_touch();
                }
                smithay::backend::input::InputEvent::DeviceRemoved { .. } => {}
                _ => {}
            }
            super::input::handle(app, &event, &socket_name);
            if matches!(
                &event,
                smithay::backend::input::InputEvent::PointerMotion { .. }
                    | smithay::backend::input::InputEvent::PointerMotionAbsolute { .. }
            ) {
                let target = output_at_pointer(app);
                app.driver.wake_dpms_on_input(target.as_ref());
            }
        })
        .expect("failed to insert libinput source");

    event_loop
        .handle()
        .insert_source(session_notifier, {
            let loop_handle = loop_handle.clone();
            move |event, _, app| match event {
                SessionEvent::PauseSession => {
                    eventline::info!("session event: pause");
                    app.driver.paused = true;
                    app.driver.backend.pause();
                }
                SessionEvent::ActivateSession => {
                    eventline::info!("session event: activate");
                    app.driver.paused = false;
                    match app.driver.backend.resume() {
                        // The whole DRM pipeline (and any frame that was in
                        // flight before the switch away) is gone - reset
                        // clean rather than trusting whatever redraw states
                        // said before the switch.
                        Ok(()) => {
                            if let Some(outputs) = app.driver.pending_output_config.take() {
                                apply_tty_output_config(app, &outputs);
                            }
                            reset_redraw_state(app, &loop_handle);
                        }
                        Err(err) => eventline::error!("resume failed: {err}"),
                    }
                }
            }
        })
        .expect("failed to insert session notifier");

    event_loop
        .handle()
        .insert_source(drm_notifier, |event, metadata, app| match event {
            DrmEvent::VBlank(crtc) => on_vblank(app, crtc, metadata.as_ref()),
            DrmEvent::Error(err) => eventline::error!("drm event: error {err:?}"),
        })
        .expect("failed to insert drm notifier");

    eventline::info!(
        "dispatching - switch to this VT to see a solid color fill the screen, press the Quit chord to exit"
    );
    event_loop
        .run(None, &mut app, |app| {
            app.reap_autostart();
            if !app.driver.paused {
                redraw_queued_outputs(app, &loop_handle);
            }
            let _ = app.wayland.display_handle.flush_clients();
        })
        .expect("event loop run failed");
    eventline::info!("quit requested, exiting cleanly");
}

fn presentation_time(metadata: Option<&DrmEventMetadata>) -> Option<Duration> {
    match metadata?.time {
        DrmEventTime::Monotonic(time) if !time.is_zero() => Some(time),
        DrmEventTime::Monotonic(_) | DrmEventTime::Realtime(_) => None,
    }
}

fn on_vblank(app: &mut TtyApp, crtc: crtc::Handle, metadata: Option<&DrmEventMetadata>) {
    let Some(output) = app.driver.backend.output_for_crtc(crtc).cloned() else {
        eventline::warn!("vblank received for unknown CRTC {crtc:?}");
        return;
    };
    let submission = match app.driver.backend.frame_submitted(crtc) {
        Ok(submission) => submission,
        Err(err) => {
            eventline::warn!(
                "failed to acknowledge vblank for {:?}: {err}",
                output.name()
            );
            None
        }
    };
    let presented = presentation_time(metadata);
    let Some(state) = app.driver.output_frames.get_mut(&output) else {
        return;
    };
    if let Some(unexpected) = state.on_vblank(presented) {
        eventline::warn!(
            "unexpected redraw state on vblank for {:?}: {unexpected}",
            output.name()
        );
    }
    if !app.driver.backend.output_dpms_enabled(&output) {
        return;
    }

    if let Some(mut submission) = submission {
        if let Some(generation) = submission.session_lock_generation {
            app.session_lock.presented(&output, generation);
        }
        // Keep the prediction attached to its submitted frame for
        // diagnostics, while reporting the kernel page-flip timestamp to
        // clients whenever it is available.
        let _target_presentation_time = submission.target_presentation_time;
        let refresh = output
            .current_mode()
            .map(|mode| {
                smithay::wayland::presentation::Refresh::fixed(Duration::from_secs_f64(
                    1_000.0 / mode.refresh as f64,
                ))
            })
            .unwrap_or(smithay::wayland::presentation::Refresh::Unknown);
        let sequence = metadata
            .map(|metadata| u64::from(metadata.sequence))
            .unwrap_or(0);
        let (time, flags) = match presented {
            Some(time) => (
                smithay::utils::Time::<smithay::utils::Monotonic>::from(time),
                smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync
                    | smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::HwClock
                    | smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::HwCompletion,
            ),
            None => (
                smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
                smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync,
            ),
        };
        submission
            .presentation_feedback
            .presented(time, refresh, sequence, flags);
    }

    let elapsed = app.start_time.elapsed();
    let primary = app.driver.backend.primary_output();
    if app.session_lock.active() {
        crate::wayland::session_lock::send_frames(&app.session_lock, &output, elapsed);
    } else if app.apogee.is_active() {
        if app.apogee.take_callback_due(
            &output.name(),
            crate::frame_clock::monotonic_now(),
            app.apogee_config.preview_max_fps,
        ) {
            crate::apogee::send_preview_frames(&app.apogee, &app.nodes, &output, elapsed);
        }
    } else {
        app.wayland
            .space
            .elements()
            .filter(|window| wayland::window_is_on_output(window, &output, primary))
            .for_each(|window| {
                window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            });
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
    wayland::layer_shell::cleanup(&mut app.wayland);
}

/// Pure state transition - safe to call from any input/event handler with
/// no event-loop access. The actual rendering only ever happens in
/// `redraw_output()`, called from the `run()` tail once a redraw is actually
/// `Queued`.
fn queue_redraw(app: &mut TtyApp) {
    app.request_redraw();
}

fn output_at_pointer(app: &TtyApp) -> Option<Output> {
    app.wayland
        .space
        .output_under(app.pointer.position())
        .next()
        .cloned()
}

fn queue_output_redraw(app: &mut TtyApp, output: &Output) {
    app.request_output_redraw(output);
}

fn apply_runtime_config(app: &mut TtyApp, reload: crate::config::ConfigReload) {
    match reload {
        crate::config::ConfigReload::Loaded(config) => {
            let reload_commands = config.autostart.on_reload.clone();
            app.apply_common_config(&config);
            app.driver.physical_input.reload(&app.input);

            if app.driver.paused {
                app.driver.pending_output_config = Some(config.outputs);
            } else {
                apply_tty_output_config(app, &config.outputs);
            }
            app.run_autostart_reload(&reload_commands);
            app.clear_config_reload_error();
        }
        crate::config::ConfigReload::Rejected(diagnostic) => {
            eventline::debug!("config: rejected reload for {:?}", diagnostic.path);
            app.show_config_reload_error();
        }
    }
}

fn apply_tty_output_config(app: &mut TtyApp, outputs_config: &[halley_config::OutputConfig]) {
    let changes = app.driver.backend.apply_output_config(outputs_config);
    let mut layout_changed = false;
    let mut output_changed = false;

    for change in changes {
        output_changed = true;
        if change.mode_changed {
            let interval = app
                .driver
                .backend
                .refresh_interval_for_output(&change.output);
            if let Some(state) = app.driver.output_frames.get_mut(&change.output) {
                state.replace_clock(interval);
            }
        }

        if change.layout_changed {
            app.wayland
                .space
                .map_output(&change.output, change.output.current_location());
            smithay::desktop::layer_map_for_output(&change.output).arrange();
            layout_changed = true;
        }

        if change.size_changed
            && let Some(geometry) = app.wayland.space.output_geometry(&change.output)
        {
            app.cameras
                .reset(change.output.name(), geometry.size.to_physical(1));
            let external = app
                .fullscreen
                .reconfigure_output(&app.wayland, &change.output);
            crate::xwayland::reconfigure_fullscreen(external);
        }

        if change.mode_changed || change.layout_changed {
            queue_output_redraw(app, &change.output);
        }
    }

    if layout_changed {
        app.wayland.space.refresh();
        app.capture.update_layout(&app.wayland.space);
    }
    if output_changed {
        crate::wayland::session_lock::configure_surfaces(app);
        super::pointer::update_client_state(app, app.start_time.elapsed().as_millis() as u32);
    }
}

fn redraw_queued_outputs(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    let _ = crate::nodes::tick_physics(app, crate::frame_clock::monotonic_now());
    let outputs: Vec<_> = app
        .driver
        .output_frames
        .iter()
        .filter(|(_, state)| state.is_redraw_queued())
        .map(|(output, _)| output.clone())
        .collect();

    for output in outputs {
        redraw_output(app, &output, loop_handle);
    }
}

fn redraw_output(app: &mut TtyApp, output: &Output, loop_handle: &LoopHandle<'_, TtyApp>) {
    let now = crate::frame_clock::monotonic_now();
    let (target_presentation_time, dt) = {
        let state = app
            .driver
            .output_frames
            .get_mut(output)
            .expect("redraw output has frame state");
        state.next_frame_sample(now)
    };

    let pointer_is_on_output = app
        .wayland
        .space
        .output_under(app.pointer.position())
        .next()
        .is_some_and(|under| under == output);
    let view_before = pointer_is_on_output
        .then(|| app.cameras.view(&output.name()))
        .flatten();
    let fullscreen_camera_changed = app.sync_fullscreen_camera(output, target_presentation_time);
    let camera_animating = app.cameras.get_mut(&output.name()).is_some_and(|camera| {
        crate::input::zoom::tick(
            camera,
            &app.zoom,
            app.input.gestures.pan_decay_rate,
            dt.as_secs_f32(),
        )
        .1
    });
    let primary = app.driver.backend.primary_output();
    let window_animating = app.wayland.space.elements().any(|window| {
        wayland::window_is_on_output(window, output, primary)
            && window.wl_surface().is_some_and(|surface| {
                app.window_open_animations
                    .is_animating(surface.as_ref(), target_presentation_time)
            })
    });
    let _ = crate::focus_cycle::finish_pending_pointer_warp(app);
    let fullscreen_animating = app
        .fullscreen
        .is_animating_on_output(output, target_presentation_time);
    let maximize_animating = app.maximize.is_animating(target_presentation_time);
    let closing_animating = app
        .window_close_animations
        .is_animating_on_output(output, target_presentation_time);
    let node_animating = app
        .nodes
        .is_animating_on_output(&output.name(), target_presentation_time);
    let bearings_animating = app.bearings.tick(&output.name(), target_presentation_time);
    let focus_cycle_animating = app.focus_cycle.tick(target_presentation_time);
    let apogee_animating = crate::apogee::tick(app, target_presentation_time);
    let overlay_animating = app.overlays.animating(target_presentation_time);
    let show_cursor = super::pointer::cursor_visible(app);
    let cursor_override = app
        .capture
        .is_active()
        .then_some(smithay::input::pointer::CursorIcon::Default);
    crate::cursor::surface::refresh_outputs(
        &app.cursor,
        &app.wayland.space,
        app.pointer.position(),
    );
    let cursor_animating = show_cursor
        && app.cursor.current_is_animated_with_override(
            output.current_scale().integer_scale(),
            cursor_override,
        );
    let mut animating = camera_animating
        || fullscreen_camera_changed
        || window_animating
        || closing_animating
        || node_animating
        || bearings_animating
        || focus_cycle_animating
        || apogee_animating
        || overlay_animating
        || fullscreen_animating
        || maximize_animating
        || cursor_animating;
    if (fullscreen_animating || maximize_animating) && pointer_is_on_output {
        super::pointer::update_client_state(app, app.start_time.elapsed().as_millis() as u32);
    }
    let view_after = pointer_is_on_output
        .then(|| app.cameras.view(&output.name()))
        .flatten();
    if view_before != view_after {
        super::pointer::update_client_state(app, app.start_time.elapsed().as_millis() as u32);
    }

    let outcome = match app.driver.backend.render(
        output,
        RenderRequest {
            target_presentation_time,
            clear: CLEAR_COLOR,
            session_lock: &app.session_lock,
            cursor: &app.cursor,
            cursor_position: app.pointer.position(),
            show_cursor,
            cursor_override,
            capture_overlay: app.capture.overlay(),
            space: &app.wayland.space,
            focused: app.wayland.focused_window.as_ref(),
            decorations: &app.decorations,
            cameras: &app.cameras,
            window_open_animations: &app.window_open_animations,
            window_close_animations: &mut app.window_close_animations,
            fullscreen: &app.fullscreen,
            maximize: &app.maximize,
            fullscreen_textures: &mut app.fullscreen_textures,
            overlay_previews: &mut app.overlay_previews,
            nodes: &app.nodes,
            bearings: &app.bearings,
            backdrop_blur_renderer: &mut app.backdrop_blur_renderer,
            focus_cycle: &app.focus_cycle,
            apogee: &app.apogee,
            apogee_config: app.apogee_config,
            overlays: &app.overlays,
            overlay_config: &app.overlay_config,
            node_renderer: &mut app.node_renderer,
            window_decoration_renderer: &mut app.window_decoration_renderer,
            ui_text: &mut app.ui_text,
        },
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            eventline::error!("render failed for {:?}: {err}", output.name());
            RenderOutcome::new(RenderStatus::Skipped, None)
        }
    };
    animating |= app.node_renderer.has_pending_icons();
    app.window_open_animations.cleanup(target_presentation_time);
    app.window_close_animations
        .cleanup(target_presentation_time);
    if app.cleanup_fullscreen(target_presentation_time) {
        super::sync_keyboard_focus(app, smithay::utils::SERIAL_COUNTER.next_serial());
        super::pointer::update_client_state(app, app.start_time.elapsed().as_millis() as u32);
    }

    let feedback = app.driver.dmabuf_feedback(output).cloned();
    if let (Some(feedback), Some(element_states)) = (feedback.as_ref(), outcome.element_states()) {
        let primary_output = app.driver.backend.primary_output().clone();
        wayland::dmabuf::send_output_feedback(
            &app.wayland,
            output,
            &primary_output,
            &app.session_lock,
            feedback,
            element_states,
        );
    }

    if outcome.status() == RenderStatus::Submitted {
        let state = app
            .driver
            .output_frames
            .get_mut(output)
            .expect("rendered output has frame state");
        if let Some(token) = state.frame_submitted(animating) {
            loop_handle.remove(token);
        }
        return;
    }

    queue_estimated_vblank_timer(app, output, animating, loop_handle);
}

fn queue_estimated_vblank_timer(
    app: &mut TtyApp,
    output: &Output,
    animating: bool,
    loop_handle: &LoopHandle<'_, TtyApp>,
) {
    let state = app
        .driver
        .output_frames
        .get_mut(output)
        .expect("estimated-vblank output has frame state");
    let now = crate::frame_clock::monotonic_now();
    let EstimatedVblankTimer::ArmAfter(delay) = state.frame_skipped(animating, now) else {
        return;
    };
    let output = output.clone();
    let token = loop_handle
        .insert_source(Timer::from_duration(delay), move |_, _, app| {
            on_estimated_vblank_timer(app, &output);
            TimeoutAction::Drop
        })
        .expect("failed to arm estimated-vblank timer");
    state.timer_armed(token);
}

fn on_estimated_vblank_timer(app: &mut TtyApp, output: &Output) {
    let Some(state) = app.driver.output_frames.get_mut(output) else {
        return;
    };
    if let Some(unexpected) = state.estimated_vblank_fired() {
        eventline::warn!(
            "unexpected redraw state on estimated-vblank timer for {:?}: {unexpected}",
            output.name()
        );
    }
}

fn reset_redraw_state(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    let now = crate::frame_clock::monotonic_now();
    for state in app.driver.output_frames.values_mut() {
        if let Some(token) = state.reset(now) {
            loop_handle.remove(token);
        }
    }
}
