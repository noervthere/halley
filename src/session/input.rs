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

fn cluster_at_pointer<D: SessionDriver>(
    session: &Session<D>,
) -> Option<(halley_core::cluster::ClusterId, Output)> {
    let position = session.pointer.position();
    let (output, geometry) = output_at_pointer(&session.wayland.space, position)?;
    let camera = session.cameras.get(&output.name())?;
    let id = session.clusters.core_hit_test(
        &output.name(),
        camera,
        geometry,
        Point::<f64, Logical>::from(position),
    )?;
    Some((id, output))
}

fn cluster_overflow_at_pointer<D: SessionDriver>(
    session: &Session<D>,
) -> Option<(halley_core::field::NodeId, Output)> {
    let position = session.pointer.position();
    let (output, geometry) = output_at_pointer(&session.wayland.space, position)?;
    let work_area = smithay::desktop::layer_map_for_output(&output).non_exclusive_zone();
    let local = Point::<f64, Logical>::from((
        position.0 - f64::from(geometry.loc.x),
        position.1 - f64::from(geometry.loc.y),
    ));
    let member = session
        .clusters
        .overflow_hit_test(&output.name(), work_area, local)?;
    Some((member, output))
}

fn sync_cluster_activation_focus<D: SessionDriver>(
    session: &mut Session<D>,
    output: &Output,
    id: halley_core::cluster::ClusterId,
    serial: smithay::utils::Serial,
) {
    let Some(member) = session.clusters.first_member(id) else {
        return;
    };
    if session.clusters.active_on(&output.name()) == Some(id) {
        if let Some(window) = session
            .nodes
            .record(member)
            .map(|record| record.window.clone())
        {
            super::focus_window(session, &window, serial);
        }
    } else {
        crate::window::clear_focus(&mut session.wayland);
        session.nodes.focus(
            Some(member),
            session.start_time.elapsed().as_millis() as u64,
        );
        super::sync_keyboard_focus(session, serial);
    }
}

fn navigate_cluster_tile<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
    direction: halley_config::ClusterDirection,
    swap: bool,
) {
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == output_name)
        .cloned()
    else {
        return;
    };
    let work_area = smithay::desktop::layer_map_for_output(&output).non_exclusive_zone();
    let focused = session.nodes.focused();
    let target = if swap {
        session
            .clusters
            .swap_directional_tile(output_name, focused, direction, work_area)
    } else {
        session
            .clusters
            .directional_tile_target(output_name, focused, direction, work_area)
    };
    if let Some(window) = target
        .and_then(|id| session.nodes.record(id))
        .map(|record| record.window.clone())
    {
        super::focus_window(session, &window, SERIAL_COUNTER.next_serial());
        session.request_redraw();
    }
}

fn bearing_at_pointer<D: SessionDriver>(
    session: &Session<D>,
) -> Option<(halley_core::field::NodeId, Output)> {
    let position = session.pointer.position();
    let (output, geometry) = output_at_pointer(&session.wayland.space, position)?;
    let local = Point::<f64, Logical>::from((
        position.0 - f64::from(geometry.loc.x),
        position.1 - f64::from(geometry.loc.y),
    ));
    let id = session.bearings.hit_test(&output.name(), local)?;
    Some((id, output))
}

fn window_action_output(
    focus_mode: halley_config::FocusMode,
    pointer_output: Option<&str>,
    selected_output: Option<&str>,
) -> Option<String> {
    match focus_mode {
        halley_config::FocusMode::Hover => pointer_output.or(selected_output).map(str::to_owned),
        halley_config::FocusMode::Click => selected_output.map(str::to_owned),
    }
}

fn dispatch_action<D: SessionDriver>(
    session: &mut Session<D>,
    action: halley_config::Action,
    socket_name: &OsStr,
    output_name: Option<&str>,
    held_keycode: Option<u32>,
) {
    let zoom_action = matches!(
        &action,
        halley_config::Action::ZoomIn
            | halley_config::Action::ZoomOut
            | halley_config::Action::ZoomReset
    );
    let selected_output =
        crate::wayland::focus::selected_output(&session.wayland).map(Output::name);
    let action_output = window_action_output(
        session.input.focus_mode,
        output_name,
        selected_output.as_deref(),
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
        super::SessionControl::Quit => session.show_exit_confirmation(),
        super::SessionControl::CloseFocusedWindow => {
            crate::nodes::close_focused_on_output(session, action_output.as_deref())
        }
        super::SessionControl::ToggleFullscreen => {
            super::toggle_focused_fullscreen(session, action_output.as_deref())
        }
        super::SessionControl::ToggleFieldMaximize => {
            super::toggle_focused_field_maximize(session, action_output.as_deref())
        }
        super::SessionControl::ToggleState => crate::nodes::toggle_focused_on_output(
            session,
            action_output.as_deref(),
            SERIAL_COUNTER.next_serial(),
        ),
        super::SessionControl::Apogee => {
            crate::shell::apogee::toggle(session);
        }
        super::SessionControl::FocusCycle(direction) => {
            let cluster_member = action_output
                .as_deref()
                .and_then(|output| session.clusters.cycle_stack(output, direction));
            if let Some(window) = cluster_member
                .and_then(|id| session.nodes.record(id))
                .map(|record| record.window.clone())
            {
                super::focus_window(session, &window, SERIAL_COUNTER.next_serial());
                session.request_redraw();
            } else {
                crate::shell::focus_cycle::start_or_step(session, direction);
            }
        }
        super::SessionControl::ClusterMode => {
            if let Some(output) = action_output
                && session.clusters.begin_creation(output)
            {
                session.request_redraw();
            }
        }
        super::SessionControl::ClusterLayoutCycle => {
            if let Some(output) = action_output
                && session.clusters.cycle_active_layout(&output)
            {
                session.request_redraw();
            }
        }
        super::SessionControl::ClusterSlot(slot) => {
            if let Some(output_name) = action_output
                && session.clusters.activate_slot(&output_name, slot)
            {
                if let Some(id) = session.clusters.active_on(&output_name).or_else(|| {
                    session
                        .clusters
                        .clusters_for_output(&output_name)
                        .find_map(|(candidate_slot, id, _)| (candidate_slot == slot).then_some(id))
                }) {
                    let output = session
                        .wayland
                        .space
                        .outputs()
                        .find(|candidate| candidate.name() == output_name)
                        .cloned();
                    if let Some(output) = output {
                        sync_cluster_activation_focus(
                            session,
                            &output,
                            id,
                            SERIAL_COUNTER.next_serial(),
                        );
                    }
                }
                session.request_redraw();
            }
        }
        super::SessionControl::ClusterTileFocus(direction) => {
            if let Some(output) = action_output {
                navigate_cluster_tile(session, &output, direction, false);
            }
        }
        super::SessionControl::ClusterTileSwap(direction) => {
            if let Some(output) = action_output {
                navigate_cluster_tile(session, &output, direction, true);
            }
        }
        super::SessionControl::BearingsShow => {
            let changed = match held_keycode {
                Some(keycode) => session.bearings.show_while_held(keycode),
                None => session.bearings.set_visible(true),
            };
            if changed {
                session.request_redraw();
            }
        }
        super::SessionControl::BearingsToggle => {
            session.bearings.toggle();
            session.request_redraw();
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
            crate::capture::begin_local(session, output_name, window_available);
        }
    }
    if zoom_action && let Some(output_name) = output_name {
        let scale = session
            .cameras
            .get(output_name)
            .map(crate::presentation::camera::target_scale);
        if let Some(scale) = scale {
            crate::nodes::reconcile_landmarks_at_scale(session, output_name, scale);
        }
    }
}

enum KeyboardOutcome {
    Action(halley_config::Action),
    ExitConfirm,
    ExitCancel,
    ExitIntercept,
    AccessibilityIntercept,
    ApogeeCancel,
    ApogeeAccept,
    ApogeeMove(crate::shell::apogee::Direction),
    ApogeeIntercept,
    FocusCycleCancel,
    FocusCycleIntercept,
    CaptureAccept,
    CaptureCancel,
    CapturePrevious,
    CaptureNext,
    CaptureIntercept,
    BearingsRelease,
    ClusterAccept,
    ClusterCancel,
    ClusterBackspace,
    ClusterCharacter(char),
    ClusterIntercept,
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
    if is_user_activity(event) {
        let seat = session.seat.clone();
        session.idle_notifier_state.notify_activity(&seat);
    }
    if session.session_lock.active() {
        crate::wayland::session_lock::handle_input(session, event);
        return;
    }
    if session.overlays.exit_modal_active() && !matches!(event, InputEvent::Keyboard { .. }) {
        match event {
            InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. } => {
                session
                    .pointer
                    .process_input_event(event, &session.wayland.space);
                session.cursor_policy.pointer_activity();
                session.request_redraw();
            }
            InputEvent::PointerButton { event } => match event.state() {
                ButtonState::Pressed => session.suppressed_buttons.suppress(event.button_code()),
                ButtonState::Released => {
                    session
                        .suppressed_buttons
                        .release_is_suppressed(event.button_code());
                }
            },
            InputEvent::PointerAxis { .. } => session.wheel_accumulator.reset_all(),
            _ => {}
        }
        return;
    }
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
                                Some(crate::capture::CapturePress::ActivateScreenshot(mode)) => {
                                    if session.capture.activate_menu(mode, &session.wayland.space) {
                                        update_capture_pointer(session, proposed_position);
                                    }
                                }
                                Some(crate::capture::CapturePress::ActivateSource(mode)) => {
                                    if session.capture.activate_source(mode) {
                                        update_capture_pointer(session, proposed_position);
                                    }
                                }
                                Some(crate::capture::CapturePress::Accept) => {
                                    crate::capture::accept_selected(session);
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
    if session.apogee.accepts_input() {
        match event {
            InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. } => {
                crate::shell::apogee::pointer_motion(session, proposed_position);
            }
            InputEvent::PointerButton { event } if event.button_code() == BTN_LEFT => {
                match event.state() {
                    ButtonState::Pressed => {
                        session.suppressed_buttons.suppress(BTN_LEFT);
                        crate::shell::apogee::pointer_press(session, proposed_position);
                    }
                    ButtonState::Released => {
                        session.suppressed_buttons.release_is_suppressed(BTN_LEFT);
                    }
                }
            }
            InputEvent::PointerButton { .. } | InputEvent::PointerAxis { .. } => {}
            _ => {}
        }
        if matches!(
            event,
            InputEvent::PointerMotion { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
        ) {
            session.request_redraw();
            return;
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
                    if session
                        .clusters
                        .update_join_candidate(&output_name, id, desired_center, now)
                    {
                        session.request_redraw();
                    }
                } else {
                    session.clusters.cancel_join_candidate();
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
        let node_grab_active = matches!(
            &session.grab,
            crate::input::grab::Grab::PendingNode { .. }
                | crate::input::grab::Grab::MoveNode { .. }
        );
        let hovered_node = (!node_grab_active)
            .then(|| node_at_pointer(session))
            .flatten();
        if let Some((id, output)) = hovered_node.as_ref() {
            super::focus::focus_node_from_hover(session, *id, output, SERIAL_COUNTER.next_serial());
        }
        let hovered = hovered_node.map(|(id, _)| id);
        if session
            .nodes
            .set_hovered(hovered, crate::frame_clock::monotonic_now())
        {
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
        if session.clusters.accepts_modal_input() {
            if button == BTN_LEFT
                && state == ButtonState::Pressed
                && let Some(crate::input::pointer::PointerRoute {
                    output,
                    target: crate::input::pointer::PointerTarget::Window(window),
                    ..
                }) = route.as_ref()
                && let Some(surface) = window.wl_surface()
                && let Some(id) = session.nodes.id_for_surface(surface.as_ref())
                && session.clusters.toggle_creation_member(id, &output.name())
            {
                wayland::focus::select_output(&mut session.wayland, output);
                session.request_redraw();
            }
            if state == ButtonState::Pressed {
                session.suppressed_buttons.suppress(button);
            } else {
                session.suppressed_buttons.release_is_suppressed(button);
            }
            super::pointer::finish_frame(session, &pointer_handle);
            return;
        }
        if button == BTN_LEFT
            && state == ButtonState::Pressed
            && let Some(route) = route.as_ref()
        {
            wayland::focus::select_output(&mut session.wayland, &route.output);
        }
        let mut intercepted = false;
        if button == BTN_LEFT
            && state == ButtonState::Pressed
            && !session.focus_cycle.is_open()
            && let Some((member, output)) = cluster_overflow_at_pointer(session)
            && session
                .clusters
                .promote_overflow_member(&output.name(), member)
        {
            wayland::focus::select_output(&mut session.wayland, &output);
            if let Some(window) = session
                .nodes
                .record(member)
                .map(|record| record.window.clone())
            {
                super::focus_window(session, &window, serial);
            }
            session.suppressed_buttons.suppress(button);
            session.request_redraw();
            intercepted = true;
        }
        if button == BTN_LEFT
            && state == ButtonState::Pressed
            && !session.focus_cycle.is_open()
            && !intercepted
            && let Some((id, output)) = cluster_at_pointer(session)
        {
            wayland::focus::select_output(&mut session.wayland, &output);
            if session.clusters.activate(&output.name(), id) {
                sync_cluster_activation_focus(session, &output, id, serial);
                session.suppressed_buttons.suppress(button);
                session.request_redraw();
                intercepted = true;
            }
        }
        if button == BTN_LEFT
            && state == ButtonState::Pressed
            && !session.focus_cycle.is_open()
            && let Some((id, output)) = bearing_at_pointer(session)
        {
            wayland::focus::select_output(&mut session.wayland, &output);
            if crate::nodes::focus_or_restore_from_bearing(session, id, serial) {
                session.suppressed_buttons.suppress(button);
                intercepted = true;
            }
        }
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
        if !intercepted {
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
                    dispatch_action(session, action, socket_name, output_name.as_deref(), None);
                    intercepted = true;
                }
                PointerBindingResult::SuppressedRelease => intercepted = true,
                PointerBindingResult::Unhandled => {}
            }
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
            session
                .nodes
                .clear_hover(crate::frame_clock::monotonic_now());
            super::focus::focus_node_from_pointer(session, id, &output, serial);
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
                        && crate::window::accepts_compositor_grab(window)
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
                                && crate::window::accepts_compositor_grab(window)
                                && !window.wl_surface().is_some_and(|surface| {
                                    session
                                        .fullscreen
                                        .is_fullscreen_or_pending(surface.as_ref())
                                }) =>
                        {
                            intercepted = super::begin_pointer_move(session, window, serial);
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
                        let joined = id.and_then(|member| {
                            session.clusters.commit_join_candidate(
                                &mut session.nodes.field,
                                member,
                                now,
                            )
                        });
                        session.grab = crate::input::grab::Grab::None;
                        session.cursor.set_override(None);
                        if joined.is_some() {
                            if let Some(id) = id {
                                session.nodes.clear_direct_motion(id);
                            }
                            session.request_redraw();
                        } else if session.nodes.physics.enabled
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
            dispatch_action(session, action, socket_name, output_name.as_deref(), None);
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
        let accessibility = if session.overlays.exit_modal_active() {
            crate::accessibility::KeyboardDisposition::Pass
        } else {
            crate::accessibility::process_key(
                session,
                std::time::Duration::from_millis(u64::from(time)),
                keycode,
                state,
            )
        };
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
                if data.overlays.exit_modal_active() {
                    if state == KeyState::Released {
                        return if release_is_suppressed {
                            FilterResult::Intercept(KeyboardOutcome::ExitIntercept)
                        } else {
                            FilterResult::Forward
                        };
                    }
                    return match handle.raw_latin_sym_or_raw_current_sym() {
                        Some(Keysym::Return | Keysym::KP_Enter) => {
                            FilterResult::Intercept(KeyboardOutcome::ExitConfirm)
                        }
                        Some(Keysym::Escape) => {
                            FilterResult::Intercept(KeyboardOutcome::ExitCancel)
                        }
                        _ => FilterResult::Intercept(KeyboardOutcome::ExitIntercept),
                    };
                }
                if data.clusters.accepts_modal_input() {
                    if state == KeyState::Released {
                        return if release_is_suppressed {
                            FilterResult::Intercept(KeyboardOutcome::ClusterIntercept)
                        } else {
                            FilterResult::Forward
                        };
                    }
                    let sym = handle.modified_sym();
                    let outcome = match sym {
                        Keysym::Escape => KeyboardOutcome::ClusterCancel,
                        Keysym::Return | Keysym::KP_Enter => KeyboardOutcome::ClusterAccept,
                        Keysym::BackSpace => KeyboardOutcome::ClusterBackspace,
                        _ => sym
                            .key_char()
                            .map(KeyboardOutcome::ClusterCharacter)
                            .unwrap_or(KeyboardOutcome::ClusterIntercept),
                    };
                    return FilterResult::Intercept(outcome);
                }
                if state == KeyState::Released && data.bearings.is_show_key_held(keycode.raw()) {
                    return FilterResult::Intercept(KeyboardOutcome::BearingsRelease);
                }
                if data.apogee.accepts_input() {
                    let sym = handle.raw_latin_sym_or_raw_current_sym();
                    if state == KeyState::Released {
                        return FilterResult::Intercept(KeyboardOutcome::ApogeeIntercept);
                    }
                    let outcome = match sym {
                        Some(Keysym::Escape) => KeyboardOutcome::ApogeeCancel,
                        Some(Keysym::Return | Keysym::KP_Enter) => KeyboardOutcome::ApogeeAccept,
                        Some(Keysym::Left) => {
                            KeyboardOutcome::ApogeeMove(crate::shell::apogee::Direction::Left)
                        }
                        Some(Keysym::Right) => {
                            KeyboardOutcome::ApogeeMove(crate::shell::apogee::Direction::Right)
                        }
                        Some(Keysym::Up) => {
                            KeyboardOutcome::ApogeeMove(crate::shell::apogee::Direction::Up)
                        }
                        Some(Keysym::Down) => {
                            KeyboardOutcome::ApogeeMove(crate::shell::apogee::Direction::Down)
                        }
                        _ => {
                            if let Some(
                                action @ (halley_config::Action::Apogee
                                | halley_config::Action::Quit
                                | halley_config::Action::OpenTerminal),
                            ) =
                                match_keyboard_bind(&data.keyboard.binds, modifiers, sym, keycode)
                            {
                                KeyboardOutcome::Action(action)
                            } else {
                                KeyboardOutcome::ApogeeIntercept
                            }
                        }
                    };
                    return FilterResult::Intercept(outcome);
                }
                if data.focus_cycle.is_open() {
                    let sym = handle.raw_latin_sym_or_raw_current_sym();
                    if state == KeyState::Released
                        && matches!(sym, Some(Keysym::Alt_L | Keysym::Alt_R))
                    {
                        return FilterResult::Forward;
                    }
                    if state == KeyState::Pressed && sym == Some(Keysym::Escape) {
                        return FilterResult::Intercept(KeyboardOutcome::FocusCycleCancel);
                    }
                    if state == KeyState::Pressed
                        && let Some(action @ halley_config::Action::FocusCycle(_)) =
                            match_keyboard_bind(&data.keyboard.binds, modifiers, sym, keycode)
                    {
                        return FilterResult::Intercept(KeyboardOutcome::Action(action));
                    }
                    return FilterResult::Intercept(KeyboardOutcome::FocusCycleIntercept);
                }
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
                        Some(Keysym::Left) if data.capture.menu_is_active() => {
                            FilterResult::Intercept(KeyboardOutcome::CapturePrevious)
                        }
                        Some(Keysym::Right) if data.capture.menu_is_active() => {
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
            Some(KeyboardOutcome::ExitConfirm) => {
                session.suppressed_keys.suppress(keycode);
                session.confirm_exit();
            }
            Some(KeyboardOutcome::ExitCancel) => {
                session.suppressed_keys.suppress(keycode);
                session.cancel_exit_confirmation();
            }
            Some(KeyboardOutcome::ExitIntercept) => {
                if state == KeyState::Pressed {
                    session.suppressed_keys.suppress(keycode);
                }
            }
            Some(KeyboardOutcome::Action(action)) => {
                session.suppressed_keys.suppress(keycode);
                dispatch_action(
                    session,
                    action,
                    socket_name,
                    pointer_output.as_deref(),
                    Some(keycode.raw()),
                );
            }
            Some(KeyboardOutcome::BearingsRelease) => {
                if session.bearings.release_show_key(keycode.raw()) {
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::ClusterCancel) => {
                session.suppressed_keys.suppress(keycode);
                if session.clusters.cancel_creation() {
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::ClusterAccept) => {
                session.suppressed_keys.suppress(keycode);
                if session
                    .clusters
                    .creation()
                    .is_some_and(|creation| creation.naming)
                {
                    match session.clusters.finish_creation(&mut session.nodes.field) {
                        Ok(_) => {
                            crate::window::clear_focus(&mut session.wayland);
                            session
                                .nodes
                                .focus(None, session.start_time.elapsed().as_millis() as u64);
                            super::sync_keyboard_focus(session, SERIAL_COUNTER.next_serial());
                        }
                        Err(message) => eventline::warn!("clusters: {message}"),
                    }
                } else {
                    session.clusters.begin_naming();
                }
                session.request_redraw();
            }
            Some(KeyboardOutcome::ClusterBackspace) => {
                session.suppressed_keys.suppress(keycode);
                if session
                    .clusters
                    .edit_name(crate::clusters::NameInput::Backspace)
                {
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::ClusterCharacter(ch)) => {
                session.suppressed_keys.suppress(keycode);
                if session
                    .clusters
                    .edit_name(crate::clusters::NameInput::Character(ch))
                {
                    session.request_redraw();
                }
            }
            Some(KeyboardOutcome::ClusterIntercept) => {
                if state == KeyState::Pressed {
                    session.suppressed_keys.suppress(keycode);
                }
            }
            Some(KeyboardOutcome::AccessibilityIntercept) => {}
            Some(KeyboardOutcome::ApogeeCancel) => {
                session.suppressed_keys.suppress(keycode);
                crate::shell::apogee::cancel(session);
            }
            Some(KeyboardOutcome::ApogeeAccept) => {
                session.suppressed_keys.suppress(keycode);
                crate::shell::apogee::select(session);
            }
            Some(KeyboardOutcome::ApogeeMove(direction)) => {
                session.suppressed_keys.suppress(keycode);
                crate::shell::apogee::move_selection(session, direction);
            }
            Some(KeyboardOutcome::ApogeeIntercept) => {
                if state == KeyState::Pressed {
                    session.suppressed_keys.suppress(keycode);
                }
            }
            Some(KeyboardOutcome::FocusCycleCancel) => {
                session.suppressed_keys.suppress(keycode);
                crate::shell::focus_cycle::cancel(session);
            }
            Some(KeyboardOutcome::FocusCycleIntercept) => {
                if state == KeyState::Pressed {
                    session.suppressed_keys.suppress(keycode);
                }
            }
            Some(KeyboardOutcome::CaptureAccept) => {
                if session.capture.menu_is_active() {
                    if session
                        .capture
                        .activate_selected_menu(&session.wayland.space)
                    {
                        update_capture_pointer(session, session.pointer.position());
                        session.request_redraw();
                    }
                } else if crate::capture::accept_selected(session) {
                    session.suppressed_keys.suppress(keycode);
                }
            }
            Some(KeyboardOutcome::CaptureCancel) => {
                if session.capture.return_to_menu() {
                    session.request_redraw();
                } else if crate::capture::cancel_selected(session) {
                    session.suppressed_keys.suppress(keycode);
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

        if state == KeyState::Released
            && session.focus_cycle.is_open()
            && !keyboard.modifier_state().alt
        {
            crate::shell::focus_cycle::commit(session, SERIAL_COUNTER.next_serial());
        }
    }
}

fn is_user_activity<B: InputBackend>(event: &InputEvent<B>) -> bool {
    !matches!(
        event,
        InputEvent::DeviceAdded { .. }
            | InputEvent::DeviceRemoved { .. }
            | InputEvent::SwitchToggle { .. }
            | InputEvent::Special(_)
    )
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
        Some(crate::capture::CaptureKind::Source) if session.capture.menu_is_active() => {
            session.capture.motion(position);
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
        shortcut_policy_allows_bindings, window_action_output,
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
    fn window_actions_follow_pointer_only_in_hover_mode() {
        assert_eq!(
            window_action_output(halley_config::FocusMode::Hover, Some("right"), Some("left"),),
            Some("right".to_string())
        );
        assert_eq!(
            window_action_output(halley_config::FocusMode::Click, Some("right"), Some("left"),),
            Some("left".to_string())
        );
        assert_eq!(
            window_action_output(halley_config::FocusMode::Hover, None, Some("left")),
            Some("left".to_string())
        );
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
