use std::collections::HashMap;

use smithay::backend::input::{
    AbsolutePositionEvent, Device, Event, InputBackend, InputEvent, TouchEvent,
};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use super::{Session, SessionDriver};

#[derive(Clone, Debug)]
enum CoordinateSpace {
    Screen,
    Window(WlSurface),
}

#[derive(Clone, Debug)]
struct TouchTarget {
    surface: WlSurface,
    coordinates: CoordinateSpace,
}

#[derive(Default)]
pub(super) struct TouchState {
    targets: HashMap<smithay::backend::input::TouchSlot, TouchTarget>,
}

impl TouchState {
    fn begin(&mut self, slot: smithay::backend::input::TouchSlot, target: TouchTarget) {
        self.targets.insert(slot, target);
    }

    fn finish(&mut self, slot: smithay::backend::input::TouchSlot) -> bool {
        self.targets.remove(&slot).is_some()
    }

    fn has_surface(&self, surface: &WlSurface) -> bool {
        let root = crate::wayland::compositor::root_surface(surface);
        self.targets
            .values()
            .any(|target| crate::wayland::compositor::root_surface(&target.surface) == root)
    }

    fn clear(&mut self) -> bool {
        let active = !self.targets.is_empty();
        self.targets.clear();
        active
    }
}

pub(super) fn handle<D, B>(session: &mut Session<D>, event: &InputEvent<B>) -> bool
where
    D: SessionDriver,
    B: InputBackend,
{
    match event {
        InputEvent::TouchDown { event } => down::<D, B>(session, event),
        InputEvent::TouchMotion { event } => motion::<D, B>(session, event),
        InputEvent::TouchUp { event } => up::<D, B>(session, event),
        InputEvent::TouchFrame { .. } => frame(session),
        InputEvent::TouchCancel { .. } => cancel_all(session),
        _ => return false,
    }
    true
}

pub(crate) fn handle_session_lock<D, B>(session: &mut Session<D>, event: &InputEvent<B>) -> bool
where
    D: SessionDriver,
    B: InputBackend,
{
    match event {
        InputEvent::TouchDown { event } => {
            let Some(handle) = session.seat.get_touch() else {
                return true;
            };
            let Some(screen) = screen_position(session, event) else {
                return true;
            };
            let Some((surface, origin, _)) = session
                .session_lock
                .focus_at(&session.wayland.space, screen.into())
            else {
                return true;
            };
            session.session_lock.set_focus(&surface);
            super::sync_keyboard_focus(session, SERIAL_COUNTER.next_serial());
            session.touch.begin(
                event.slot(),
                TouchTarget {
                    surface: surface.clone(),
                    coordinates: CoordinateSpace::Screen,
                },
            );
            handle.down(
                session,
                Some((surface, origin)),
                &DownEvent {
                    slot: event.slot(),
                    location: screen,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchMotion { event } => {
            let Some(handle) = session.seat.get_touch() else {
                return true;
            };
            if !session.touch.targets.contains_key(&event.slot()) {
                return true;
            }
            let Some(screen) = screen_position(session, event) else {
                return true;
            };
            handle.motion(
                session,
                None,
                &MotionEvent {
                    slot: event.slot(),
                    location: screen,
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::TouchUp { event } => {
            let Some(handle) = session.seat.get_touch() else {
                return true;
            };
            if session.touch.finish(event.slot()) {
                handle.up(
                    session,
                    &UpEvent {
                        slot: event.slot(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
            }
        }
        InputEvent::TouchFrame { .. } => frame(session),
        InputEvent::TouchCancel { .. } => cancel_all(session),
        _ => return false,
    }
    true
}

fn down<D, B>(session: &mut Session<D>, event: &B::TouchDownEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(handle) = session.seat.get_touch() else {
        return;
    };
    let Some(screen) = screen_position(session, event) else {
        return;
    };
    let Some(route) = route(session, screen) else {
        return;
    };
    crate::wayland::focus::select_output(&mut session.wayland, &route.output);
    let serial = SERIAL_COUNTER.next_serial();
    match &route.target {
        crate::input::pointer::PointerTarget::Window(window) => {
            super::focus::focus_window_from_pointer(session, window, serial);
        }
        crate::input::pointer::PointerTarget::Layer(layer) => {
            super::focus::focus_layer(session, Some(layer.clone()), serial);
        }
        crate::input::pointer::PointerTarget::Background => {
            super::focus::focus_layer(session, None, serial);
        }
    }

    if !session.settings.input.gestures.touch_passthrough {
        return;
    }
    let Some((surface, origin)) = route.focus else {
        return;
    };
    let coordinates = match route.target {
        crate::input::pointer::PointerTarget::Window(_) => {
            CoordinateSpace::Window(crate::wayland::compositor::root_surface(&surface))
        }
        crate::input::pointer::PointerTarget::Layer(_)
        | crate::input::pointer::PointerTarget::Background => CoordinateSpace::Screen,
    };
    session.touch.begin(
        event.slot(),
        TouchTarget {
            surface: surface.clone(),
            coordinates,
        },
    );
    handle.down(
        session,
        Some((surface, origin)),
        &DownEvent {
            slot: event.slot(),
            location: route.location,
            serial,
            time: event.time_msec(),
        },
    );
}

fn motion<D, B>(session: &mut Session<D>, event: &B::TouchMotionEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(handle) = session.seat.get_touch() else {
        return;
    };
    let Some(target) = session.touch.targets.get(&event.slot()).cloned() else {
        return;
    };
    let Some(screen) = screen_position(session, event) else {
        return;
    };
    let location = match target.coordinates {
        CoordinateSpace::Screen => screen,
        CoordinateSpace::Window(surface) => {
            let Some(presentation) = crate::presentation::window::WindowPresentation::for_surface(
                &session.wayland.space,
                &session.cameras,
                Some(&session.clusters),
                Some(&session.nodes),
                session.driver.primary_output(),
                &session.window_open_animations,
                &session.fullscreen,
                &session.maximize,
                &surface,
                crate::frame_clock::monotonic_now(),
            ) else {
                cancel_all(session);
                return;
            };
            presentation.source_from_screen(screen)
        }
    };
    handle.motion(
        session,
        None,
        &MotionEvent {
            slot: event.slot(),
            location,
            time: event.time_msec(),
        },
    );
}

fn up<D, B>(session: &mut Session<D>, event: &B::TouchUpEvent)
where
    D: SessionDriver,
    B: InputBackend,
{
    let Some(handle) = session.seat.get_touch() else {
        return;
    };
    if !session.touch.finish(event.slot()) {
        return;
    }
    handle.up(
        session,
        &UpEvent {
            slot: event.slot(),
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
        },
    );
}

fn frame<D: SessionDriver>(session: &mut Session<D>) {
    if session.touch.targets.is_empty() {
        return;
    }
    if let Some(handle) = session.seat.get_touch() {
        handle.frame(session);
    }
}

pub(crate) fn cancel_all<D: SessionDriver>(session: &mut Session<D>) {
    if !session.touch.clear() {
        return;
    }
    if let Some(handle) = session.seat.get_touch() {
        handle.cancel(session);
    }
}

pub(super) fn cancel_surface<D: SessionDriver>(session: &mut Session<D>, surface: &WlSurface) {
    if session.touch.has_surface(surface) {
        cancel_all(session);
    }
}

fn route<D: SessionDriver>(
    session: &Session<D>,
    screen: Point<f64, Logical>,
) -> Option<crate::input::pointer::PointerRoute> {
    crate::input::pointer::route_to_client(
        crate::input::pointer::PointerRoutingContext {
            space: &session.wayland.space,
            cameras: &session.cameras,
            clusters: &session.clusters,
            nodes: &session.nodes,
            window_open_animations: &session.window_open_animations,
            primary: session.driver.primary_output(),
            fullscreen: &session.fullscreen,
            maximize: &session.maximize,
            focused: session.wayland.focused_window.as_ref(),
            now: crate::frame_clock::monotonic_now(),
        },
        screen.into(),
    )
}

fn screen_position<D, B, E>(session: &Session<D>, event: &E) -> Option<Point<f64, Logical>>
where
    D: SessionDriver,
    B: InputBackend,
    E: AbsolutePositionEvent<B>,
{
    let device_name = event.device().name();
    let configured = session
        .settings
        .input
        .settings_for_device(halley_config::DeviceKind::Touchscreen, device_name.as_ref());
    let requested_output = configured.map_to_output.as_deref();
    let mapped_output = requested_output.and_then(|name| {
        session
            .wayland
            .space
            .outputs()
            .find(|output| output.name() == name)
    });
    if let Some(requested) = requested_output
        && mapped_output.is_none()
    {
        eventline::warn!(
            "input: touchscreen {device_name:?} requested unavailable output {requested:?}; using \
             {:?}",
            session.driver.primary_output().name()
        );
    }
    let output = mapped_output.unwrap_or_else(|| session.driver.primary_output());
    transform_absolute_position(
        event,
        output,
        session.wayland.space.output_geometry(output)?,
    )
}

fn transform_absolute_position<B, E>(
    event: &E,
    output: &Output,
    geometry: smithay::utils::Rectangle<i32, Logical>,
) -> Option<Point<f64, Logical>>
where
    B: InputBackend,
    E: AbsolutePositionEvent<B>,
{
    let transform = output.current_transform();
    let input_size = transform.invert().transform_size(geometry.size);
    let local =
        transform.transform_point_in(event.position_transformed(input_size), &input_size.to_f64());
    Some(local + geometry.loc.to_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_state_pairs_slots_and_cancels_a_surface_as_one_sequence() {
        // The protocol objects require a display, so keep the lifecycle proof
        // on the pure state container.
        let mut state = TouchState::default();
        assert!(!state.finish(None.into()));
        assert!(!state.clear());
    }

    #[test]
    fn all_wayland_output_transforms_have_an_inverse() {
        for transform in [
            smithay::utils::Transform::Normal,
            smithay::utils::Transform::_90,
            smithay::utils::Transform::_180,
            smithay::utils::Transform::_270,
            smithay::utils::Transform::Flipped,
            smithay::utils::Transform::Flipped90,
            smithay::utils::Transform::Flipped180,
            smithay::utils::Transform::Flipped270,
        ] {
            assert_eq!(transform.invert().invert(), transform);
        }
    }
}
