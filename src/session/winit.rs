use std::ffi::OsString;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use smithay::backend::input::{
    ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::input::{Seat, SeatState};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
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
use crate::input::{Keyboard, PointerBindingResult, SuppressedButtons};
use crate::input::keybinds::BackendKind;
use crate::input::{match_keyboard_bind, match_wheel_bind, process_pointer_binding};
use crate::input::pointer::{
    Pointer, WheelAccumulator, axis_frame_filtered, process_wheel_bindings,
};
use crate::ipc::OutputInfoSource;
use crate::wayland::{self, WaylandState};

use super::{Session, SessionDriver, focus_layer, focus_window};

/// From `<linux/input-event-codes.h>` - the left mouse button's raw code.
/// Compared against `button_code()` rather than using
/// `PointerButtonEvent::button()`'s `MouseButton` enum, since the tty
/// backend's underlying libinput event type has an inherent `button()`
/// returning a raw `u32` that shadows the trait method of the same name -
/// using the same raw-code comparison in both backends keeps them
/// consistent rather than relying on which one wins per backend.
const BTN_LEFT: u32 = 0x110;
/// The right mouse button, same source and same reasoning as `BTN_LEFT`.
const BTN_RIGHT: u32 = 0x111;

fn route_client_pointer(app: &App) -> Option<crate::input::pointer::PointerRoute> {
    crate::input::pointer::route_to_client(
        &app.wayland.space,
        &app.cameras,
        app.driver.backend.output(),
        app.pointer.position(),
    )
}

fn update_client_pointer_focus(
    app: &mut App,
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
    app: &mut App,
    action: halley_config::Action,
    socket_name: &OsString,
    output_name: &str,
) {
    let camera = app
        .cameras
        .get_mut(output_name)
        .expect("winit output camera initialized at startup");
    if super::dispatch_action(
        action,
        &app.wayland,
        app.keyboard.terminal_command(),
        socket_name,
        Some(camera),
        &app.zoom,
    ) == super::SessionControl::Quit
    {
        app.driver.exit = true;
    }
}

struct WinitDriver {
    backend: WinitBackend,
    exit: bool,
    last_camera_tick: Instant,
}

impl SessionDriver for WinitDriver {
    fn primary_output(&self) -> &smithay::output::Output {
        self.backend.output()
    }

    fn request_redraw(&mut self, _output: Option<&smithay::output::Output>) {
        self.backend.request_redraw();
    }
}

type App = Session<WinitDriver>;

fn apply_runtime_config(app: &mut App, config: halley_config::RuntimeConfig) {
    app.keyboard
        .reload(&config.keybinds, BackendKind::Winit);
    let redraw = app.decorations != config.decorations || app.zoom != config.zoom;
    app.decorations = config.decorations;
    app.zoom = config.zoom;
    app.window_open_animations.reload(config.animations);
    if redraw {
        app.request_redraw();
    }
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
                    update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
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
                update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
                let pointer = app
                    .seat
                    .get_pointer()
                    .expect("pointer capability added at seat setup");
                pointer.frame(app);
                app.request_redraw();
            }
            WinitEvent::Input(event) => {
                let output_size_physical = app.driver.backend.window_size();
                let output_name = app.driver.backend.output().name();
                let position_before = app.pointer.position();
                app.pointer.process_input_event(&event, &app.wayland.space);
                let position_after = app.pointer.position();
                // Motion alone doesn't trigger a Redraw - request one so the
                // cursor visibly follows the mouse instead of only moving on
                // the next unrelated redraw.
                app.request_redraw();

                // Apply whatever's being dragged, if anything - reuses the
                // delta `Pointer` already computed (handles relative and
                // absolute motion, and clamping, uniformly) rather than
                // re-deriving it from the raw event per grab kind.
                match &app.grab {
                    crate::input::grab::Grab::MoveWindow {
                        window,
                        screen_offset,
                    } => {
                        let camera = app
                            .cameras
                            .get(&output_name)
                            .expect("winit output camera initialized at startup");
                        let world =
                            crate::input::grab::screen_to_world(position_after, camera, output_size_physical);
                        let world_offset = crate::input::grab::screen_offset_to_world(
                            *screen_offset,
                            camera,
                        );
                        let new_location = Point::<i32, Logical>::from((
                            (world.x + world_offset.x).round() as i32,
                            (world.y + world_offset.y).round() as i32,
                        ));
                        app.wayland.space.map_element(window.clone(), new_location, false);
                    }
                    crate::input::grab::Grab::Pan { output } => {
                        let dx = position_after.0 - position_before.0;
                        let dy = position_after.1 - position_before.1;
                        if let Some(camera) = app.cameras.get_mut(output) {
                            let delta =
                                crate::input::grab::screen_delta_to_world(dx, dy, camera);
                            // Negated - content follows the cursor ("natural
                            // drag") on the output where this drag began.
                            camera.pan_target(halley_core::field::Vec2 {
                                x: -delta.x,
                                y: -delta.y,
                            });
                        }
                    }
                    crate::input::grab::Grab::ResizeWindow(state) => {
                        let camera = app
                            .cameras
                            .get(&output_name)
                            .expect("winit output camera initialized at startup");
                        let world =
                            crate::input::grab::screen_to_world(position_after, camera, output_size_physical);
                        let size = crate::input::grab::resize_target_size(
                            state.handle,
                            state.start_rect,
                            state.start_cursor,
                            world,
                        );
                        if let Some(toplevel) = state.window.toplevel() {
                            toplevel.with_pending_state(|pending| pending.size = Some(size));
                            // No-ops unless the pending state actually
                            // changed, so this is safe to call per motion
                            // event rather than rate-limiting it here.
                            let serial = toplevel.send_pending_configure();
                            crate::input::grab::note_resize_configure(
                                &mut app.resize_anchor,
                                serial,
                            );
                        }
                    }
                    crate::input::grab::Grab::None => {}
                }

                let motion_time = match &event {
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
                            dispatch_action(app, action, &socket_name, &output_name);
                            intercepted = true;
                        }
                        PointerBindingResult::SuppressedRelease => intercepted = true,
                        PointerBindingResult::Unhandled => {}
                    }

                    if !intercepted && button == BTN_RIGHT {
                        match state {
                            ButtonState::Pressed => {
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
                                        app.resize_anchor =
                                            Some(crate::input::grab::ResizeAnchor {
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
                                    crate::input::grab::release_resize_anchor(
                                        &mut app.resize_anchor,
                                    );
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
                                        let camera = app
                                            .cameras
                                            .get(&route.output.name())
                                            .expect("routed output camera initialized");
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
                    update_client_pointer_focus(app, axis_event.time_msec());
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
                        dispatch_action(app, action, &socket_name, &output_name);
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
                // one) so that whatever isn't intercepted as a configured
                // bind (`FilterResult::Forward`) actually reaches the
                // focused client - Smithay's own `KeyboardTarget<D> for
                // WlSurface` impl handles that forwarding for free once
                // `App::KeyboardFocus = WlSurface`.
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

                    if let Some(action) = action {
                        dispatch_action(app, action, &socket_name, &output_name);
                    }
                }
            }
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
