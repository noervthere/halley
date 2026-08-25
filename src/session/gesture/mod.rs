mod camera;

use halley_config::{GestureModifier, GestureScope, ScrollPanMode};
use smithay::backend::input::{
    Axis, AxisSource, Event, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
    GestureSwipeUpdateEvent as _, InputBackend, InputEvent, PointerAxisEvent,
};
use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;

use super::{Session, SessionDriver};
use camera::{PanGesture, PinchGesture};

#[derive(Clone, Debug)]
enum Sequence<T> {
    Client(WlSurface),
    Compositor(T),
    Ignored,
}

#[derive(Clone, Debug)]
struct AxisPan {
    output: String,
    horizontal_active: bool,
    vertical_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApogeeSwipeMode {
    Open,
    Close,
}

#[derive(Clone, Debug)]
struct ApogeeSwipe {
    mode: ApogeeSwipeMode,
    net_y: f64,
    last_delta_y: f64,
    interactive_started: bool,
}

#[derive(Clone, Debug)]
enum SwipeGesture {
    Pan(PanGesture),
    Apogee(ApogeeSwipe),
}

#[derive(Default)]
pub(super) struct GestureState {
    swipe: Option<Sequence<SwipeGesture>>,
    pinch: Option<Sequence<PinchGesture>>,
    hold: Option<Sequence<()>>,
    axis_pan: Option<AxisPan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteChoice {
    Client,
    Compositor,
    Ignored,
}

#[derive(Clone, Copy, Debug)]
struct RoutePolicy {
    behavior_enabled: bool,
    client_available: bool,
    client_passthrough: bool,
    scope: GestureScope,
    modifier_forces: bool,
    client_forced: bool,
    blocked: bool,
}

impl RoutePolicy {
    fn choose(self) -> RouteChoice {
        if self.blocked {
            return RouteChoice::Ignored;
        }
        if self.client_forced {
            return if self.client_available && self.client_passthrough {
                RouteChoice::Client
            } else {
                RouteChoice::Ignored
            };
        }
        if !self.behavior_enabled {
            return if self.client_available && self.client_passthrough {
                RouteChoice::Client
            } else {
                RouteChoice::Ignored
            };
        }
        if self.client_available && self.scope == GestureScope::EmptyField && !self.modifier_forces
        {
            return if self.client_passthrough {
                RouteChoice::Client
            } else {
                RouteChoice::Ignored
            };
        }
        RouteChoice::Compositor
    }
}

struct BeginRoute {
    choice: RouteChoice,
    owner: Option<WlSurface>,
    output: Option<String>,
}

pub(super) fn handle<D, B>(session: &mut Session<D>, event: &InputEvent<B>) -> bool
where
    D: SessionDriver,
    B: InputBackend,
{
    match event {
        InputEvent::GestureSwipeBegin { event } => swipe_begin::<D, B>(session, event),
        InputEvent::GestureSwipeUpdate { event } => swipe_update::<D, B>(session, event),
        InputEvent::GestureSwipeEnd { event } => swipe_end::<D, B>(session, event),
        InputEvent::GesturePinchBegin { event } => pinch_begin::<D, B>(session, event),
        InputEvent::GesturePinchUpdate { event } => pinch_update::<D, B>(session, event),
        InputEvent::GesturePinchEnd { event } => pinch_end::<D, B>(session, event),
        InputEvent::GestureHoldBegin { event } => hold_begin::<D, B>(session, event),
        InputEvent::GestureHoldEnd { event } => hold_end::<D, B>(session, event),
        _ => return false,
    }
    true
}

fn begin_route<D: SessionDriver>(
    session: &mut Session<D>,
    time: u32,
    behavior_enabled: bool,
    scope: GestureScope,
) -> BeginRoute {
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    let route = super::pointer::route_for_discrete_input(session, time);
    super::pointer::finish_frame(session, &pointer);
    let owner = pointer.current_focus();
    let client_available = owner.is_some();
    let blocked = session.capture.is_active()
        || !matches!(session.interactions.grab, crate::input::grab::Grab::None);
    let client_forced = super::pointer::has_active_constraint(session) || pointer.is_grabbed();
    let choice = RoutePolicy {
        behavior_enabled: session.settings.input.gestures.enabled && behavior_enabled,
        client_available,
        client_passthrough: session.settings.input.gestures.client_passthrough,
        scope,
        modifier_forces: modifier_forces(session),
        client_forced,
        blocked,
    }
    .choose();
    BeginRoute {
        choice,
        owner,
        output: route.map(|route| route.output.name()),
    }
}

fn modifier_forces<D: SessionDriver>(session: &Session<D>) -> bool {
    let modifier = match session.settings.input.gestures.modifier {
        GestureModifier::Disabled => return false,
        GestureModifier::Keybind => session.keyboard.effective_mod,
        GestureModifier::Explicit(modifier) => {
            crate::input::keybinds::effective_mod(modifier, D::BACKEND_KIND)
        }
    };
    let modifiers = session
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup")
        .modifier_state();
    crate::input::mod_key_held(&modifiers, session.keyboard.side_modifiers, modifier)
}

fn owner_is_current<D: SessionDriver, T>(session: &Session<D>, sequence: &Sequence<T>) -> bool {
    let Sequence::Client(owner) = sequence else {
        return false;
    };
    session
        .seat
        .get_pointer()
        .and_then(|pointer| pointer.current_focus())
        .as_ref()
        == Some(owner)
}

fn swipe_begin<D, B>(session: &mut Session<D>, event: &B::GestureSwipeBeginEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let settings = &session.settings.input.gestures;
    let apogee_fingers = if session.shell.apogee.is_active() {
        settings.apogee_close_fingers
    } else {
        settings.apogee_open_fingers
    };
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    let apogee_blocked = session.capture.is_active()
        || !matches!(session.interactions.grab, crate::input::grab::Grab::None)
        || super::pointer::has_active_constraint(session)
        || pointer.is_grabbed();
    if settings.enabled
        && session.settings.apogee.enabled
        && event.fingers() == apogee_fingers
        && !apogee_blocked
    {
        session.gestures.swipe = Some(Sequence::Compositor(SwipeGesture::Apogee(ApogeeSwipe {
            mode: if session.shell.apogee.is_active() {
                ApogeeSwipeMode::Close
            } else {
                ApogeeSwipeMode::Open
            },
            net_y: 0.0,
            last_delta_y: 0.0,
            interactive_started: false,
        })));
        return;
    }
    let route = begin_route(
        session,
        event.time_msec(),
        settings.pan_fingers > 0 && event.fingers() == settings.pan_fingers,
        settings.compositor_scope,
    );
    session.gestures.swipe = Some(match route.choice {
        RouteChoice::Client => {
            let Some(owner) = route.owner else {
                return;
            };
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_swipe_begin(
                session,
                &GestureSwipeBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
            Sequence::Client(owner)
        }
        RouteChoice::Compositor => route
            .output
            .and_then(|output| {
                let camera = session.cameras.get_mut(&output)?;
                Some(Sequence::Compositor(SwipeGesture::Pan(PanGesture::new(
                    output, camera,
                ))))
            })
            .unwrap_or(Sequence::Ignored),
        RouteChoice::Ignored => Sequence::Ignored,
    });
}

fn swipe_update<D, B>(session: &mut Session<D>, event: &B::GestureSwipeUpdateEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(mut sequence) = session.gestures.swipe.take() else {
        return;
    };
    let client_owner_is_current = owner_is_current(session, &sequence);
    match &mut sequence {
        Sequence::Client(_) if client_owner_is_current => {
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_swipe_update(
                session,
                &GestureSwipeUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                },
            );
        }
        Sequence::Client(_) => sequence = Sequence::Ignored,
        Sequence::Compositor(SwipeGesture::Pan(gesture)) => {
            if let Some(camera) = session.cameras.get_mut(&gesture.output) {
                let delta = event.delta();
                gesture.update(camera, event.time_msec(), delta.x, delta.y);
                session.request_redraw();
            } else {
                sequence = Sequence::Ignored;
            }
        }
        Sequence::Compositor(SwipeGesture::Apogee(gesture)) => {
            let delta = event.delta();
            gesture.net_y += delta.y;
            gesture.last_delta_y = delta.y;
            if gesture.mode == ApogeeSwipeMode::Open {
                const DEADZONE_PX: f64 = 8.0;
                const OPEN_TRAVEL_PX: f64 = 320.0;
                let travel = (-gesture.net_y - DEADZONE_PX).max(0.0);
                if travel > 0.0 && !gesture.interactive_started {
                    session.nodes.sync_from_space(&session.wayland.space);
                    gesture.interactive_started = session.shell.apogee.begin_interactive(
                        &session.wayland.space,
                        &session.nodes,
                        &session.clusters,
                        session.settings.apogee,
                    );
                    if gesture.interactive_started {
                        session.cursor.set_override(
                            crate::cursor::OverrideSource::Modal,
                            Some(smithay::input::pointer::CursorIcon::Default),
                        );
                        super::note_pointer_activity(session);
                    }
                }
                if gesture.interactive_started {
                    session
                        .shell
                        .apogee
                        .set_interactive_progress((travel / OPEN_TRAVEL_PX) as f32);
                    session.request_redraw();
                }
            }
        }
        Sequence::Ignored => {}
    }
    session.gestures.swipe = Some(sequence);
}

fn swipe_end<D, B>(session: &mut Session<D>, event: &B::GestureSwipeEndEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(sequence) = session.gestures.swipe.take() else {
        return;
    };
    match sequence {
        Sequence::Client(_) if owner_is_current(session, &sequence) => {
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_swipe_end(
                session,
                &GestureSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        Sequence::Compositor(SwipeGesture::Pan(gesture)) => {
            if let Some(camera) = session.cameras.get_mut(&gesture.output) {
                gesture.finish(
                    camera,
                    event.cancelled(),
                    session.settings.input.gestures.pan_momentum,
                    session.settings.input.gestures.flick_min_px_per_s,
                );
                session.request_redraw();
            }
        }
        Sequence::Compositor(SwipeGesture::Apogee(gesture)) => match gesture.mode {
            ApogeeSwipeMode::Open if gesture.interactive_started => {
                const DEADZONE_PX: f64 = 8.0;
                const OPEN_TRAVEL_PX: f64 = 320.0;
                let progress =
                    ((-gesture.net_y - DEADZONE_PX).max(0.0) / OPEN_TRAVEL_PX).clamp(0.0, 1.0);
                let upward_flick = gesture.last_delta_y < -10.0;
                let commit = !event.cancelled() && (progress >= 0.4 || upward_flick);
                session.shell.apogee.finish_interactive(
                    commit,
                    session.settings.apogee,
                    crate::frame_clock::monotonic_now(),
                );
                session.request_redraw();
            }
            ApogeeSwipeMode::Open => {}
            ApogeeSwipeMode::Close => {
                if !event.cancelled()
                    && gesture.net_y >= session.settings.input.gestures.swipe_threshold_px as f64
                {
                    crate::shell::apogee::cancel(session);
                }
            }
        },
        Sequence::Client(_) | Sequence::Ignored => {}
    }
}

fn pinch_begin<D, B>(session: &mut Session<D>, event: &B::GesturePinchBeginEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let settings = &session.settings.input.gestures;
    let route = begin_route(
        session,
        event.time_msec(),
        settings.pinch_to_zoom && session.settings.zoom.enabled,
        settings.pinch_scope,
    );
    session.gestures.pinch = Some(match route.choice {
        RouteChoice::Client => {
            let Some(owner) = route.owner else {
                return;
            };
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_pinch_begin(
                session,
                &GesturePinchBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
            Sequence::Client(owner)
        }
        RouteChoice::Compositor => route
            .output
            .and_then(|output| {
                if session.clusters.active_on(&output).is_some() {
                    return None;
                }
                let camera = session.cameras.get_mut(&output)?;
                Some(Sequence::Compositor(PinchGesture::new(output, camera)))
            })
            .unwrap_or(Sequence::Ignored),
        RouteChoice::Ignored => Sequence::Ignored,
    });
}

fn pinch_update<D, B>(session: &mut Session<D>, event: &B::GesturePinchUpdateEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(mut sequence) = session.gestures.pinch.take() else {
        return;
    };
    let client_owner_is_current = owner_is_current(session, &sequence);
    match &mut sequence {
        Sequence::Client(_) if client_owner_is_current => {
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_pinch_update(
                session,
                &GesturePinchUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                },
            );
        }
        Sequence::Client(_) => sequence = Sequence::Ignored,
        Sequence::Compositor(gesture) => {
            if session.clusters.active_on(&gesture.output).is_some() {
                sequence = Sequence::Ignored;
            } else if let Some(camera) = session.cameras.get_mut(&gesture.output) {
                let delta = event.delta();
                gesture.update(
                    camera,
                    &session.settings.zoom,
                    event.time_msec(),
                    delta.x,
                    delta.y,
                    event.scale(),
                );
                session.request_redraw();
            } else {
                sequence = Sequence::Ignored;
            }
        }
        Sequence::Ignored => {}
    }
    session.gestures.pinch = Some(sequence);
}

fn pinch_end<D, B>(session: &mut Session<D>, event: &B::GesturePinchEndEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(sequence) = session.gestures.pinch.take() else {
        return;
    };
    match sequence {
        Sequence::Client(_) if owner_is_current(session, &sequence) => {
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_pinch_end(
                session,
                &GesturePinchEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }
        Sequence::Compositor(gesture) => {
            if let Some(camera) = session.cameras.get_mut(&gesture.output) {
                gesture.finish(
                    camera,
                    event.cancelled(),
                    session.settings.input.gestures.pan_momentum,
                    session.settings.input.gestures.flick_min_px_per_s,
                );
                session.request_redraw();
            }
        }
        Sequence::Client(_) | Sequence::Ignored => {}
    }
}

fn hold_begin<D, B>(session: &mut Session<D>, event: &B::GestureHoldBeginEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let route = begin_route(session, event.time_msec(), false, GestureScope::EmptyField);
    session.gestures.hold = Some(match route.choice {
        RouteChoice::Client => {
            let Some(owner) = route.owner else {
                return;
            };
            let pointer = session
                .seat
                .get_pointer()
                .expect("pointer capability added at seat setup");
            pointer.gesture_hold_begin(
                session,
                &GestureHoldBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
            Sequence::Client(owner)
        }
        RouteChoice::Compositor | RouteChoice::Ignored => Sequence::Ignored,
    });
}

fn hold_end<D, B>(session: &mut Session<D>, event: &B::GestureHoldEndEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(sequence) = session.gestures.hold.take() else {
        return;
    };
    if !owner_is_current(session, &sequence) {
        return;
    }
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    pointer.gesture_hold_end(
        session,
        &GestureHoldEndEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            cancelled: event.cancelled(),
        },
    );
}

pub(super) fn handle_axis_pan<D, B, E>(session: &mut Session<D>, event: &E) -> bool
where
    D: SessionDriver,
    B: InputBackend,
    E: PointerAxisEvent<B>,
{
    if event.source() != AxisSource::Finger {
        session.gestures.axis_pan = None;
        return false;
    }

    let horizontal = event.amount(Axis::Horizontal);
    let vertical = event.amount(Axis::Vertical);
    if let Some(axis_pan) = session.gestures.axis_pan.as_mut() {
        if let Some(value) = horizontal {
            axis_pan.horizontal_active = value != 0.0;
        }
        if let Some(value) = vertical {
            axis_pan.vertical_active = value != 0.0;
        }
        let output = axis_pan.output.clone();
        let ended = !axis_pan.horizontal_active && !axis_pan.vertical_active;
        if ended {
            session.gestures.axis_pan = None;
            return true;
        }
        if let Some(camera) = session.cameras.get_mut(&output) {
            camera::apply_pan(camera, horizontal.unwrap_or(0.0), vertical.unwrap_or(0.0));
            session.request_redraw();
        } else {
            session.gestures.axis_pan = None;
        }
        return true;
    }

    let horizontal_active = horizontal.is_some_and(|value| value != 0.0);
    let vertical_active = vertical.is_some_and(|value| value != 0.0);
    if !horizontal_active && !vertical_active
        || session.settings.input.gestures.scroll_pan == ScrollPanMode::Off
    {
        return false;
    }

    let route = begin_route(session, event.time_msec(), true, GestureScope::EmptyField);
    let Some(output) = (route.choice == RouteChoice::Compositor)
        .then_some(route.output)
        .flatten()
    else {
        return false;
    };
    let Some(camera) = session.cameras.get_mut(&output) else {
        return false;
    };
    camera.snap_targets_to_live();
    camera.pan_vel = halley_core::field::Vec2 { x: 0.0, y: 0.0 };
    camera::apply_pan(camera, horizontal.unwrap_or(0.0), vertical.unwrap_or(0.0));
    session.gestures.axis_pan = Some(AxisPan {
        output,
        horizontal_active,
        vertical_active,
    });
    session.request_redraw();
    true
}

pub(crate) fn cancel_all<D: SessionDriver>(session: &mut Session<D>) {
    let time = session.start_time.elapsed().as_millis() as u32;
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    if session
        .gestures
        .swipe
        .take()
        .is_some_and(|sequence| owner_is_current(session, &sequence))
    {
        pointer.gesture_swipe_end(
            session,
            &GestureSwipeEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled: true,
            },
        );
    }
    if session
        .gestures
        .pinch
        .take()
        .is_some_and(|sequence| owner_is_current(session, &sequence))
    {
        pointer.gesture_pinch_end(
            session,
            &GesturePinchEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled: true,
            },
        );
    }
    if session
        .gestures
        .hold
        .take()
        .is_some_and(|sequence| owner_is_current(session, &sequence))
    {
        pointer.gesture_hold_end(
            session,
            &GestureHoldEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled: true,
            },
        );
    }
    session.gestures.axis_pan = None;
}

pub(super) fn cancel_surface<D: SessionDriver>(session: &mut Session<D>, surface: &WlSurface) {
    let root = crate::wayland::compositor::root_surface(surface);
    let owns_route = session
        .gestures
        .swipe
        .as_ref()
        .is_some_and(|route| sequence_owned_by(route, &root))
        || session
            .gestures
            .pinch
            .as_ref()
            .is_some_and(|route| sequence_owned_by(route, &root))
        || session
            .gestures
            .hold
            .as_ref()
            .is_some_and(|route| sequence_owned_by(route, &root));
    if owns_route {
        cancel_all(session);
    }
}

fn sequence_owned_by<T>(sequence: &Sequence<T>, root: &WlSurface) -> bool {
    matches!(
        sequence,
        Sequence::Client(owner)
            if crate::wayland::compositor::root_surface(owner) == *root
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RoutePolicy {
        RoutePolicy {
            behavior_enabled: true,
            client_available: true,
            client_passthrough: true,
            scope: GestureScope::EmptyField,
            modifier_forces: false,
            client_forced: false,
            blocked: false,
        }
    }

    #[test]
    fn app_surface_owns_an_unmodified_empty_field_gesture() {
        assert_eq!(policy().choose(), RouteChoice::Client);
    }

    #[test]
    fn field_modifier_and_global_scope_route_to_the_compositor() {
        assert_eq!(
            RoutePolicy {
                client_available: false,
                ..policy()
            }
            .choose(),
            RouteChoice::Compositor
        );
        assert_eq!(
            RoutePolicy {
                modifier_forces: true,
                ..policy()
            }
            .choose(),
            RouteChoice::Compositor
        );
        assert_eq!(
            RoutePolicy {
                scope: GestureScope::Global,
                ..policy()
            }
            .choose(),
            RouteChoice::Compositor
        );
    }

    #[test]
    fn client_constraint_or_grab_always_has_first_refusal() {
        assert_eq!(
            RoutePolicy {
                client_forced: true,
                modifier_forces: true,
                scope: GestureScope::Global,
                ..policy()
            }
            .choose(),
            RouteChoice::Client
        );
    }

    #[test]
    fn compositor_modal_grabs_ignore_new_gestures() {
        assert_eq!(
            RoutePolicy {
                blocked: true,
                ..policy()
            }
            .choose(),
            RouteChoice::Ignored
        );
    }

    #[test]
    fn disabled_behavior_falls_through_only_to_available_clients() {
        assert_eq!(
            RoutePolicy {
                behavior_enabled: false,
                ..policy()
            }
            .choose(),
            RouteChoice::Client
        );
        assert_eq!(
            RoutePolicy {
                behavior_enabled: false,
                client_available: false,
                ..policy()
            }
            .choose(),
            RouteChoice::Ignored
        );
    }

    #[test]
    fn disabling_client_passthrough_does_not_steal_an_app_gesture() {
        assert_eq!(
            RoutePolicy {
                client_passthrough: false,
                ..policy()
            }
            .choose(),
            RouteChoice::Ignored
        );
    }
}
