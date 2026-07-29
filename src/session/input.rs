use std::ffi::OsStr;

use smithay::backend::input::{
    ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::desktop::{Space, Window};
use smithay::input::keyboard::{FilterResult, Keysym};
use smithay::input::pointer::{ButtonEvent, RelativeMotionEvent};
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;
use smithay::wayland::seat::WaylandFocus;

use super::{Session, SessionDriver};
use crate::input::pointer::{axis_frame_filtered, process_wheel_bindings};
use crate::input::{
    PointerBindingResult, match_keyboard_bind, match_wheel_bind, process_pointer_binding,
};
use crate::wayland;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const NODE_DRAG_THRESHOLD_PX: f64 = 8.0;

fn sampled_drag_velocity(
    previous: halley_core::field::Vec2,
    current: halley_core::field::Vec2,
    previous_velocity: halley_core::field::Vec2,
    last_update: std::time::Duration,
    now: std::time::Duration,
) -> halley_core::field::Vec2 {
    let dt = now.saturating_sub(last_update).as_secs_f32();
    if dt <= f32::EPSILON {
        return previous_velocity;
    }
    let clamp = |value: f32| value.clamp(-800.0, 800.0);
    halley_core::field::Vec2 {
        x: previous_velocity.x * 0.35 + clamp((current.x - previous.x) / dt) * 0.65,
        y: previous_velocity.y * 0.35 + clamp((current.y - previous.y) / dt) * 0.65,
    }
}

fn shortcut_policy_allows_bindings(focus_bypasses_shortcuts: bool, inhibitor_active: bool) -> bool {
    !focus_bypasses_shortcuts && !inhibitor_active
}

fn bindings_enabled<D: SessionDriver>(session: &Session<D>) -> bool {
    let focus = wayland::focus::current(
        &session.wayland,
        &session.fullscreen,
        crate::frame_clock::monotonic_now(),
    );
    let bypasses_shortcuts = focus
        .as_ref()
        .is_some_and(|focus| focus.bypasses_shortcuts());
    let inhibitor_active = focus
        .map(|focus| focus.surface())
        .and_then(|surface| {
            session
                .seat
                .keyboard_shortcuts_inhibitor_for_surface(&surface)
        })
        .is_some_and(|inhibitor| inhibitor.is_active());
    shortcut_policy_allows_bindings(bypasses_shortcuts, inhibitor_active)
}

fn output_at_pointer(
    space: &Space<Window>,
    position: (f64, f64),
) -> Option<(Output, Rectangle<i32, Logical>)> {
    let output = space.output_under(position).next()?.clone();
    let geometry = space.output_geometry(&output)?;
    Some((output, geometry))
}

fn node_at_pointer<D: SessionDriver>(
    session: &Session<D>,
) -> Option<(halley_core::field::NodeId, Output)> {
    let position = session.pointer.position();
    let (output, geometry) = output_at_pointer(&session.wayland.space, position)?;
    let camera = session.cameras.get(&output.name())?;
    let id = session.nodes.hit_test(
        &output,
        geometry,
        camera,
        Point::<f64, Logical>::from(position),
    )?;
    Some((id, output))
}

fn dispatch_action<D: SessionDriver>(
    session: &mut Session<D>,
    action: halley_config::Action,
    socket_name: &OsStr,
    output_name: Option<&str>,
) {
    let zoom_action = matches!(
        &action,
        halley_config::Action::ZoomIn
            | halley_config::Action::ZoomOut
            | halley_config::Action::ZoomReset
    );
    let x11_display = session.xwayland.display_name();
    let camera = output_name.and_then(|name| session.cameras.get_mut(name));
    match super::dispatch_action(
        action,
        session.keyboard.terminal_command(),
        super::SpawnContext {
            socket_name,
            x11_display: x11_display.as_deref(),
            cursor_theme: session.cursor.theme_name(),
            cursor_size: session.cursor.size(),
            environment: &session.launch_environment,
        },
        camera,
        &session.zoom,
    ) {
        super::SessionControl::Continue => {}
        super::SessionControl::Quit => session.driver.stop(),
        super::SessionControl::CloseFocusedWindow => crate::nodes::close_focused(session),
        super::SessionControl::ToggleFullscreen => super::toggle_focused_fullscreen(session),
        super::SessionControl::ToggleState => {
            crate::nodes::toggle_focused(session, SERIAL_COUNTER.next_serial())
        }
        super::SessionControl::Screenshot => {
            let window_available = session.wayland.space.elements().any(|window| {
                window.wl_surface().is_some()
                    && crate::wayland::window_output_name(window)
                        .map(|name| Some(name.as_str()) == output_name)
                        .unwrap_or_else(|| {
                            output_name
                                .is_some_and(|name| name == session.driver.primary_output().name())
                        })
            });
            if session
                .capture
                .begin_menu(&session.wayland.space, output_name, window_available)
            {
                session.grab = crate::input::grab::Grab::None;
                let keyboard = session
                    .seat
                    .get_keyboard()
                    .expect("keyboard capability added at seat setup");
                keyboard.set_focus(session, None, SERIAL_COUNTER.next_serial());
                session.request_redraw();
            }
        }
    }
    if zoom_action && let Some(output_name) = output_name {
        let scale = session
            .cameras
            .get(output_name)
            .map(crate::camera::target_scale);
        if let Some(scale) = scale {
            crate::nodes::reconcile_landmarks_at_scale(session, output_name, scale);
        }
    }
}

enum KeyboardOutcome {
    Action(halley_config::Action),
    AccessibilityIntercept,
    CaptureAccept,
    CaptureCancel,
    CapturePrevious,
    CaptureNext,
    CaptureIntercept,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureKeyRouting {
    Evaluate,
    RetireUnfocusedRelease,
    SuppressRelease,
}

fn capture_key_routing(
    capture_active: bool,
    state: KeyState,
    release_is_suppressed: bool,
) -> CaptureKeyRouting {
    if state == KeyState::Released && release_is_suppressed {
        CaptureKeyRouting::SuppressRelease
    } else if capture_active && state == KeyState::Released {
        CaptureKeyRouting::RetireUnfocusedRelease
    } else {
        CaptureKeyRouting::Evaluate
    }
}

pub fn handle<D, B>(session: &mut Session<D>, event: &InputEvent<B>, socket_name: &OsStr)
where
    D: SessionDriver,
    B: InputBackend,
{
    if super::touch::handle(session, event) || super::gesture::handle(session, event) {
        return;
    }
    let position_before = session.pointer.position();
    if matches!(
        event,
        InputEvent::PointerMotion { .. }
            | InputEvent::PointerMotionAbsolute { .. }
            | InputEvent::PointerButton { .. }
            | InputEvent::PointerAxis { .. }
    ) && session.cursor_policy.pointer_activity()
    {
        session.request_redraw();
    }
    let pointer_handle = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    super::pointer::finish_frame(session, &pointer_handle);
    session
        .pointer
        .process_input_event(event, &session.wayland.space);
    let proposed_position = session.pointer.position();
    let motion = match event {
        InputEvent::PointerMotion { event } => Some((
            event.delta(),
            event.delta_unaccel(),
            event.time(),
            event.time_msec(),
        )),
        InputEvent::PointerMotionAbsolute { event } => {
            let delta = Point::<f64, Logical>::from((
                proposed_position.0 - position_before.0,
                proposed_position.1 - position_before.1,
            ));
            Some((delta, delta, event.time(), event.time_msec()))
        }
        _ => None,
    };
    if session.capture.is_active() {
        match event {
            InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. } => {
                update_capture_pointer(session, proposed_position);
                session.request_redraw();
                return;
            }
            InputEvent::PointerButton { event } => {
                if event.button_code() == BTN_LEFT {
                    update_capture_pointer(session, proposed_position);
                    match event.state() {
                        ButtonState::Pressed => {
                            session.suppressed_buttons.suppress(BTN_LEFT);
                            match session.capture.press(proposed_position) {
                                Some(crate::capture::CapturePress::Activate(mode)) => {
                                    if session.capture.activate_menu(mode, &session.wayland.space) {
                                        update_capture_pointer(session, proposed_position);
                                    }
                                }
                                Some(crate::capture::CapturePress::Accept) => {
                                    if crate::capture::accept_selected(session) {
                                        super::sync_keyboard_focus(
                                            session,
                                            SERIAL_COUNTER.next_serial(),
                                        );
                                    }
                                }
                                Some(crate::capture::CapturePress::Consumed) | None => {}
                            }
                        }
                        ButtonState::Released => {
                            session.suppressed_buttons.release_is_suppressed(BTN_LEFT);
                            session.capture.release();
                        }
                    }
                }
                session.request_redraw();
                return;
            }
            InputEvent::PointerAxis { .. } => return,
            _ => {}
        }
    }
    let constrained_motion = super::pointer::constrain_motion(session, &pointer_handle);

    if let super::pointer::ConstrainedMotion::RelativeOnly { surface, origin } = &constrained_motion
        && let Some((delta, delta_unaccel, time, _)) = motion
    {
        session.pointer.set_position(position_before);
        pointer_handle.relative_motion(
            session,
            Some((surface.clone(), *origin)),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime: time,
            },
        );
        super::pointer::finish_frame(session, &pointer_handle);
        return;
    }

    if matches!(constrained_motion, super::pointer::ConstrainedMotion::Hold) {
        session.pointer.set_position(position_before);
    }
    let position_after = session.pointer.position();
    session.request_redraw();

    if motion.is_some()
        && let crate::input::grab::Grab::PendingNode {
            id,
            surface,
            press_screen,
            screen_offset,
            ..
        } = &session.grab
    {
        let dx = position_after.0 - press_screen.x;
        let dy = position_after.1 - press_screen.y;
        if dx.hypot(dy) >= NODE_DRAG_THRESHOLD_PX {
            let id = *id;
            let surface = surface.clone();
            let screen_offset = *screen_offset;
            if let Some((output, output_geometry)) =
                output_at_pointer(&session.wayland.space, position_after)
                && let Some(camera) = session.cameras.get(&output.name())
            {
                let pointer_world = crate::input::grab::screen_to_world_on_output(
                    position_after,
                    camera,
                    output_geometry,
                );
                let offset = crate::input::grab::screen_offset_to_world(screen_offset, camera);
                let desired = halley_core::field::Vec2 {
                    x: pointer_world.x + offset.x,
                    y: pointer_world.y + offset.y,
                };
                crate::nodes::set_collapsed_output(session, id, &output);
                session.nodes.clear_direct_motion(id);
                session.grab = crate::input::grab::Grab::MoveNode {
                    id,
                    surface,
                    screen_offset,
                    last_world: desired,
                    last_update: crate::frame_clock::monotonic_now(),
                    velocity: halley_core::field::Vec2 { x: 0.0, y: 0.0 },
                };
                session
                    .cursor
                    .set_override(Some(smithay::input::pointer::CursorIcon::Grabbing));
            }
        }
    }

    match &session.grab {
        crate::input::grab::Grab::MoveWindow {
            id,
            window,
            screen_offset,
            last_world,
            last_update,
            velocity,
        } => {
            let id = *id;
            let window = window.clone();
            let screen_offset = *screen_offset;
            let previous = *last_world;
            let last_update = *last_update;
            let previous_velocity = *velocity;
            if let Some((output, output_geometry)) =
                output_at_pointer(&session.wayland.space, position_after)
            {
                let Some(camera) = session.cameras.get(&output.name()) else {
                    return;
                };
                let world = crate::input::grab::screen_to_world_on_output(
                    position_after,
                    camera,
                    output_geometry,
                );
                let world_offset =
                    crate::input::grab::screen_offset_to_world(screen_offset, camera);
                let desired_location = Point::<i32, Logical>::from((
                    (world.x + world_offset.x).round() as i32,
                    (world.y + world_offset.y).round() as i32,
                ));
                let output_name = output.name();
                let output_changed =
                    wayland::window_output_name(&window).as_deref() != Some(output_name.as_str());
                wayland::set_window_output(&window, &output);
                let size = session
                    .wayland
                    .space
                    .element_geometry(&window)
                    .map(|geometry| geometry.size)
                    .unwrap_or((1, 1).into());
                let desired_center = halley_core::field::Vec2 {
                    x: desired_location.x as f32 + size.w as f32 * 0.5,
                    y: desired_location.y as f32 + size.h as f32 * 0.5,
                };
                let now = crate::frame_clock::monotonic_now();
                let sampled = sampled_drag_velocity(
                    previous,
                    desired_center,
                    previous_velocity,
                    last_update,
                    now,
                );
                if let Some(id) = id {
                    if !session.nodes.physics.enabled {
                        let _ = crate::nodes::move_grabbed_body_rigid(session, id, desired_center);
                    }
                } else {
                    session
                        .wayland
                        .space
                        .map_element(window.clone(), desired_location, false);
                }
                if let crate::input::grab::Grab::MoveWindow {
                    last_world,
                    last_update,
                    velocity,
                    ..
                } = &mut session.grab
                {
                    *last_world = desired_center;
                    *last_update = now;
                    *velocity = sampled;
                }
                if id.is_some() && session.nodes.physics.enabled {
                    session.request_redraw();
                }
                if output_changed {
                    wayland::popup::update_reactive_for_window(
                        &session.wayland,
                        &session.cameras,
                        &window,
                    );
                }
            }
        }
        crate::input::grab::Grab::MoveNode {
            id,
            screen_offset,
            last_world,
            last_update,
            velocity,
            ..
        } => {
            let id = *id;
            let screen_offset = *screen_offset;
            let previous = *last_world;
            let last_update = *last_update;
            let previous_velocity = *velocity;
            if let Some((output, output_geometry)) =
                output_at_pointer(&session.wayland.space, position_after)
                && let Some(camera) = session.cameras.get(&output.name())
            {
                let pointer_world = crate::input::grab::screen_to_world_on_output(
                    position_after,
                    camera,
                    output_geometry,
                );
                let offset = crate::input::grab::screen_offset_to_world(screen_offset, camera);
                let desired = halley_core::field::Vec2 {
                    x: pointer_world.x + offset.x,
                    y: pointer_world.y + offset.y,
                };
                let now = crate::frame_clock::monotonic_now();
                let sampled =
                    sampled_drag_velocity(previous, desired, previous_velocity, last_update, now);
                crate::nodes::set_collapsed_output(session, id, &output);
                if !session.nodes.physics.enabled {
                    let _ = crate::nodes::move_grabbed_body_rigid(session, id, desired);
                }
                if let crate::input::grab::Grab::MoveNode {
                    last_world,
                    last_update,
                    velocity,
                    ..
                } = &mut session.grab
                {
                    *last_world = desired;
                    *last_update = now;
                    *velocity = sampled;
                }
                if session.nodes.physics.enabled {
                    session.request_redraw();
                }
            }
        }
        crate::input::grab::Grab::Pan { output } => {
            let dx = position_after.0 - position_before.0;
            let dy = position_after.1 - position_before.1;
            if let Some(camera) = session.cameras.get_mut(output) {
                let delta = crate::input::grab::screen_delta_to_world(dx, dy, camera);
                camera.pan_target(halley_core::field::Vec2 {
                    x: -delta.x,
                    y: -delta.y,
                });
            }
        }
        crate::input::grab::Grab::ResizeWindow(state) => {
            let primary = session.driver.primary_output();
            let output = session
                .wayland
                .space
                .outputs()
                .find(|output| wayland::window_is_on_output(&state.window, output, primary))
                .cloned()
                .unwrap_or_else(|| primary.clone());
            let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
                return;
            };
            let Some(camera) = session.cameras.get(&output.name()) else {
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
                let serial = toplevel.send_pending_configure();
                crate::input::grab::note_resize_configure(&mut session.resize_anchor, serial);
            } else {
                let location = crate::input::grab::resize_location_after_commit(
                    state.handle,
                    state.start_rect.loc,
                    state.start_rect.size,
                    size,
                );
                crate::xwayland::configure_window(&state.window, Rectangle::new(location, size));
            }
        }
        crate::input::grab::Grab::None | crate::input::grab::Grab::PendingNode { .. } => {}
    }

    if let Some((delta, delta_unaccel, time, time_msec)) = motion {
        let route = super::pointer::route_for_motion(session, time_msec);
        pointer_handle.relative_motion(
            session,
            route.as_ref().and_then(|route| route.focus.clone()),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime: time,
            },
        );
        super::pointer::finish_frame(session, &pointer_handle);
        if let Some(route) = route.as_ref() {
            super::focus::update_hover(session, route, SERIAL_COUNTER.next_serial());
        }
        let hovered_node = node_at_pointer(session);
        if let Some((id, output)) = hovered_node.as_ref() {
            super::focus::focus_node_from_hover(session, *id, output, SERIAL_COUNTER.next_serial());
        }
        let hovered = hovered_node.map(|(id, _)| id);
        if session.nodes.hovered != hovered {
            session.nodes.hovered = hovered;
            session.request_redraw();
        }
    }

    if let InputEvent::PointerButton {
        event: button_event,
    } = event
    {
        let button = button_event.button_code();
        let state = button_event.state();
        let time = button_event.time_msec();
        let serial = SERIAL_COUNTER.next_serial();
        if button == BTN_LEFT && state == ButtonState::Released {
            match &session.grab {
                crate::input::grab::Grab::PendingNode { id, .. } => {
                    let id = *id;
                    session.grab = crate::input::grab::Grab::None;
                    session.cursor.set_override(None);
                    let _ = crate::nodes::restore(session, id, serial);
                    super::pointer::finish_frame(session, &pointer_handle);
                    return;
                }
                crate::input::grab::Grab::MoveNode { id, .. } => {
                    let id = *id;
                    if session.nodes.physics.enabled {
                        let _ = crate::nodes::tick_physics(
                            session,
                            crate::frame_clock::monotonic_now(),
                        );
                    }
                    session.grab = crate::input::grab::Grab::None;
                    session.cursor.set_override(None);
                    session.nodes.clear_direct_motion(id);
                    session.request_redraw();
                    super::pointer::finish_frame(session, &pointer_handle);
                    return;
                }
                _ => {}
            }
        }
        let route = super::pointer::route_for_discrete_input(session, time);
        if button == BTN_LEFT
            && state == ButtonState::Pressed
            && let Some(route) = route.as_ref()
        {
            wayland::focus::select_output(&mut session.wayland, &route.output);
        }
        let mut intercepted = false;
        let modifiers = session
            .seat
            .get_keyboard()
            .expect("keyboard capability added at seat setup")
            .modifier_state();
        let bindings_enabled = bindings_enabled(session);
        let on_background = route.as_ref().is_some_and(|route| {
            matches!(
                &route.target,
                crate::input::pointer::PointerTarget::Background
            )
        });
        match process_pointer_binding(
            &session.keyboard.binds,
            &modifiers,
            button,
            state,
            on_background,
            bindings_enabled,
            &mut session.suppressed_buttons,
        ) {
            PointerBindingResult::Action(action) => {
                let output_name = route.as_ref().map(|route| route.output.name().to_string());
                dispatch_action(session, action, socket_name, output_name.as_deref());
                intercepted = true;
            }
            PointerBindingResult::SuppressedRelease => intercepted = true,
            PointerBindingResult::Unhandled => {}
        }

        if !intercepted
            && button == BTN_LEFT
            && state == ButtonState::Pressed
            && let Some((id, output)) = node_at_pointer(session)
            && let Some(record) = session.nodes.record(id).cloned()
            && let Some(node_position) = session.nodes.field.node(id).map(|node| node.pos)
            && let Some(output_geometry) = session.wayland.space.output_geometry(&output)
            && let Some(camera) = session.cameras.get(&output.name())
        {
            wayland::focus::select_output(&mut session.wayland, &output);
            let center = crate::nodes::screen_from_world(node_position, camera, output_geometry);
            let screen_offset = halley_core::field::Vec2 {
                x: center.x as f32 - position_after.0 as f32,
                y: center.y as f32 - position_after.1 as f32,
            };
            let mod_held = crate::input::mod_key_held(&modifiers, session.keyboard.effective_mod);
            if mod_held {
                session.nodes.clear_direct_motion(id);
                session.grab = crate::input::grab::Grab::MoveNode {
                    id,
                    surface: record.surface,
                    screen_offset,
                    last_world: node_position,
                    last_update: crate::frame_clock::monotonic_now(),
                    velocity: halley_core::field::Vec2 { x: 0.0, y: 0.0 },
                };
                session
                    .cursor
                    .set_override(Some(smithay::input::pointer::CursorIcon::Grabbing));
            } else {
                session.grab = crate::input::grab::Grab::PendingNode {
                    id,
                    surface: record.surface,
                    press_screen: Point::<f64, Logical>::from(position_after),
                    screen_offset,
                };
            }
            intercepted = true;
        }

        if !intercepted && button == BTN_RIGHT {
            match state {
                ButtonState::Pressed => {
                    if crate::input::mod_key_held(&modifiers, session.keyboard.effective_mod)
                        && let Some(crate::input::pointer::PointerRoute {
                            target: crate::input::pointer::PointerTarget::Window(window),
                            location,
                            ..
                        }) = route.as_ref()
                        && !window.wl_surface().is_some_and(|surface| {
                            session
                                .fullscreen
                                .is_fullscreen_or_pending(surface.as_ref())
                        })
                    {
                        let world = halley_core::field::Vec2 {
                            x: location.x as f32,
                            y: location.y as f32,
                        };
                        if let Some(start_rect) = session.wayland.space.element_geometry(window) {
                            let handle =
                                crate::input::grab::handle_from_press_position(start_rect, world);
                            intercepted = super::begin_window_resize(
                                session, window, handle, button, world, serial,
                            );
                        }
                    }
                }
                ButtonState::Released => {}
            }
        } else if !intercepted && button == BTN_LEFT {
            match state {
                ButtonState::Pressed => {
                    let mod_held =
                        crate::input::mod_key_held(&modifiers, session.keyboard.effective_mod);
                    match route.as_ref().map(|route| &route.target) {
                        Some(crate::input::pointer::PointerTarget::Window(window))
                            if mod_held
                                && !window.wl_surface().is_some_and(|surface| {
                                    session
                                        .fullscreen
                                        .is_fullscreen_or_pending(surface.as_ref())
                                }) =>
                        {
                            let route = route.as_ref().expect("matched above");
                            let world = halley_core::field::Vec2 {
                                x: route.location.x as f32,
                                y: route.location.y as f32,
                            };
                            let window_location = session
                                .wayland
                                .space
                                .element_location(window)
                                .expect("routed window is mapped");
                            let Some(camera) = session.cameras.get(&route.output.name()) else {
                                return;
                            };
                            let scale = crate::input::zoom::scale(camera);
                            let screen_offset = halley_core::field::Vec2 {
                                x: (window_location.x as f32 - world.x) * scale,
                                y: (window_location.y as f32 - world.y) * scale,
                            };
                            super::focus::focus_window_from_pointer(session, window, serial);
                            let id = window
                                .wl_surface()
                                .and_then(|surface| session.nodes.id_for_surface(surface.as_ref()));
                            if let Some(id) = id {
                                session.nodes.clear_direct_motion(id);
                            }
                            let center = session
                                .wayland
                                .space
                                .element_geometry(window)
                                .map(|geometry| halley_core::field::Vec2 {
                                    x: geometry.loc.x as f32 + geometry.size.w as f32 * 0.5,
                                    y: geometry.loc.y as f32 + geometry.size.h as f32 * 0.5,
                                })
                                .unwrap_or(world);
                            session.grab = crate::input::grab::Grab::MoveWindow {
                                id,
                                window: window.clone(),
                                screen_offset,
                                last_world: center,
                                last_update: crate::frame_clock::monotonic_now(),
                                velocity: halley_core::field::Vec2 { x: 0.0, y: 0.0 },
                            };
                            session
                                .cursor
                                .set_override(Some(smithay::input::pointer::CursorIcon::Grabbing));
                            intercepted = true;
                        }
                        Some(crate::input::pointer::PointerTarget::Window(window)) => {
                            super::focus::focus_window_from_pointer(session, window, serial);
                        }
                        Some(crate::input::pointer::PointerTarget::Layer(layer)) => {
                            super::focus::focus_layer(session, Some(layer.clone()), serial);
                        }
                        Some(crate::input::pointer::PointerTarget::Background) => {
                            super::focus::focus_layer(session, None, serial);
                            session.grab = crate::input::grab::Grab::Pan {
                                output: route.as_ref().expect("matched above").output.name(),
                            };
                            intercepted = true;
                        }
                        None => {}
                    }
                }
                ButtonState::Released => {
                    let released_window = match &session.grab {
                        crate::input::grab::Grab::MoveWindow { id, .. } => Some(*id),
                        _ => None,
                    };
                    if let Some(id) = released_window {
                        let now = crate::frame_clock::monotonic_now();
                        if session.nodes.physics.enabled {
                            let _ = crate::nodes::tick_physics(session, now);
                        }
                        session.grab = crate::input::grab::Grab::None;
                        session.cursor.set_override(None);
                        if session.nodes.physics.enabled
                            && let Some(id) = id
                        {
                            session.nodes.lock_released_window(id, now);
                            session.request_redraw();
                        }
                        intercepted = true;
                    } else if matches!(session.grab, crate::input::grab::Grab::Pan { .. }) {
                        session.grab = crate::input::grab::Grab::None;
                        session.cursor.set_override(None);
                        intercepted = true;
                    }
                }
            }
        }

        if !intercepted
            && state == ButtonState::Released
            && matches!(
                &session.grab,
                crate::input::grab::Grab::ResizeWindow(resize) if resize.button == button
            )
        {
            session.grab = crate::input::grab::Grab::None;
            crate::input::grab::release_resize_anchor(&mut session.resize_anchor);
            intercepted = true;
        }

        if !intercepted {
            pointer_handle.button(
                session,
                &ButtonEvent {
                    serial,
                    time,
                    button,
                    state,
                },
            );
        }
        super::pointer::finish_frame(session, &pointer_handle);
    }

    if let InputEvent::PointerAxis { event: axis_event } = event {
        if super::gesture::handle_axis_pan(session, axis_event) {
            super::pointer::finish_frame(session, &pointer_handle);
            return;
        }
        let route = super::pointer::route_for_discrete_input(session, axis_event.time_msec());
        let output_name = route.as_ref().map(|route| route.output.name().to_string());
        let bindings_enabled = bindings_enabled(session);
        let modifiers = session
            .seat
            .get_keyboard()
            .expect("keyboard capability added at seat setup")
            .modifier_state();
        let result = process_wheel_bindings(
            axis_event,
            &mut session.wheel_accumulator,
            bindings_enabled,
            |direction| match_wheel_bind(&session.keyboard.binds, &modifiers, direction),
        );
        for (direction, action) in result.actions {
            eventline::debug!("keybinds: wheel {direction:?} + {modifiers:?} -> {action:?}");
            dispatch_action(session, action, socket_name, output_name.as_deref());
        }

        if result.forward_horizontal || result.forward_vertical {
            let frame = axis_frame_filtered(
                axis_event,
                result.forward_horizontal,
                result.forward_vertical,
            );
            pointer_handle.axis(session, frame);
        }
        super::pointer::finish_frame(session, &pointer_handle);
    }

    if let InputEvent::Keyboard { event: key_event } = event {
        session.wheel_accumulator.reset_all();
        let keycode = key_event.key_code();
        let state = key_event.state();
        let time = key_event.time_msec();
        if state == KeyState::Pressed && session.cursor_policy.keyboard_press() {
            session.request_redraw();
        }
        let release_is_suppressed =
            state == KeyState::Released && session.suppressed_keys.release_is_suppressed(keycode);
        let accessibility = crate::accessibility::process_key(
            session,
            std::time::Duration::from_millis(u64::from(time)),
            keycode,
            state,
        );
        let keyboard = session
            .seat
            .get_keyboard()
            .expect("keyboard capability added at seat setup");
        let bindings_enabled = bindings_enabled(session);
        let action = keyboard.input::<KeyboardOutcome, _>(
            session,
            keycode,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |data, modifiers, handle| {
                if accessibility == crate::accessibility::KeyboardDisposition::Intercept {
                    return FilterResult::Intercept(KeyboardOutcome::AccessibilityIntercept);
                }
                match capture_key_routing(data.capture.is_active(), state, release_is_suppressed) {
                    CaptureKeyRouting::SuppressRelease => {
                        return FilterResult::Intercept(KeyboardOutcome::CaptureIntercept);
                    }
                    CaptureKeyRouting::RetireUnfocusedRelease => {
                        // Focus is cleared for the lifetime of the overlay. Forwarding
                        // releases here reaches no client, but lets Smithay retire keys
                        // whose presses were forwarded before the modal opened.
                        return FilterResult::Forward;
                    }
                    CaptureKeyRouting::Evaluate => {}
                }
                if data.capture.is_active() {
                    return match handle.raw_latin_sym_or_raw_current_sym() {
                        Some(Keysym::Escape) => {
                            FilterResult::Intercept(KeyboardOutcome::CaptureCancel)
                        }
                        Some(Keysym::Return | Keysym::KP_Enter) => {
                            FilterResult::Intercept(KeyboardOutcome::CaptureAccept)
                        }
                        Some(Keysym::Left)
                            if data.capture.kind() == Some(crate::capture::CaptureKind::Menu) =>
                        {
                            FilterResult::Intercept(KeyboardOutcome::CapturePrevious)
                        }
                        Some(Keysym::Right)
                            if data.capture.kind() == Some(crate::capture::CaptureKind::Menu) =>
                        {
                            FilterResult::Intercept(KeyboardOutcome::CaptureNext)
                        }
                        _ => FilterResult::Intercept(KeyboardOutcome::CaptureIntercept),
                    };
                }
                if state != KeyState::Pressed || !bindings_enabled {
                    return FilterResult::Forward;
                }
                match match_keyboard_bind(
                    &data.keyboard.binds,
                    modifiers,
                    handle.raw_latin_sym_or_raw_current_sym(),
                    keycode,
                ) {
                    Some(action) => FilterResult::Intercept(KeyboardOutcome::Action(action)),
                    None => FilterResult::Forward,
                }
            },
        );
        let pointer_output = session
            .wayland
            .space
            .output_under(session.pointer.position())
            .next()
            .map(Output::name);

        match action {
            Some(KeyboardOutcome::Action(action)) => {
                session.suppressed_keys.suppress(keycode);
                dispatch_action(session, action, socket_name, pointer_output.as_deref());
            }
            Some(KeyboardOutcome::AccessibilityIntercept) => {}
            Some(KeyboardOutcome::CaptureAccept) => {
                if session.capture.kind() == Some(crate::capture::CaptureKind::Menu) {
                    if session
                        .capture
                        .activate_selected_menu(&session.wayland.space)
                    {
                        update_capture_pointer(session, session.pointer.position());
                        session.request_redraw();
                    }
                } else if crate::capture::accept_selected(session) {
                    session.suppressed_keys.suppress(keycode);
                    super::sync_keyboard_focus(session, SERIAL_COUNTER.next_serial());
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::CaptureCancel) => {
                if session.capture.return_to_menu() {
                    session.request_redraw();
                } else if crate::capture::cancel_selected(session) {
                    session.suppressed_keys.suppress(keycode);
                    super::sync_keyboard_focus(session, SERIAL_COUNTER.next_serial());
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::CapturePrevious) => {
                if session.capture.move_menu_selection(-1) {
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::CaptureNext) => {
                if session.capture.move_menu_selection(1) {
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::CaptureIntercept) | None => {}
        }
    }
}

fn update_capture_pointer<D: SessionDriver>(session: &mut Session<D>, position: (f64, f64)) {
    match session.capture.kind() {
        Some(crate::capture::CaptureKind::Menu | crate::capture::CaptureKind::Area) => {
            session.capture.motion(position);
        }
        Some(crate::capture::CaptureKind::Screen) => {
            if let Some((_, geometry)) = output_at_pointer(&session.wayland.space, position) {
                session.capture.hover_screen(geometry);
            }
        }
        Some(crate::capture::CaptureKind::Source) => {
            let Some(route) = super::pointer::route_client(session) else {
                return;
            };
            let Some(output_geometry) = session.wayland.space.output_geometry(&route.output) else {
                return;
            };
            let monitor = halley_ipc::CaptureSource::Monitor {
                name: route.output.name(),
                x: output_geometry.loc.x,
                y: output_geometry.loc.y,
                width: output_geometry.size.w,
                height: output_geometry.size.h,
            };
            let window = match route.target {
                crate::input::pointer::PointerTarget::Window(window) => {
                    window.wl_surface().and_then(|surface| {
                        let geometry = route.visual_geometry?;
                        let size = window.geometry().size;
                        Some((
                            halley_ipc::CaptureSource::Window {
                                surface_id: surface.id().protocol_id(),
                                width: size.w,
                                height: size.h,
                            },
                            geometry,
                        ))
                    })
                }
                _ => None,
            };
            session
                .capture
                .hover_source(monitor, window, output_geometry);
        }
        Some(crate::capture::CaptureKind::Window) => {
            let hovered = super::pointer::route_client(session).and_then(|route| {
                let crate::input::pointer::PointerTarget::Window(window) = route.target else {
                    return None;
                };
                let surface = window.wl_surface()?.into_owned();
                Some((surface, route.visual_geometry?))
            });
            let (surface, geometry) = hovered
                .map(|(surface, geometry)| (Some(surface), Some(geometry)))
                .unwrap_or((None, None));
            session.capture.hover_window(surface, geometry);
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use halley_core::field::Vec2;
    use smithay::backend::input::KeyState;

    use super::{
        CaptureKeyRouting, capture_key_routing, sampled_drag_velocity,
        shortcut_policy_allows_bindings,
    };

    fn sample_constant_motion(report_hz: u32) -> Vec2 {
        let step = Duration::from_secs_f64(1.0 / f64::from(report_hz));
        let mut previous = Vec2 { x: 0.0, y: 0.0 };
        let mut velocity = Vec2 { x: 0.0, y: 0.0 };
        let mut last = Duration::ZERO;
        for index in 1..=report_hz {
            let now = step * index;
            let current = Vec2 {
                x: 400.0 * now.as_secs_f32(),
                y: -180.0 * now.as_secs_f32(),
            };
            velocity = sampled_drag_velocity(previous, current, velocity, last, now);
            previous = current;
            last = now;
        }
        velocity
    }

    #[test]
    fn drag_velocity_is_stable_across_common_mouse_report_rates() {
        for report_hz in [125, 500, 1_000] {
            let sampled = sample_constant_motion(report_hz);
            assert!((sampled.x - 400.0).abs() < 0.1, "{report_hz} Hz");
            assert!((sampled.y + 180.0).abs() < 0.1, "{report_hz} Hz");
        }
    }

    #[test]
    fn drag_velocity_ignores_duplicate_timestamps() {
        let previous_velocity = Vec2 { x: 25.0, y: -15.0 };
        let sampled = sampled_drag_velocity(
            Vec2 { x: 10.0, y: 20.0 },
            Vec2 { x: 50.0, y: 80.0 },
            previous_velocity,
            Duration::from_millis(5),
            Duration::from_millis(5),
        );
        assert_eq!(sampled, previous_velocity);
    }

    #[test]
    fn shortcut_policy_respects_shell_and_client_inhibition() {
        assert!(shortcut_policy_allows_bindings(false, false));
        assert!(!shortcut_policy_allows_bindings(true, false));
        assert!(!shortcut_policy_allows_bindings(false, true));
        assert!(!shortcut_policy_allows_bindings(true, true));
    }

    #[test]
    fn modal_releases_retire_preexisting_forwarded_keys() {
        assert_eq!(
            capture_key_routing(true, KeyState::Released, false),
            CaptureKeyRouting::RetireUnfocusedRelease
        );
        assert_eq!(
            capture_key_routing(false, KeyState::Released, true),
            CaptureKeyRouting::SuppressRelease
        );
        assert_eq!(
            capture_key_routing(true, KeyState::Pressed, false),
            CaptureKeyRouting::Evaluate
        );
    }
}
