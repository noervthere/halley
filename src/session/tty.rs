use std::collections::HashMap;
use std::ffi::OsString;
use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, LoopSignal, RegistrationToken};
use smithay::backend::drm::{DrmEvent, DrmEventMetadata, DrmEventTime};
use smithay::backend::input::{
    ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::desktop::{Space, Window};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::drm::control::crtc;
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;

use crate::backend::tty::TtyBackend;
use crate::backend::{CLEAR_COLOR, RenderRequest, RenderStatus, Renderable};
use crate::cursor::CursorImage;
use crate::frame_clock::FrameClock;
use crate::input::{Keyboard, PointerBindingResult, SuppressedButtons};
use crate::input::keybinds::BackendKind;
use crate::input::{match_keyboard_bind, match_wheel_bind, process_pointer_binding};
use crate::input::pointer::{
    Pointer, WheelAccumulator, axis_frame_filtered, process_wheel_bindings,
};
use crate::ipc::OutputInfoSource;
use crate::wayland::{self, WaylandState};

use super::{focus_layer, focus_window};

/// From `<linux/input-event-codes.h>` - the left mouse button's raw code,
/// used instead of `PointerButtonEvent::button()`'s `MouseButton` enum
/// because libinput's own event type has an inherent `button()` returning a
/// raw `u32` that shadows the trait method of the same name.
const BTN_LEFT: u32 = 0x110;
/// The right mouse button, same source and same reasoning as `BTN_LEFT`.
const BTN_RIGHT: u32 = 0x111;

fn output_at_pointer(
    space: &Space<Window>,
    position: (f64, f64),
) -> Option<(Output, Rectangle<i32, Logical>)> {
    let output = space.output_under(position).next()?.clone();
    let geometry = space.output_geometry(&output)?;
    Some((output, geometry))
}

fn route_client_pointer(app: &TtyApp) -> Option<crate::input::pointer::PointerRoute> {
    crate::input::pointer::route_to_client(
        &app.wayland.space,
        &app.cameras,
        app.driver.backend.primary_output(),
        app.pointer.position(),
    )
}

/// Refreshes Smithay's pointer location and surface focus from Halley's
/// camera-aware scene.
fn update_client_pointer_focus(
    app: &mut TtyApp,
    time: u32,
) -> Option<crate::input::pointer::PointerRoute> {
    let route = route_client_pointer(app)?;
    let pointer = app
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    pointer.motion(
        app,
        route.focus.clone(),
        &MotionEvent {
            location: route.location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    Some(route)
}

fn dispatch_action(
    app: &mut TtyApp,
    action: halley_config::Action,
    socket_name: &OsString,
    output_name: Option<&str>,
) {
    let camera = output_name.and_then(|name| app.cameras.get_mut(name));
    if super::dispatch_action(
        action,
        &app.wayland,
        app.keyboard.terminal_command(),
        socket_name,
        camera,
        &app.zoom,
    ) == super::SessionControl::Quit
    {
        app.driver.loop_signal.stop();
    }
}

/// Tracks the minimum state needed for correct DRM redraw scheduling. DRM
/// only produces a VBlank event in response to a page flip it was actually
/// asked to do, so a scene that settles into "nothing changed" naturally
/// stops generating any further
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
    WaitingForVBlank {
        redraw_needed: bool,
    },
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
            RedrawState::WaitingForVBlank { .. } => RedrawState::WaitingForVBlank {
                redraw_needed: true,
            },
        }
    }
}

struct OutputFrameState {
    clock: FrameClock,
    redraw: RedrawState,
    last_camera_sample: Duration,
    unfinished_animations: bool,
}

impl OutputFrameState {
    fn new(refresh_interval: Duration) -> Self {
        Self {
            clock: FrameClock::new(Some(refresh_interval)),
            redraw: RedrawState::default(),
            last_camera_sample: crate::frame_clock::monotonic_now(),
            unfinished_animations: false,
        }
    }
}

struct TtyDriver {
    backend: TtyBackend,
    loop_signal: LoopSignal,
    output_frames: HashMap<Output, OutputFrameState>,
    paused: bool,
    pending_output_config: Option<Vec<halley_config::OutputConfig>>,
}

impl super::SessionDriver for TtyDriver {
    fn primary_output(&self) -> &Output {
        self.backend.primary_output()
    }

    fn request_redraw(&mut self, output: Option<&Output>) {
        if let Some(output) = output {
            if let Some(state) = self.output_frames.get_mut(output) {
                state.redraw = std::mem::take(&mut state.redraw).queue_redraw();
            }
            return;
        }
        for state in self.output_frames.values_mut() {
            state.redraw = std::mem::take(&mut state.redraw).queue_redraw();
        }
    }
}

type TtyApp = super::Session<TtyDriver>;

/// Runs the real-hardware (DRM/KMS) session - takes over the seat and a
/// free VT. Returns (rather than panicking) if `TtyBackend::new()` fails,
/// since that's expected when nested under a host compositor that already
/// holds exclusive session control.
pub fn run() {
    let (config_path, runtime_config) = crate::config::load_initial();
    let (backend, session_notifier, drm_notifier) = match TtyBackend::new(&runtime_config.outputs) {
        Ok(parts) => parts,
        Err(err) => {
            eventline::error!("TtyBackend::new() failed: {err}");
            return;
        }
    };
    eventline::info!("TtyBackend constructed successfully");

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
    let layer_shell_state = WlrLayerShellState::new::<TtyApp>(&dh);
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

    let mut app = TtyApp {
        driver: TtyDriver {
            backend,
            loop_signal,
            output_frames,
            paused: false,
            pending_output_config: None,
        },
        keyboard: Keyboard::from_config(&runtime_config.keybinds, BackendKind::Tty),
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
        cameras: crate::camera::OutputCameras::default(),
        zoom: runtime_config.zoom,
        grab: crate::input::grab::Grab::None,
        resize_anchor: None,
        suppressed_buttons: SuppressedButtons::default(),
        wheel_accumulator: WheelAccumulator::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
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
            let position_before = app.pointer.position();
            app.pointer.process_input_event(&event, &app.wayland.space);
            let position_after = app.pointer.position();
            queue_redraw(app);

            // Apply whatever's being dragged, if anything - reuses the
            // delta `Pointer` already computed (handles relative and
            // absolute motion, and clamping, uniformly) rather than
            // re-deriving it from the raw event per grab kind.
            match &app.grab {
                crate::input::grab::Grab::MoveWindow {
                    window,
                    screen_offset,
                } => {
                    if let Some((output, output_geometry)) =
                        output_at_pointer(&app.wayland.space, position_after)
                    {
                        let Some(camera) = app.cameras.get(&output.name()) else {
                            return;
                        };
                        let world = crate::input::grab::screen_to_world_on_output(
                            position_after,
                            camera,
                            output_geometry,
                        );
                        let world_offset = crate::input::grab::screen_offset_to_world(
                            *screen_offset,
                            camera,
                        );
                        let new_location = Point::<i32, Logical>::from((
                            (world.x + world_offset.x).round() as i32,
                            (world.y + world_offset.y).round() as i32,
                        ));
                        wayland::set_window_output(window, &output);
                        app.wayland
                            .space
                            .map_element(window.clone(), new_location, false);
                    }
                }
                crate::input::grab::Grab::Pan { output } => {
                    let dx = position_after.0 - position_before.0;
                    let dy = position_after.1 - position_before.1;
                    if let Some(camera) = app.cameras.get_mut(output) {
                        let delta =
                            crate::input::grab::screen_delta_to_world(dx, dy, camera);
                        // Negated - content follows the cursor ("natural
                        // drag"), on the output where this drag began.
                        camera.pan_target(halley_core::field::Vec2 {
                            x: -delta.x,
                            y: -delta.y,
                        });
                    }
                }
                crate::input::grab::Grab::ResizeWindow(state) => {
                    let primary = app.driver.backend.primary_output();
                    let output = app
                        .wayland
                        .space
                        .outputs()
                        .find(|output| wayland::window_is_on_output(&state.window, output, primary))
                        .cloned()
                        .unwrap_or_else(|| primary.clone());
                    let Some(output_geometry) = app.wayland.space.output_geometry(&output) else {
                        return;
                    };
                    let Some(camera) = app.cameras.get(&output.name()) else {
                        return;
                    };
                    let world = crate::input::grab::screen_to_world_on_output(
                        position_after,
                        camera,
                        output_geometry,
                    );
                    let size = crate::input::grab::resize_target_size(
                        state.handle,
                        state.start_rect,
                        state.start_cursor,
                        world,
                    );
                    if let Some(toplevel) = state.window.toplevel() {
                        toplevel.with_pending_state(|pending| pending.size = Some(size));
                        // No-ops unless the pending state actually changed, so
                        // this is safe to call per motion event rather than
                        // rate-limiting it here.
                        let serial = toplevel.send_pending_configure();
                        crate::input::grab::note_resize_configure(&mut app.resize_anchor, serial);
                    }
                }
                crate::input::grab::Grab::None => {}
            }

            let motion_time = match &event {
                InputEvent::PointerMotion { event } => Some(event.time_msec()),
                InputEvent::PointerMotionAbsolute { event } => Some(event.time_msec()),
                _ => None,
            };
            if let Some(time) = motion_time {
                update_client_pointer_focus(app, time);
                let pointer = app
                    .seat
                    .get_pointer()
                    .expect("pointer capability added at seat setup");
                pointer.frame(app);
            }

            if let InputEvent::PointerButton {
                event: button_event,
            } = &event
            {
                let button = button_event.button_code();
                let state = button_event.state();
                let time = button_event.time_msec();
                let serial = SERIAL_COUNTER.next_serial();
                let route = update_client_pointer_focus(app, time);
                if button == BTN_LEFT
                    && state == ButtonState::Pressed
                    && let Some(route) = route.as_ref()
                {
                    wayland::focus::select_output(&mut app.wayland, &route.output);
                }
                let mut intercepted = false;
                let mods = app
                    .seat
                    .get_keyboard()
                    .expect("keyboard capability added at seat setup")
                    .modifier_state();
                let bypass_shortcuts = wayland::focus::current(&app.wayland)
                    .is_some_and(|focus| focus.bypasses_shortcuts());
                let on_background = route.as_ref().is_some_and(|route| {
                    matches!(
                        &route.target,
                        crate::input::pointer::PointerTarget::Background
                    )
                });
                match process_pointer_binding(
                    &app.keyboard.binds,
                    &mods,
                    button,
                    state,
                    on_background,
                    !bypass_shortcuts,
                    &mut app.suppressed_buttons,
                ) {
                    PointerBindingResult::Action(action) => {
                        let output_name = route
                            .as_ref()
                            .map(|route| route.output.name().to_string());
                        dispatch_action(app, action, &socket_name, output_name.as_deref());
                        intercepted = true;
                    }
                    PointerBindingResult::SuppressedRelease => intercepted = true,
                    PointerBindingResult::Unhandled => {}
                }

                if !intercepted && button == BTN_RIGHT {
                    match state {
                        ButtonState::Pressed => {
                            // Resize is mod-only: a bare right-click stays
                            // available to clients for context menus.
                            if crate::input::mod_key_held(&mods, app.keyboard.effective_mod)
                                && let Some(crate::input::pointer::PointerRoute {
                                    target:
                                        crate::input::pointer::PointerTarget::Window(window),
                                    location,
                                    ..
                                }) = route.as_ref()
                                    && let Some(start_rect) =
                                        app.wayland.space.element_geometry(window)
                            {
                                    let world = halley_core::field::Vec2 {
                                        x: location.x as f32,
                                        y: location.y as f32,
                                    };
                                    let handle = crate::input::grab::handle_from_press_position(
                                        start_rect, world,
                                    );
                                    focus_window(app, window, serial);
                                    app.grab = crate::input::grab::Grab::ResizeWindow(
                                        crate::input::grab::ResizeState {
                                            window: window.clone(),
                                            handle,
                                            start_rect,
                                            start_cursor: world,
                                        },
                                    );
                                    app.resize_anchor = Some(crate::input::grab::ResizeAnchor {
                                        window: window.clone(),
                                        handle,
                                        phase: crate::input::grab::ResizePhase::Ongoing,
                                        last_configure: None,
                                        last_size: start_rect.size,
                                    });
                                    intercepted = true;
                            }
                        }
                        ButtonState::Released => {
                            if matches!(app.grab, crate::input::grab::Grab::ResizeWindow(_)) {
                                app.grab = crate::input::grab::Grab::None;
                                crate::input::grab::release_resize_anchor(&mut app.resize_anchor);
                                intercepted = true;
                            }
                        }
                    }
                } else if !intercepted && button == BTN_LEFT {
                    match state {
                        ButtonState::Pressed => {
                            let mod_held =
                                crate::input::mod_key_held(&mods, app.keyboard.effective_mod);
                            match route.as_ref().map(|route| &route.target) {
                                Some(crate::input::pointer::PointerTarget::Window(window))
                                    if mod_held =>
                                {
                                    let route = route.as_ref().expect("matched above");
                                    let world = halley_core::field::Vec2 {
                                        x: route.location.x as f32,
                                        y: route.location.y as f32,
                                    };
                                    let window_loc = app
                                        .wayland
                                        .space
                                        .element_location(window)
                                        .expect("routed window is mapped");
                                    let Some(camera) = app.cameras.get(&route.output.name()) else {
                                        return;
                                    };
                                    let scale = crate::input::zoom::scale(camera);
                                    let screen_offset = halley_core::field::Vec2 {
                                        x: (window_loc.x as f32 - world.x) * scale,
                                        y: (window_loc.y as f32 - world.y) * scale,
                                    };
                                    focus_window(app, window, serial);
                                    app.grab = crate::input::grab::Grab::MoveWindow {
                                        window: window.clone(),
                                        screen_offset,
                                    };
                                    intercepted = true;
                                }
                                Some(crate::input::pointer::PointerTarget::Window(window)) => {
                                    focus_window(app, window, serial);
                                }
                                Some(crate::input::pointer::PointerTarget::Layer(layer)) => {
                                    focus_layer(app, Some(layer.clone()), serial);
                                }
                                Some(crate::input::pointer::PointerTarget::Background) => {
                                    focus_layer(app, None, serial);
                                    app.grab = crate::input::grab::Grab::Pan {
                                        output: route
                                            .as_ref()
                                            .expect("matched above")
                                            .output
                                            .name(),
                                    };
                                    intercepted = true;
                                }
                                None => {}
                            }
                        }
                        ButtonState::Released => {
                            if matches!(
                                app.grab,
                                crate::input::grab::Grab::MoveWindow { .. }
                                    | crate::input::grab::Grab::Pan { .. }
                            ) {
                                app.grab = crate::input::grab::Grab::None;
                                intercepted = true;
                            }
                        }
                    }
                }

                if !intercepted {
                    let pointer = app
                        .seat
                        .get_pointer()
                        .expect("pointer capability added at seat setup");
                    pointer.button(
                        app,
                        &ButtonEvent {
                            serial,
                            time,
                            button,
                            state,
                        },
                    );
                }
                let pointer = app
                    .seat
                    .get_pointer()
                    .expect("pointer capability added at seat setup");
                pointer.frame(app);
            }

            if let InputEvent::PointerAxis { event: axis_event } = &event {
                let route = update_client_pointer_focus(app, axis_event.time_msec());
                let output_name = route.as_ref().map(|route| route.output.name().to_string());
                let bypass_shortcuts = wayland::focus::current(&app.wayland)
                    .is_some_and(|focus| focus.bypasses_shortcuts());
                let mods = app
                    .seat
                    .get_keyboard()
                    .expect("keyboard capability added at seat setup")
                    .modifier_state();
                let result = process_wheel_bindings(
                    axis_event,
                    &mut app.wheel_accumulator,
                    !bypass_shortcuts,
                    |direction| match_wheel_bind(&app.keyboard.binds, &mods, direction),
                );
                for (direction, action) in result.actions {
                    eventline::debug!(
                        "keybinds: wheel {direction:?} + {mods:?} -> {action:?}"
                    );
                    dispatch_action(app, action, &socket_name, output_name.as_deref());
                }

                let pointer = app
                    .seat
                    .get_pointer()
                    .expect("pointer capability added at seat setup");
                if result.forward_horizontal || result.forward_vertical {
                    let frame = axis_frame_filtered(
                        axis_event,
                        result.forward_horizontal,
                        result.forward_vertical,
                    );
                    pointer.axis(app, frame);
                }
                pointer.frame(app);
            }

            // Drives the real seat directly (rather than a separate fake
            // one) so that whatever isn't intercepted as a configured bind
            // (`FilterResult::Forward`) actually reaches the focused client
            // - Smithay's own `KeyboardTarget<D> for WlSurface` impl handles
            // that forwarding for free once `App::KeyboardFocus = WlSurface`.
            if let InputEvent::Keyboard { event: key_event } = &event {
                app.wheel_accumulator.reset_all();
                let keycode = key_event.key_code();
                let state = key_event.state();
                let time = key_event.time_msec();
                let keyboard = app
                    .seat
                    .get_keyboard()
                    .expect("keyboard capability added at seat setup");
                let bypass_shortcuts = wayland::focus::current(&app.wayland)
                    .is_some_and(|focus| focus.bypasses_shortcuts());
                let action = keyboard.input::<halley_config::Action, _>(
                    app,
                    keycode,
                    state,
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |data, mods, handle| {
                        if state != KeyState::Pressed || bypass_shortcuts {
                            return FilterResult::Forward;
                        }
                        match match_keyboard_bind(
                            &data.keyboard.binds,
                            mods,
                            handle.raw_latin_sym_or_raw_current_sym(),
                            keycode,
                        ) {
                            Some(action) => FilterResult::Intercept(action),
                            None => FilterResult::Forward,
                        }
                    },
                );
                let pointer_output = app
                    .wayland
                    .space
                    .output_under(app.pointer.position())
                    .next()
                    .map(Output::name);

                if let Some(action) = action {
                    dispatch_action(app, action, &socket_name, pointer_output.as_deref());
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

fn on_vblank(
    app: &mut TtyApp,
    crtc: crtc::Handle,
    metadata: Option<&DrmEventMetadata>,
) {
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
    state.clock.presented(presented);
    let redraw_needed = match std::mem::take(&mut state.redraw) {
        RedrawState::WaitingForVBlank { redraw_needed } => {
            redraw_needed || state.unfinished_animations
        }
        other => {
            eventline::warn!(
                "unexpected redraw state on vblank for {:?}: {other:?}",
                output.name()
            );
            true
        }
    };
    state.redraw = if redraw_needed {
        RedrawState::Queued
    } else {
        RedrawState::Idle
    };

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
    app.keyboard
        .reload(&config.keybinds, BackendKind::Tty);

    let redraw_all = app.decorations != config.decorations || app.zoom != config.zoom;
    app.decorations = config.decorations;
    app.zoom = config.zoom;
    app.window_open_animations.reload(config.animations);

    if app.driver.paused {
        app.driver.pending_output_config = Some(config.outputs);
    } else {
        apply_tty_output_config(app, &config.outputs);
    }

    if redraw_all {
        queue_redraw(app);
    }
}

fn apply_tty_output_config(
    app: &mut TtyApp,
    outputs_config: &[halley_config::OutputConfig],
) {
    let changes = app.driver.backend.apply_output_config(outputs_config);
    let mut layout_changed = false;

    for change in changes {
        if change.mode_changed {
            let interval = app
                .driver
                .backend
                .refresh_interval_for_output(&change.output);
            if let Some(state) = app.driver.output_frames.get_mut(&change.output) {
                state.clock = FrameClock::new(Some(interval));
                state.last_camera_sample = crate::frame_clock::monotonic_now();
                state.unfinished_animations = false;
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
        }

        if change.mode_changed || change.layout_changed {
            queue_output_redraw(app, &change.output);
        }
    }

    if layout_changed {
        app.wayland.space.refresh();
        update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
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
        .filter(|(_, state)| {
            matches!(
                state.redraw,
                RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued(_)
            )
        })
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
        let target = state.clock.next_presentation_time(now);
        let dt = target.saturating_sub(state.last_camera_sample);
        state.last_camera_sample = target;
        (target, dt)
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
    let animating = camera_animating || window_animating;
    let view_after = pointer_is_on_output
        .then(|| app.cameras.view(&output.name()))
        .flatten();
    if view_before != view_after {
        update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
        let pointer = app
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        pointer.frame(app);
    }

    let status = match app.driver.backend.render(
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
        },
    ) {
        Ok(status) => status,
        Err(err) => {
            eventline::error!("render failed for {:?}: {err}", output.name());
            RenderStatus::Skipped
        }
    };
    app.window_open_animations
        .cleanup(target_presentation_time);

    if status == RenderStatus::Submitted {
        let state = app
            .driver
            .output_frames
            .get_mut(output)
            .expect("rendered output has frame state");
        if let RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) =
            std::mem::take(&mut state.redraw)
        {
            loop_handle.remove(token);
        }
        state.unfinished_animations = animating;
        state.redraw = RedrawState::WaitingForVBlank {
            redraw_needed: animating,
        };
        return;
    }

    app.driver
        .output_frames
        .get_mut(output)
        .expect("rendered output has frame state")
        .unfinished_animations = animating;
    queue_estimated_vblank_timer(app, output, loop_handle);
}

fn queue_estimated_vblank_timer(
    app: &mut TtyApp,
    output: &Output,
    loop_handle: &LoopHandle<'_, TtyApp>,
) {
    let state = app
        .driver
        .output_frames
        .get_mut(output)
        .expect("estimated-vblank output has frame state");
    match std::mem::take(&mut state.redraw) {
        RedrawState::Idle | RedrawState::WaitingForVBlank { .. } => {
            unreachable!("queue_estimated_vblank_timer called from an unexpected redraw state")
        }
        RedrawState::Queued => {}
        // Already waiting on a timer - keep it rather than stacking a
        // second one for the same missing VBlank.
        RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
            state.redraw = RedrawState::WaitingForEstimatedVBlank(token);
            return;
        }
    }

    let now = crate::frame_clock::monotonic_now();
    let due = state.clock.next_presentation_time(now);
    let delay = due
        .saturating_sub(now)
        .max(Duration::from_millis(1));
    let output = output.clone();
    let token = loop_handle
        .insert_source(Timer::from_duration(delay), move |_, _, app| {
            on_estimated_vblank_timer(app, &output);
            TimeoutAction::Drop
        })
        .expect("failed to arm estimated-vblank timer");
    state.redraw = RedrawState::WaitingForEstimatedVBlank(token);
}

fn on_estimated_vblank_timer(app: &mut TtyApp, output: &Output) {
    let Some(state) = app.driver.output_frames.get_mut(output) else {
        return;
    };
    match std::mem::take(&mut state.redraw) {
        RedrawState::WaitingForEstimatedVBlank(_) if state.unfinished_animations => {
            state.redraw = RedrawState::Queued;
        }
        RedrawState::WaitingForEstimatedVBlank(_) => {}
        RedrawState::WaitingForEstimatedVBlankAndQueued(_) => {
            state.redraw = RedrawState::Queued;
        }
        other => {
            eventline::warn!(
                "unexpected redraw state on estimated-vblank timer for {:?}: {other:?}",
                output.name()
            );
        }
    }
}

fn reset_redraw_state(app: &mut TtyApp, loop_handle: &LoopHandle<'_, TtyApp>) {
    let now = crate::frame_clock::monotonic_now();
    for state in app.driver.output_frames.values_mut() {
        if let RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) =
            std::mem::take(&mut state.redraw)
        {
            loop_handle.remove(token);
        }
        state.clock.reset();
        state.last_camera_sample = now;
        state.unfinished_animations = false;
        state.redraw = RedrawState::Queued;
    }
}
