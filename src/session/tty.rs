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
use smithay::reexports::wayland_server::Display;

use crate::backend::tty::TtyBackend;
use crate::backend::{CLEAR_COLOR, RenderOutcome, RenderRequest, RenderStatus, Renderable};
use crate::cursor::CursorImage;
use crate::input::keybinds::BackendKind;
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::input::{Keyboard, SuppressedButtons};
use crate::ipc::OutputInfoSource;
use crate::wayland;

use super::SessionDriver;
use super::tty_frame::{EstimatedVblankTimer, OutputFrameState};

struct TtyDriver {
    backend: TtyBackend,
    loop_signal: LoopSignal,
    output_frames: HashMap<Output, OutputFrameState>,
    paused: bool,
    pending_output_config: Option<Vec<halley_config::OutputConfig>>,
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
            if let Some(state) = self.output_frames.get_mut(output) {
                state.queue_redraw();
            }
            return;
        }
        for state in self.output_frames.values_mut() {
            state.queue_redraw();
        }
    }

    fn with_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut smithay::backend::renderer::gles::GlesRenderer) -> T,
    ) -> T {
        f(self.backend.renderer())
    }

    fn stop(&mut self) {
        self.loop_signal.stop();
    }
}

type TtyApp = super::Session<TtyDriver>;

/// Runs the real-hardware (DRM/KMS) session - takes over the seat and a
/// free VT. Returns (rather than panicking) if `TtyBackend::new()` fails,
/// since that's expected when nested under a host compositor that already
/// holds exclusive session control.
pub fn run(session_mode: bool) {
    let (config_path, runtime_config) = crate::config::load_initial();
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
    seat.add_keyboard(Default::default(), 200, 25)
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
        loop_signal,
        output_frames,
        paused: false,
        pending_output_config: None,
    };
    let wayland = TtyApp::create_wayland_state(dh, &mut driver);
    let mut app = TtyApp {
        driver,
        keyboard: Keyboard::from_config(&runtime_config.keybinds, BackendKind::Tty),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorImage::load(),
        wayland,
        seat_state,
        seat,
        start_time: Instant::now(),
        decorations: runtime_config.decorations,
        cameras: crate::camera::OutputCameras::default(),
        zoom: runtime_config.zoom,
        grab: crate::input::grab::Grab::None,
        resize_anchor: None,
        suppressed_buttons: SuppressedButtons::default(),
        wheel_accumulator: WheelAccumulator::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
        fullscreen: crate::wayland::fullscreen::FullscreenManager::new(runtime_config.animations),
        fullscreen_textures:
            crate::backend::fullscreen_texture::FullscreenTextureTransitions::default(),
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

    let socket_name = super::protocol::init_wayland_listener(display, &mut event_loop);
    eventline::info!("wayland socket ready, WAYLAND_DISPLAY={socket_name:?}");
    if session_mode {
        super::environment::activate_session(&socket_name);
    }

    if let Err(err) = crate::ipc::init_ipc_listener(&event_loop.handle(), |app: &TtyApp| {
        app.driver.backend.output_info()
    }) {
        eventline::error!("ipc: failed to start listener: {err}");
    }
    if let Some(path) = config_path
        && let Err(err) = crate::config::watch(&event_loop.handle(), path, apply_runtime_config)
    {
        eventline::warn!("config: failed to start watcher: {err}");
    }

    // Queue every output's first frame through the same state machine used
    // for all later redraws.
    queue_redraw(&mut app);
    redraw_queued_outputs(&mut app, &loop_handle);

    event_loop
        .handle()
        .insert_source(libinput_backend, move |event, _, app| {
            super::input::handle(app, &event, &socket_name);
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
    // Keep the prediction attached to its submitted frame. It is useful for
    // presentation diagnostics, while the clock itself is deliberately
    // corrected only by the kernel's monotonic timestamp below.
    let _target_presentation_time =
        submission.map(|submission| submission.target_presentation_time);

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

    let elapsed = app.start_time.elapsed();
    let primary = app.driver.backend.primary_output();
    app.wayland
        .space
        .elements()
        .filter(|window| wayland::window_is_on_output(window, &output, primary))
        .for_each(|window| {
            window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        });
    wayland::layer_shell::send_frames(&output, elapsed);
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

fn queue_output_redraw(app: &mut TtyApp, output: &Output) {
    app.request_output_redraw(output);
}

fn apply_runtime_config(app: &mut TtyApp, config: halley_config::RuntimeConfig) {
    app.apply_common_config(&config);

    if app.driver.paused {
        app.driver.pending_output_config = Some(config.outputs);
    } else {
        apply_tty_output_config(app, &config.outputs);
    }
}

fn apply_tty_output_config(app: &mut TtyApp, outputs_config: &[halley_config::OutputConfig]) {
    let changes = app.driver.backend.apply_output_config(outputs_config);
    let mut layout_changed = false;

    for change in changes {
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
            app.fullscreen
                .reconfigure_output(&app.wayland, &change.output);
        }

        if change.mode_changed || change.layout_changed {
            queue_output_redraw(app, &change.output);
        }
    }

    if layout_changed {
        app.wayland.space.refresh();
        super::input::update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
        let pointer = app
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        pointer.frame(app);
    }
}

fn redraw_queued_outputs(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
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
    let camera_animating = app
        .cameras
        .get_mut(&output.name())
        .is_some_and(|camera| crate::input::zoom::tick(camera, &app.zoom, dt.as_secs_f32()).1);
    let primary = app.driver.backend.primary_output();
    let window_animating = app.wayland.space.elements().any(|window| {
        wayland::window_is_on_output(window, output, primary)
            && window.toplevel().is_some_and(|toplevel| {
                app.window_open_animations
                    .is_animating(toplevel.wl_surface(), target_presentation_time)
            })
    });
    let fullscreen_animating = app
        .fullscreen
        .is_animating_on_output(output, target_presentation_time);
    let animating = camera_animating || window_animating || fullscreen_animating;
    if fullscreen_animating && pointer_is_on_output {
        super::input::update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
        let pointer = app
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        pointer.frame(app);
    }
    let view_after = pointer_is_on_output
        .then(|| app.cameras.view(&output.name()))
        .flatten();
    if view_before != view_after {
        super::input::update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
        let pointer = app
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        pointer.frame(app);
    }

    let outcome = match app.driver.backend.render(
        output,
        RenderRequest {
            target_presentation_time,
            clear: CLEAR_COLOR,
            cursor: &app.cursor,
            cursor_position: app.pointer.position(),
            space: &app.wayland.space,
            focused: app.wayland.focused_window.as_ref(),
            decorations: &app.decorations,
            cameras: &app.cameras,
            window_open_animations: &app.window_open_animations,
            fullscreen: &app.fullscreen,
            fullscreen_textures: &mut app.fullscreen_textures,
        },
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            eventline::error!("render failed for {:?}: {err}", output.name());
            RenderOutcome::new(RenderStatus::Skipped, None)
        }
    };
    app.window_open_animations.cleanup(target_presentation_time);
    if app.cleanup_fullscreen(target_presentation_time) {
        super::sync_keyboard_focus(app, smithay::utils::SERIAL_COUNTER.next_serial());
        super::input::update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
        let pointer = app
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        pointer.frame(app);
    }

    let feedback = app.driver.dmabuf_feedback(output).cloned();
    if let (Some(feedback), Some(element_states)) = (feedback.as_ref(), outcome.element_states()) {
        let primary_output = app.driver.backend.primary_output().clone();
        wayland::dmabuf::send_output_feedback(
            &app.wayland,
            output,
            &primary_output,
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
