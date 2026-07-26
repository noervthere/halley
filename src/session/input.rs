use std::ffi::OsStr;

use smithay::backend::input::{
    ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::desktop::{Space, Window};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{ButtonEvent, MotionEvent, RelativeMotionEvent};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};

use super::{Session, SessionDriver, focus_layer, focus_window};
use crate::input::pointer::{axis_frame_filtered, process_wheel_bindings};
use crate::input::{
    PointerBindingResult, match_keyboard_bind, match_wheel_bind, process_pointer_binding,
};
use crate::wayland;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

fn output_at_pointer(
    space: &Space<Window>,
    position: (f64, f64),
) -> Option<(Output, Rectangle<i32, Logical>)> {
    let output = space.output_under(position).next()?.clone();
    let geometry = space.output_geometry(&output)?;
    Some((output, geometry))
}

pub(super) fn route_client_pointer<D: SessionDriver>(
    session: &Session<D>,
) -> Option<crate::input::pointer::PointerRoute> {
    crate::input::pointer::route_to_client(
        &session.wayland.space,
        &session.cameras,
        session.driver.primary_output(),
        session.pointer.position(),
    )
}

pub(super) fn update_client_pointer_focus<D: SessionDriver>(
    session: &mut Session<D>,
    time: u32,
) -> Option<crate::input::pointer::PointerRoute> {
    let route = route_client_pointer(session)?;
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    pointer.motion(
        session,
        route.focus.clone(),
        &MotionEvent {
            location: route.location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    Some(route)
}

fn dispatch_action<D: SessionDriver>(
    session: &mut Session<D>,
    action: halley_config::Action,
    socket_name: &OsStr,
    output_name: Option<&str>,
) {
    let camera = output_name.and_then(|name| session.cameras.get_mut(name));
    if super::dispatch_action(
        action,
        &session.wayland,
        session.keyboard.terminal_command(),
        socket_name,
        camera,
        &session.zoom,
    ) == super::SessionControl::Quit
    {
        session.driver.stop();
    }
}

pub fn handle<D, B>(
    session: &mut Session<D>,
    event: &InputEvent<B>,
    socket_name: &OsStr,
) where
    D: SessionDriver,
    B: InputBackend,
{
    let position_before = session.pointer.position();
    let pointer_handle = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    let active_constraint =
        super::pointer_constraints::active(session, &pointer_handle);
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
    let confined_position_allowed = active_constraint.as_ref().is_none_or(|constraint| {
        constraint.kind != super::pointer_constraints::ConstraintKind::Confined
            || super::pointer_constraints::allows_current_position(session, constraint)
    });
    let motion_disposition = super::pointer_constraints::motion_disposition(
        active_constraint.as_ref().map(|constraint| constraint.kind),
        confined_position_allowed,
    );

    if motion_disposition == super::pointer_constraints::MotionDisposition::RelativeOnly
        && let Some(constraint) = active_constraint.as_ref()
        && let Some((delta, delta_unaccel, time, _)) = motion
    {
        session.pointer.set_position(position_before);
        pointer_handle.relative_motion(
            session,
            Some((constraint.surface.clone(), constraint.origin)),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime: time,
            },
        );
        pointer_handle.frame(session);
        return;
    }

    if motion_disposition == super::pointer_constraints::MotionDisposition::Hold {
        session.pointer.set_position(position_before);
    }
    let position_after = session.pointer.position();
    session.request_redraw();

    match &session.grab {
        crate::input::grab::Grab::MoveWindow {
            window,
            screen_offset,
        } => {
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
                    crate::input::grab::screen_offset_to_world(*screen_offset, camera);
                let new_location = Point::<i32, Logical>::from((
                    (world.x + world_offset.x).round() as i32,
                    (world.y + world_offset.y).round() as i32,
                ));
                wayland::set_window_output(window, &output);
                session
                    .wayland
                    .space
                    .map_element(window.clone(), new_location, false);
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
            }
        }
        crate::input::grab::Grab::None => {}
    }

    if let Some((delta, delta_unaccel, time, time_msec)) = motion {
        let route = update_client_pointer_focus(session, time_msec);
        pointer_handle.relative_motion(
            session,
            route.and_then(|route| route.focus),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime: time,
            },
        );
        pointer_handle.frame(session);
    }

    if let InputEvent::PointerButton {
        event: button_event,
    } = event
    {
        let button = button_event.button_code();
        let state = button_event.state();
        let time = button_event.time_msec();
        let serial = SERIAL_COUNTER.next_serial();
        let route = update_client_pointer_focus(session, time);
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
        let bindings_enabled = !wayland::focus::current(&session.wayland)
            .is_some_and(|focus| focus.bypasses_shortcuts());
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
                let output_name = route
                    .as_ref()
                    .map(|route| route.output.name().to_string());
                dispatch_action(session, action, socket_name, output_name.as_deref());
                intercepted = true;
            }
            PointerBindingResult::SuppressedRelease => intercepted = true,
            PointerBindingResult::Unhandled => {}
        }

        if !intercepted && button == BTN_RIGHT {
            match state {
                ButtonState::Pressed => {
                    if crate::input::mod_key_held(
                        &modifiers,
                        session.keyboard.effective_mod,
                    ) && let Some(crate::input::pointer::PointerRoute {
                        target: crate::input::pointer::PointerTarget::Window(window),
                        location,
                        ..
                    }) = route.as_ref()
                        && let Some(start_rect) =
                            session.wayland.space.element_geometry(window)
                    {
                        let world = halley_core::field::Vec2 {
                            x: location.x as f32,
                            y: location.y as f32,
                        };
                        let handle =
                            crate::input::grab::handle_from_press_position(start_rect, world);
                        focus_window(session, window, serial);
                        session.grab =
                            crate::input::grab::Grab::ResizeWindow(
                                crate::input::grab::ResizeState {
                                    window: window.clone(),
                                    handle,
                                    start_rect,
                                    start_cursor: world,
                                },
                            );
                        session.resize_anchor = Some(crate::input::grab::ResizeAnchor {
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
                    if matches!(
                        session.grab,
                        crate::input::grab::Grab::ResizeWindow(_)
                    ) {
                        session.grab = crate::input::grab::Grab::None;
                        crate::input::grab::release_resize_anchor(
                            &mut session.resize_anchor,
                        );
                        intercepted = true;
                    }
                }
            }
        } else if !intercepted && button == BTN_LEFT {
            match state {
                ButtonState::Pressed => {
                    let mod_held =
                        crate::input::mod_key_held(&modifiers, session.keyboard.effective_mod);
                    match route.as_ref().map(|route| &route.target) {
                        Some(crate::input::pointer::PointerTarget::Window(window))
                            if mod_held =>
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
                            focus_window(session, window, serial);
                            session.grab = crate::input::grab::Grab::MoveWindow {
                                window: window.clone(),
                                screen_offset,
                            };
                            intercepted = true;
                        }
                        Some(crate::input::pointer::PointerTarget::Window(window)) => {
                            focus_window(session, window, serial);
                        }
                        Some(crate::input::pointer::PointerTarget::Layer(layer)) => {
                            focus_layer(session, Some(layer.clone()), serial);
                        }
                        Some(crate::input::pointer::PointerTarget::Background) => {
                            focus_layer(session, None, serial);
                            session.grab = crate::input::grab::Grab::Pan {
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
                        session.grab,
                        crate::input::grab::Grab::MoveWindow { .. }
                            | crate::input::grab::Grab::Pan { .. }
                    ) {
                        session.grab = crate::input::grab::Grab::None;
                        intercepted = true;
                    }
                }
            }
        }

        if !intercepted {
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.button(
                session,
                &ButtonEvent {
                    serial,
                    time,
                    button,
                    state,
                },
            );
        }
        let pointer = session
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        pointer.frame(session);
    }

    if let InputEvent::PointerAxis { event: axis_event } = event {
        let route = update_client_pointer_focus(session, axis_event.time_msec());
        let output_name = route
            .as_ref()
            .map(|route| route.output.name().to_string());
        let bindings_enabled = !wayland::focus::current(&session.wayland)
            .is_some_and(|focus| focus.bypasses_shortcuts());
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
            eventline::debug!(
                "keybinds: wheel {direction:?} + {modifiers:?} -> {action:?}"
            );
            dispatch_action(session, action, socket_name, output_name.as_deref());
        }

        let pointer = session
            .seat
            .get_pointer()
            .expect("pointer capability added at seat setup");
        if result.forward_horizontal || result.forward_vertical {
            let frame = axis_frame_filtered(
                axis_event,
                result.forward_horizontal,
                result.forward_vertical,
            );
            pointer.axis(session, frame);
        }
        pointer.frame(session);
    }

    if let InputEvent::Keyboard { event: key_event } = event {
        session.wheel_accumulator.reset_all();
        let keycode = key_event.key_code();
        let state = key_event.state();
        let time = key_event.time_msec();
        let keyboard = session
            .seat
            .get_keyboard()
            .expect("keyboard capability added at seat setup");
        let bindings_enabled = !wayland::focus::current(&session.wayland)
            .is_some_and(|focus| focus.bypasses_shortcuts());
        let action = keyboard.input::<halley_config::Action, _>(
            session,
            keycode,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |data, modifiers, handle| {
                if state != KeyState::Pressed || !bindings_enabled {
                    return FilterResult::Forward;
                }
                match match_keyboard_bind(
                    &data.keyboard.binds,
                    modifiers,
                    handle.raw_latin_sym_or_raw_current_sym(),
                    keycode,
                ) {
                    Some(action) => FilterResult::Intercept(action),
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

        if let Some(action) = action {
            dispatch_action(session, action, socket_name, pointer_output.as_deref());
        }
    }
}
