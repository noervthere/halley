use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode as CalloopMode, PostAction};
use smithay::backend::input::{
    ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as smithay_winit, WinitEvent};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
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
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_xdg_decoration,
    delegate_xdg_shell,
};

use crate::backend::winit::WinitBackend;
use crate::backend::{self, Renderable};
use crate::cursor::CursorImage;
use crate::input::{Keyboard, PointerBindingResult, SuppressedButtons};
use crate::input::keybinds::BackendKind;
use crate::input::{match_keyboard_bind, match_wheel_bind, process_pointer_binding};
use crate::input::pointer::{
    Pointer, WheelAccumulator, axis_frame_filtered, process_wheel_bindings,
};
use crate::ipc::OutputInfoSource;
use crate::wayland::{self, ClientState, WaylandState};

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
        app.backend.output(),
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

fn sync_keyboard_focus(app: &mut App, serial: Serial) {
    let focused = wayland::focus::current(&app.wayland).map(|focus| focus.surface());
    let keyboard = app
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup");
    keyboard.set_focus(app, focused, serial);
}

fn focus_layer(
    app: &mut App,
    layer: Option<smithay::desktop::LayerSurface>,
    serial: Serial,
) {
    wayland::focus::select_layer(&mut app.wayland, layer);
    sync_keyboard_focus(app, serial);
}

fn focus_window(app: &mut App, window: &smithay::desktop::Window, serial: Serial) {
    wayland::xdg_shell::focus_and_raise(&mut app.wayland, window);
    sync_keyboard_focus(app, serial);
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
        app.exit = true;
    }
}

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
    cameras: crate::camera::OutputCameras,
    zoom: halley_config::Zoom,
    last_camera_tick: Instant,
    grab: crate::input::grab::Grab,
    /// Outlives `grab` on purpose - a released resize keeps anchoring until
    /// the client answers the last configure. See `grab::ResizePhase`.
    resize_anchor: Option<crate::input::grab::ResizeAnchor>,
    suppressed_buttons: SuppressedButtons,
    wheel_accumulator: WheelAccumulator,
    window_open_animations: crate::animation::WindowOpenAnimations,
}

fn apply_runtime_config(app: &mut App, config: halley_config::RuntimeConfig) {
    app.keyboard
        .reload(&config.keybinds, BackendKind::Winit);
    let redraw = app.decorations != config.decorations || app.zoom != config.zoom;
    app.decorations = config.decorations;
    app.zoom = config.zoom;
    app.window_open_animations.reload(config.animations);
    if redraw {
        app.backend.request_redraw();
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
        backend: winit_backend,
        keyboard: Keyboard::from_config(&runtime_config.keybinds, BackendKind::Winit),
        pointer: Pointer::new((100.0, 100.0)),
        cursor: CursorImage::load(),
        exit: false,
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
        last_camera_tick: Instant::now(),
        grab: crate::input::grab::Grab::None,
        resize_anchor: None,
        suppressed_buttons: SuppressedButtons::default(),
        wheel_accumulator: WheelAccumulator::default(),
        window_open_animations: crate::animation::WindowOpenAnimations::new(
            runtime_config.animations,
        ),
    };
    app.wayland.space.map_output(app.backend.output(), (0, 0));

    let socket_name = init_wayland_listener(display, &mut event_loop);
    println!("halley (winit) starting, WAYLAND_DISPLAY={socket_name:?}");

    if let Err(err) = crate::ipc::init_ipc_listener(&event_loop.handle(), |app: &App| app.backend.output_info()) {
        eprintln!("ipc: failed to start listener: {err}");
    }
    if let Some(path) = config_path
        && let Err(err) = crate::config::watch(&event_loop.handle(), path, apply_runtime_config)
    {
        eprintln!("config: failed to start watcher: {err}");
    }

    event_loop
        .handle()
        .insert_source(winit_source, move |event, _, app| match event {
            WinitEvent::CloseRequested => {
                app.exit = true;
            }
            WinitEvent::Redraw => {
                let now = Instant::now();
                let target_presentation_time = crate::frame_clock::monotonic_now();
                let dt = now.duration_since(app.last_camera_tick).as_secs_f32();
                app.last_camera_tick = now;
                let output_name = app.backend.output().name();
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
                let output = app.backend.output().clone();
                if let Err(err) = app.backend.render(
                    &output,
                    target_presentation_time,
                    backend::CLEAR_COLOR,
                    &app.cursor,
                    position,
                    &app.wayland.space,
                    app.wayland.focused_window.as_ref(),
                    &app.decorations,
                    &app.cameras,
                    &app.window_open_animations,
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
                wayland::layer_shell::send_frames(&output, elapsed);
                app.wayland.space.refresh();
                wayland::layer_shell::cleanup(&mut app.wayland);
                app.window_open_animations
                    .cleanup(target_presentation_time);

                if animating || window_animating {
                    app.backend.request_redraw();
                }
            }
            WinitEvent::Resized { .. } => {
                // No separate size-tracking needed: render() re-queries
                // window_size() every call, and WinitGraphicsBackend::bind()
                // already resizes the EGL surface internally when it
                // differs from the last bound size. Just need a new frame,
                // plus the advertised wl_output mode kept in sync.
                app.backend.update_output_mode();
                smithay::desktop::layer_map_for_output(app.backend.output()).arrange();
                // Simplification: snap zoom and pan back to rest at the new
                // size rather than preserving the current state across a
                // resize - resizing mid-zoom/pan is a rare dev-only edge
                // case, not worth the extra math.
                let output_size = app.backend.window_size();
                app.cameras.reset(app.backend.output().name(), output_size);
                update_client_pointer_focus(app, app.start_time.elapsed().as_millis() as u32);
                let pointer = app
                    .seat
                    .get_pointer()
                    .expect("pointer capability added at seat setup");
                pointer.frame(app);
                app.backend.request_redraw();
            }
            WinitEvent::Input(event) => {
                let output_size_physical = app.backend.window_size();
                let output_name = app.backend.output().name();
                let position_before = app.pointer.position();
                app.pointer.process_input_event(&event, &app.wayland.space);
                let position_after = app.pointer.position();
                // Motion alone doesn't trigger a Redraw - request one so the
                // cursor visibly follows the mouse instead of only moving on
                // the next unrelated redraw.
                app.backend.request_redraw();

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
                        eprintln!(
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
        if let Some(mapped) =
            wayland::compositor::commit::<Self>(&mut self.wayland, &self.cameras, surface)
        {
            self.window_open_animations
                .start(mapped, crate::frame_clock::monotonic_now());
        }
        crate::input::grab::finish_resize_commit(
            &mut self.resize_anchor,
            &mut self.wayland.space,
        );
        self.backend.request_redraw();

        sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
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
        self.window_open_animations.remove(surface.wl_surface());
        crate::input::grab::forget_resize_anchor(&mut self.resize_anchor, surface.wl_surface());
        wayland::xdg_shell::toplevel_destroyed(&mut self.wayland, &surface);
        self.backend.request_redraw();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        wayland::popup::track(&mut self.wayland, surface);
        self.backend.request_redraw();
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        wayland::popup::reposition(&self.wayland, surface, positioner, token);
        self.backend.request_redraw();
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let seat = Seat::<Self>::from_resource(&seat).expect("popup grab used an unknown wl_seat");
        let grab =
            wayland::popup::begin_grab(&mut self.wayland.popup_manager, &seat, surface, serial);
        if let Some(grab) = grab {
            wayland::popup::install_grab(self, &seat, grab, serial);
        }
    }
}

impl WlrLayerShellHandler for App {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.wayland.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(smithay::output::Output::from_resource)
            .or_else(|| Some(self.backend.output().clone()));
        wayland::layer_shell::new_surface(&mut self.wayland, surface, output, namespace);
        self.backend.request_redraw();
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        wayland::layer_shell::destroyed(&mut self.wayland, &surface);
        sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.backend.request_redraw();
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        wayland::popup::unconstrain_surface(&self.wayland, popup);
        self.backend.request_redraw();
    }
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

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = self.wayland.display_handle.clone();
        crate::wayland::selection::sync_selection_focus(&dh, seat, focused);
    }
}

impl OutputHandler for App {}

/// `()` - nothing here ever *sets* a selection on behalf of the compositor
/// (no clipboard manager, no XWayland bridge yet), so there's no server-side
/// selection to attach data to. Every selection in play is owned by a client,
/// and Smithay passes those through without consulting this type.
impl SelectionHandler for App {
    type SelectionUserData = ();
}

impl DataDeviceHandler for App {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.wayland.data_device_state
    }
}

impl WaylandDndGrabHandler for App {
    fn dnd_requested<S: smithay::input::dnd::Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: smithay::input::dnd::GrabType,
    ) {
        // `_icon` ignored: rendering the drag icon that follows the cursor
        // needs a render pass that knows about it, which is real work beyond
        // making drags function. Drags work without it, just without visual
        // feedback under the cursor.
        let dh = self.wayland.display_handle.clone();
        crate::wayland::selection::start_dnd_grab(self, &dh, source, seat, serial, type_);
    }
}

impl smithay::input::dnd::DndGrabHandler for App {}

impl PrimarySelectionHandler for App {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.wayland.primary_selection_state
    }
}

delegate_compositor!(App);
delegate_shm!(App);
delegate_xdg_shell!(App);
delegate_layer_shell!(App);
delegate_xdg_decoration!(App);
delegate_seat!(App);
delegate_output!(App);
delegate_data_device!(App);
delegate_primary_selection!(App);
