mod constraints;

pub(super) use constraints::PointerConstraintLifecycle;

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

pub(super) fn has_active_confinement<D: SessionDriver>(session: &Session<D>) -> bool {
    session
        .seat
        .get_pointer()
        .is_some_and(|pointer| constraints::has_active_confinement(session, &pointer))
}

pub(super) fn has_active_constraint<D: SessionDriver>(session: &Session<D>) -> bool {
    session
        .seat
        .get_pointer()
        .is_some_and(|pointer| constraints::active(session, &pointer).is_some())
}

fn constraint_requires_stable_presentation(kind: Option<constraints::ConstraintKind>) -> bool {
    kind != Some(constraints::ConstraintKind::Locked)
}

pub(super) fn new_constraint_requires_stable_presentation<D: SessionDriver>(
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
) -> bool {
    constraint_requires_stable_presentation(constraints::constraint_kind(surface, pointer))
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
        constraints::has_active_lock(session, &pointer),
    ) {
        if surface_changed {
            constraints::deactivate_before_pointer_focus_change(session, routed_surface);
        }
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
    // Reconcile only after the current event batch is complete so an input
    // event cannot straddle the old and new constraint owners.
    constraints::reconcile(session, pointer, None);
    pointer.frame(session);
}

pub(super) fn update_client_state<D: SessionDriver>(session: &mut Session<D>, time: u32) {
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    route_for_motion(session, time);
    finish_frame(session, &pointer);
}

pub(super) fn reconcile_state<D: SessionDriver>(session: &mut Session<D>) {
    let Some(pointer) = session.seat.get_pointer() else {
        return;
    };
    constraints::reconcile(session, &pointer, None);
    pointer.frame(session);
}

pub(super) enum ConstrainedMotion {
    Apply,
    Hold,
    RelativeOnly {
        surface: WlSurface,
        origin: Point<f64, Logical>,
    },
}

pub(super) fn constrain_motion<D: SessionDriver>(
    session: &mut Session<D>,
    pointer: &PointerHandle<Session<D>>,
) -> ConstrainedMotion {
    constraints::reconcile(session, pointer, None);
    let active = constraints::active(session, pointer);
    let position_allowed = active.as_ref().is_none_or(|constraint| {
        constraint.kind != constraints::ConstraintKind::Confined
            || constraints::allows_current_position(session, constraint)
    });
    match constraints::motion_disposition(
        active.as_ref().map(|constraint| constraint.kind),
        position_allowed,
    ) {
        constraints::MotionDisposition::Apply => ConstrainedMotion::Apply,
        constraints::MotionDisposition::Hold => ConstrainedMotion::Hold,
        constraints::MotionDisposition::RelativeOnly => {
            let constraint = active
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
    cursor_presentation_visible(
        session.cursor_policy.visible(),
        constraints::cursor_visible(session),
    )
}

fn cursor_presentation_visible(policy_visible: bool, constraint_visible: bool) -> bool {
    policy_visible && constraint_visible
}

pub(super) fn prepare_keyboard_focus_change<D: SessionDriver>(
    session: &mut Session<D>,
    next_focused_root: Option<&WlSurface>,
) {
    constraints::deactivate_before_focus_change(session, next_focused_root);
}

pub(super) fn prepare_unmap<D: SessionDriver>(session: &mut Session<D>, root: &WlSurface) {
    constraints::deactivate_before_unmap(session, root);
}

pub(super) fn activate_new<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
) {
    constraints::reconcile(session, pointer, Some(surface));
    pointer.frame(session);
}

pub(super) fn apply_position_hint<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
    location: Point<f64, Logical>,
) {
    constraints::apply_position_hint(session, surface, pointer, location);
}

#[cfg(test)]
mod tests {
    use super::{
        AbsoluteMotionPolicy, constraint_requires_stable_presentation, constraints,
        cursor_presentation_visible, should_emit_absolute_motion,
    };

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

    #[test]
    fn locks_allow_visual_motion_while_confinement_requires_stable_geometry() {
        assert!(!constraint_requires_stable_presentation(Some(
            constraints::ConstraintKind::Locked
        )));
        assert!(constraint_requires_stable_presentation(Some(
            constraints::ConstraintKind::Confined
        )));
        assert!(constraint_requires_stable_presentation(None));
    }

    #[test]
    fn policy_hiding_and_pointer_lock_independently_mask_presentation() {
        assert!(cursor_presentation_visible(true, true));
        assert!(!cursor_presentation_visible(false, true));
        assert!(!cursor_presentation_visible(true, false));
        assert!(!cursor_presentation_visible(false, false));
    }
}
