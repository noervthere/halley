mod constraints;

use smithay::input::pointer::{MotionEvent, PointerHandle};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use super::{Session, SessionDriver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbsoluteMotionPolicy {
    PositionChanged,
    SurfaceChanged,
}

fn should_emit_absolute_motion(
    policy: AbsoluteMotionPolicy,
    surface_changed: bool,
    locked: bool,
) -> bool {
    // An active locked pointer must receive relative motion only. Sending
    // `wl_pointer.motion` while locked lets XWayland replace its emulated
    // cursor anchor and breaks relative-look clients.
    !locked
        && match policy {
            AbsoluteMotionPolicy::PositionChanged => true,
            AbsoluteMotionPolicy::SurfaceChanged => surface_changed,
        }
}

pub(super) fn route_client<D: SessionDriver>(
    session: &Session<D>,
) -> Option<crate::input::pointer::PointerRoute> {
    crate::input::pointer::route_to_client(
        &session.wayland.space,
        &session.cameras,
        session.driver.primary_output(),
        &session.fullscreen,
        session.wayland.focused_window.as_ref(),
        crate::frame_clock::monotonic_now(),
        session.pointer.position(),
    )
}

fn route_and_update_client_focus<D: SessionDriver>(
    session: &mut Session<D>,
    time: u32,
    policy: AbsoluteMotionPolicy,
) -> Option<crate::input::pointer::PointerRoute> {
    let route = route_client(session)?;
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    let routed_surface = route.focus.as_ref().map(|(surface, _)| surface);
    let surface_changed = pointer.current_focus().as_ref() != routed_surface;
    if should_emit_absolute_motion(
        policy,
        surface_changed,
        constraints::has_active_lock(&pointer),
    ) {
        pointer.motion(
            session,
            route.focus.clone(),
            &MotionEvent {
                location: route.location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
    }
    Some(route)
}

pub(super) fn route_for_motion<D: SessionDriver>(
    session: &mut Session<D>,
    time: u32,
) -> Option<crate::input::pointer::PointerRoute> {
    route_and_update_client_focus(session, time, AbsoluteMotionPolicy::PositionChanged)
}

pub(super) fn route_for_discrete_input<D: SessionDriver>(
    session: &mut Session<D>,
    time: u32,
) -> Option<crate::input::pointer::PointerRoute> {
    route_and_update_client_focus(session, time, AbsoluteMotionPolicy::SurfaceChanged)
}

pub(super) fn finish_frame<D: SessionDriver>(
    session: &mut Session<D>,
    pointer: &PointerHandle<Session<D>>,
) {
    pointer.frame(session);
    // Activate only after the current event batch is complete so the first
    // event cannot straddle the unlocked and locked protocol states.
    constraints::activate_focused(session, pointer);
}

pub(super) fn update_client_state<D: SessionDriver>(session: &mut Session<D>, time: u32) {
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    route_for_motion(session, time);
    finish_frame(session, &pointer);
}

pub(super) fn retire_surface<D: SessionDriver>(
    session: &mut Session<D>,
    removed_root: &WlSurface,
    time: u32,
) {
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    let Some(focused) = pointer.current_focus() else {
        return;
    };
    if crate::wayland::compositor::root_surface(&focused) != *removed_root {
        return;
    }

    let route = route_client(session);
    let location = route
        .as_ref()
        .map(|route| route.location)
        .unwrap_or_else(|| Point::from(session.pointer.position()));
    let focus = route.and_then(|route| route.focus);

    // Surface retirement is the one absolute-focus update that must bypass
    // active-lock suppression. Delivering this leave deactivates the old
    // pointer constraint; without it, an unmapped game can leave relative
    // input anchored to a surface that is no longer in the scene.
    pointer.motion(
        session,
        focus,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    finish_frame(session, &pointer);
}

pub(super) struct ConstraintSnapshot(Option<constraints::ActiveConstraint>);

pub(super) enum ConstrainedMotion {
    Apply,
    Hold,
    RelativeOnly {
        surface: WlSurface,
        origin: Point<f64, Logical>,
    },
}

pub(super) fn constraint_snapshot<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
) -> ConstraintSnapshot {
    ConstraintSnapshot(constraints::active(session, pointer))
}

pub(super) fn constrain_motion<D: SessionDriver>(
    session: &Session<D>,
    snapshot: &ConstraintSnapshot,
) -> ConstrainedMotion {
    let position_allowed = snapshot.0.as_ref().is_none_or(|constraint| {
        constraint.kind != constraints::ConstraintKind::Confined
            || constraints::allows_current_position(session, constraint)
    });
    match constraints::motion_disposition(
        snapshot.0.as_ref().map(|constraint| constraint.kind),
        position_allowed,
    ) {
        constraints::MotionDisposition::Apply => ConstrainedMotion::Apply,
        constraints::MotionDisposition::Hold => ConstrainedMotion::Hold,
        constraints::MotionDisposition::RelativeOnly => {
            let constraint = snapshot
                .0
                .as_ref()
                .expect("relative-only motion requires an active lock");
            ConstrainedMotion::RelativeOnly {
                surface: constraint.surface.clone(),
                origin: constraint.origin,
            }
        }
    }
}

pub(super) fn cursor_visible<D: SessionDriver>(session: &Session<D>) -> bool {
    constraints::cursor_visible(session)
}

pub(super) fn activate_new<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
) {
    constraints::activate_new(session, surface, pointer);
}

pub(super) fn apply_position_hint<D: SessionDriver>(
    session: &Session<D>,
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
    location: Point<f64, Logical>,
) {
    constraints::apply_position_hint(session, surface, pointer, location);
}

#[cfg(test)]
mod tests {
    use super::{AbsoluteMotionPolicy, should_emit_absolute_motion};

    #[test]
    fn locked_pointer_suppresses_every_absolute_motion_policy() {
        assert!(!should_emit_absolute_motion(
            AbsoluteMotionPolicy::PositionChanged,
            true,
            true
        ));
        assert!(!should_emit_absolute_motion(
            AbsoluteMotionPolicy::SurfaceChanged,
            true,
            true
        ));
    }

    #[test]
    fn discrete_input_only_refreshes_a_changed_unlocked_surface() {
        assert!(!should_emit_absolute_motion(
            AbsoluteMotionPolicy::SurfaceChanged,
            false,
            false
        ));
        assert!(should_emit_absolute_motion(
            AbsoluteMotionPolicy::SurfaceChanged,
            true,
            false
        ));
    }

    #[test]
    fn physical_motion_updates_an_unlocked_pointer_position() {
        assert!(should_emit_absolute_motion(
            AbsoluteMotionPolicy::PositionChanged,
            false,
            false
        ));
    }
}
