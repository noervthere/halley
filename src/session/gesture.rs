use smithay::backend::input::{
    Event, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
    GestureSwipeUpdateEvent as _, InputBackend, InputEvent,
};
use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;

use super::{Session, SessionDriver};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Route {
    Client(WlSurface),
    Ignored,
}

#[derive(Default)]
pub(super) struct GestureState {
    swipe: Option<Route>,
    pinch: Option<Route>,
    hold: Option<Route>,
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

fn client_route<D: SessionDriver>(session: &mut Session<D>, time: u32) -> Route {
    if !session.input.gestures.enabled || !session.input.gestures.client_passthrough {
        return Route::Ignored;
    }
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    super::pointer::route_for_discrete_input(session, time);
    super::pointer::finish_frame(session, &pointer);
    pointer
        .current_focus()
        .map(Route::Client)
        .unwrap_or(Route::Ignored)
}

fn owner_is_current<D: SessionDriver>(session: &Session<D>, route: &Route) -> bool {
    let Route::Client(owner) = route else {
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
    let route = client_route(session, event.time_msec());
    if matches!(route, Route::Client(_)) {
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
    }
    session.gestures.swipe = Some(route);
}

fn swipe_update<D, B>(session: &mut Session<D>, event: &B::GestureSwipeUpdateEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(route) = session.gestures.swipe.as_ref() else {
        return;
    };
    if !owner_is_current(session, route) {
        session.gestures.swipe = Some(Route::Ignored);
        return;
    }
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

fn swipe_end<D, B>(session: &mut Session<D>, event: &B::GestureSwipeEndEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(route) = session.gestures.swipe.take() else {
        return;
    };
    if !owner_is_current(session, &route) {
        return;
    }
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

fn pinch_begin<D, B>(session: &mut Session<D>, event: &B::GesturePinchBeginEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let route = client_route(session, event.time_msec());
    if matches!(route, Route::Client(_)) {
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
    }
    session.gestures.pinch = Some(route);
}

fn pinch_update<D, B>(session: &mut Session<D>, event: &B::GesturePinchUpdateEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(route) = session.gestures.pinch.as_ref() else {
        return;
    };
    if !owner_is_current(session, route) {
        session.gestures.pinch = Some(Route::Ignored);
        return;
    }
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

fn pinch_end<D, B>(session: &mut Session<D>, event: &B::GesturePinchEndEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(route) = session.gestures.pinch.take() else {
        return;
    };
    if !owner_is_current(session, &route) {
        return;
    }
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

fn hold_begin<D, B>(session: &mut Session<D>, event: &B::GestureHoldBeginEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let route = client_route(session, event.time_msec());
    if matches!(route, Route::Client(_)) {
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
    }
    session.gestures.hold = Some(route);
}

fn hold_end<D, B>(session: &mut Session<D>, event: &B::GestureHoldEndEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(route) = session.gestures.hold.take() else {
        return;
    };
    if !owner_is_current(session, &route) {
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

pub(super) fn cancel_all<D: SessionDriver>(session: &mut Session<D>) {
    let time = session.start_time.elapsed().as_millis() as u32;
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    if session
        .gestures
        .swipe
        .take()
        .is_some_and(|route| owner_is_current(session, &route))
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
        .is_some_and(|route| owner_is_current(session, &route))
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
        .is_some_and(|route| owner_is_current(session, &route))
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
}

pub(super) fn cancel_surface<D: SessionDriver>(session: &mut Session<D>, surface: &WlSurface) {
    let root = crate::wayland::compositor::root_surface(surface);
    let owns_route = [
        session.gestures.swipe.as_ref(),
        session.gestures.pinch.as_ref(),
        session.gestures.hold.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|route| {
        matches!(
            route,
            Route::Client(owner)
                if crate::wayland::compositor::root_surface(owner) == root
        )
    });
    if owns_route {
        cancel_all(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_routes_never_claim_client_ownership() {
        assert!(!matches!(Route::Ignored, Route::Client(_)));
    }
}
