mod constraints;

pub(super) use constraints::PointerConstraintLifecycle;

use smithay::input::pointer::{MotionEvent, PointerHandle};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use super::{Session, SessionDriver};

#[derive(Debug)]
pub(super) struct ConstraintDiagnostic {
    pub(super) protocol_resource: Option<String>,
    pub(super) protocol_kind: Option<&'static str>,
    pub(super) protocol_active: bool,
    pub(super) halley_tracked: bool,
    pub(super) tracked_kind: Option<&'static str>,
    pub(super) tracked_surface_id: Option<u32>,
}

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
    let mut route = crate::input::pointer::route_to_client(
        crate::input::pointer::PointerRoutingContext {
            space: &session.wayland.space,
            cameras: &session.cameras,
            clusters: &session.clusters,
            nodes: &session.nodes,
            window_open_animations: &session.window_open_animations,
            primary: session.driver.primary_output(),
            fullscreen: &session.fullscreen,
            maximize: &session.maximize,
            decorations: &session.settings.decorations,
            font: &session.settings.font,
            focused: session.wayland.focused_window.as_ref(),
            now: crate::frame_clock::monotonic_now(),
        },
        session.pointer.position(),
    )?;
    let output_geometry = session.wayland.space.output_geometry(&route.output)?;
    let camera = session.cameras.get(&route.output.name())?;
    let cluster_exclusive = crate::presentation::window::cluster_exclusive_presentation(
        &session.clusters,
        &session.nodes,
        &session.fullscreen,
        &session.maximize,
        &route.output,
        output_geometry,
        crate::frame_clock::monotonic_now(),
    )
    .is_some_and(|presentation| presentation.progress > 0.0);
    if !cluster_exclusive
        && session
            .nodes
            .hit_test(
                &route.output,
                output_geometry,
                camera,
                Point::from(session.pointer.position()),
            )
            .is_some()
    {
        let world = crate::input::grab::screen_to_world_on_output(
            session.pointer.position(),
            camera,
            output_geometry,
        );
        route.location = Point::from((world.x as f64, world.y as f64));
        route.focus = None;
        route.target = crate::input::pointer::PointerTarget::Background;
        route.visual_geometry = None;
    }
    Some(route)
}

pub(super) fn has_active_constraint<D: SessionDriver>(session: &Session<D>) -> bool {
    session
        .seat
        .get_pointer()
        .is_some_and(|pointer| constraints::active(session, &pointer).is_some())
}

pub(super) fn has_active_confinement<D: SessionDriver>(session: &Session<D>) -> bool {
    session
        .seat
        .get_pointer()
        .is_some_and(|pointer| constraints::has_active_confinement(session, &pointer))
}

pub(super) fn constraint_diagnostic<D: SessionDriver>(
    session: &Session<D>,
    surface: &WlSurface,
) -> ConstraintDiagnostic {
    let Some(pointer) = session.seat.get_pointer() else {
        return ConstraintDiagnostic {
            protocol_resource: None,
            protocol_kind: None,
            protocol_active: false,
            halley_tracked: false,
            tracked_kind: None,
            tracked_surface_id: None,
        };
    };
    constraints::diagnostic(session, &pointer, surface)
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
            reset_client_cursor_image(session);
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

fn reset_client_cursor_image<D: SessionDriver>(session: &mut Session<D>) {
    if let Some(previous) = session.cursor.reset_image() {
        crate::cursor::surface::clear_outputs(&previous, &session.wayland.space);
    }
    crate::cursor::surface::refresh_outputs(
        &session.cursor,
        &session.wayland.space,
        session.pointer.position(),
    );
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

/// Re-evaluates presentation-driven pointer focus without inventing physical
/// mouse motion. A moving cluster card may enter or leave the stationary
/// pointer, but changing its visual transform must not stream new absolute
/// coordinates to the same client every frame.
pub(super) fn refresh_client_focus<D: SessionDriver>(session: &mut Session<D>, time: u32) {
    let pointer = session
        .seat
        .get_pointer()
        .expect("pointer capability added at seat setup");
    route_for_discrete_input(session, time);
    finish_frame(session, &pointer);
}

pub(crate) fn release_for_compositor_warp<D: SessionDriver>(session: &mut Session<D>) {
    constraints::deactivate_before_pointer_focus_change(session, None);
}

pub(crate) fn recover_after_session_resume<D: SessionDriver>(session: &mut Session<D>) {
    constraints::deactivate_before_pointer_focus_change(session, None);
    reset_client_cursor_image(session);
    session.cursor.set_override(None);
    session.cursor_policy.pointer_activity();
    session.request_redraw();
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
    if interactive_overlay_forces_cursor(
        session.shell.apogee.is_active(),
        session.capture.is_active(),
        session.clusters.accepts_modal_input(),
    ) {
        return true;
    }
    cursor_presentation_visible(
        session.cursor_policy.visible(),
        constraints::cursor_visible(session),
    )
}

pub(super) fn cursor_override<D: SessionDriver>(
    session: &Session<D>,
) -> Option<smithay::input::pointer::CursorIcon> {
    interactive_overlay_cursor_override(
        session.capture.is_active(),
        session.clusters.accepts_modal_input(),
    )
}

fn interactive_overlay_cursor_override(
    capture_active: bool,
    cluster_creation_active: bool,
) -> Option<smithay::input::pointer::CursorIcon> {
    (capture_active || cluster_creation_active)
        .then_some(smithay::input::pointer::CursorIcon::Default)
}

fn interactive_overlay_forces_cursor(
    apogee_active: bool,
    capture_active: bool,
    cluster_creation_active: bool,
) -> bool {
    apogee_active || capture_active || cluster_creation_active
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
        cursor_presentation_visible, interactive_overlay_cursor_override,
        interactive_overlay_forces_cursor, should_emit_absolute_motion,
    };

    #[test]
    fn interactive_overlays_force_the_cursor_visible() {
        assert!(interactive_overlay_forces_cursor(true, false, false));
        assert!(interactive_overlay_forces_cursor(false, true, false));
        assert!(interactive_overlay_forces_cursor(false, false, true));
        assert!(!interactive_overlay_forces_cursor(false, false, false));
    }

    #[test]
    fn interactive_text_overlay_replaces_a_hidden_client_cursor_image() {
        assert_eq!(
            interactive_overlay_cursor_override(false, true),
            Some(smithay::input::pointer::CursorIcon::Default)
        );
        assert_eq!(interactive_overlay_cursor_override(false, false), None);
    }

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
