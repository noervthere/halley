use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::input::pointer::{MotionEvent, PointerHandle};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Size};
use smithay::wayland::compositor::RegionAttributes;
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};
use smithay::wayland::seat::WaylandFocus;

use crate::input::presentation::WindowPresentation;
use crate::session::{Session, SessionDriver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConstraintKind {
    Confined,
    Locked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MotionDisposition {
    Apply,
    Hold,
    RelativeOnly,
}

pub(super) fn motion_disposition(
    kind: Option<ConstraintKind>,
    confined_position_allowed: bool,
) -> MotionDisposition {
    match kind {
        Some(ConstraintKind::Locked) => MotionDisposition::RelativeOnly,
        Some(ConstraintKind::Confined) if !confined_position_allowed => MotionDisposition::Hold,
        Some(ConstraintKind::Confined) | None => MotionDisposition::Apply,
    }
}

#[derive(Clone)]
struct TrackedConstraint {
    surface: WlSurface,
    kind: ConstraintKind,
    position_hint: Option<Point<f64, Logical>>,
}

/// Halley's single record of the protocol constraint currently allowed to
/// affect input. Protocol resources remain owned by Smithay; every transition
/// into or out of their active state is serialized through `reconcile`.
#[derive(Default)]
pub struct PointerConstraintLifecycle {
    active: Option<TrackedConstraint>,
}

#[derive(Clone)]
struct ConstraintDescriptor {
    kind: ConstraintKind,
    active: bool,
    region: Option<RegionAttributes>,
    position_hint: Option<Point<f64, Logical>>,
}

struct OwnerContext {
    presentation: WindowPresentation,
    surface_size: Size<i32, Logical>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReconcileDecision {
    deactivate_current: bool,
    establish_focus: bool,
    activate_candidate: bool,
}

fn reconcile_decision(
    current_present: bool,
    current_is_candidate: bool,
    current_valid: bool,
    candidate_present: bool,
    candidate_pointer_focused: bool,
    candidate_protocol_active: bool,
) -> ReconcileDecision {
    ReconcileDecision {
        deactivate_current: current_present && (!current_is_candidate || !current_valid),
        establish_focus: candidate_present
            && (!candidate_pointer_focused || !candidate_protocol_active),
        activate_candidate: candidate_present && !candidate_protocol_active,
    }
}

fn descriptor<D: SessionDriver>(
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
) -> Option<ConstraintDescriptor> {
    with_pointer_constraint(surface, pointer, |constraint| {
        let constraint = constraint?;
        let (kind, position_hint) = match &*constraint {
            PointerConstraint::Confined(_) => (ConstraintKind::Confined, None),
            PointerConstraint::Locked(locked) => {
                (ConstraintKind::Locked, locked.cursor_position_hint())
            }
        };
        Some(ConstraintDescriptor {
            kind,
            active: constraint.is_active(),
            region: constraint.region().cloned(),
            position_hint,
        })
    })
}

fn surface_size(surface: &WlSurface) -> Option<Size<i32, Logical>> {
    with_renderer_surface_state(surface, |state| state.surface_size()).flatten()
}

fn owner_context<D: SessionDriver>(
    session: &Session<D>,
    surface: &WlSurface,
) -> Option<OwnerContext> {
    if !surface.alive() {
        return None;
    }
    let presentation = WindowPresentation::for_surface(
        &session.wayland.space,
        &session.cameras,
        session.driver.primary_output(),
        &session.fullscreen,
        surface,
        crate::frame_clock::monotonic_now(),
    )?;
    if session.wayland.focused_window.as_ref() != Some(presentation.root()) {
        return None;
    }
    let keyboard_root = session
        .seat
        .get_keyboard()?
        .current_focus()?
        .wl_surface()
        .map(|surface| crate::wayland::compositor::root_surface(surface.as_ref()))?;
    if keyboard_root != *presentation.root() {
        return None;
    }
    Some(OwnerContext {
        presentation,
        surface_size: surface_size(surface)?,
    })
}

fn valid_active_owner<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
    tracked: &TrackedConstraint,
) -> bool {
    pointer.current_focus().as_ref() == Some(&tracked.surface)
        && owner_context(session, &tracked.surface).is_some()
        && descriptor(&tracked.surface, pointer)
            .is_some_and(|constraint| constraint.active && constraint.kind == tracked.kind)
}

fn protocol_active_focus<D: SessionDriver>(
    pointer: &PointerHandle<Session<D>>,
) -> Option<TrackedConstraint> {
    let surface = pointer.current_focus()?;
    let constraint = descriptor(&surface, pointer)?;
    constraint.active.then_some(TrackedConstraint {
        surface,
        kind: constraint.kind,
        position_hint: constraint.position_hint,
    })
}

fn deactivate_protocol<D: SessionDriver>(surface: &WlSurface, pointer: &PointerHandle<Session<D>>) {
    with_pointer_constraint(surface, pointer, |constraint| {
        if let Some(constraint) = constraint {
            constraint.deactivate();
        }
    });
}

fn deactivate_tracked<D: SessionDriver>(
    session: &mut Session<D>,
    pointer: &PointerHandle<Session<D>>,
    tracked: &TrackedConstraint,
) {
    let state = descriptor(&tracked.surface, pointer);
    let position_hint = tracked
        .position_hint
        .or_else(|| state.as_ref().and_then(|state| state.position_hint));
    if let (Some(position_hint), Some(context)) =
        (position_hint, owner_context(session, &tracked.surface))
        && let Some(local) = nearest_valid_point(
            position_hint,
            context.surface_size,
            state.as_ref().and_then(|state| state.region.as_ref()),
        )
        && let Some(screen) = context
            .presentation
            .screen_from_surface(&tracked.surface, local)
    {
        session.pointer.set_position((screen.x, screen.y));
    }
    deactivate_protocol(&tracked.surface, pointer);
}

fn activate<D: SessionDriver>(surface: &WlSurface, pointer: &PointerHandle<Session<D>>) -> bool {
    with_pointer_constraint(surface, pointer, |constraint| {
        let Some(constraint) = constraint else {
            return false;
        };
        constraint.activate();
        true
    })
}

fn candidate_surface<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
    preferred: Option<&WlSurface>,
    current: Option<&TrackedConstraint>,
) -> Option<WlSurface> {
    preferred
        .filter(|surface| {
            owner_context(session, surface).is_some() && descriptor(surface, pointer).is_some()
        })
        .cloned()
        .or_else(|| {
            pointer.current_focus().filter(|surface| {
                owner_context(session, surface).is_some() && descriptor(surface, pointer).is_some()
            })
        })
        .or_else(|| {
            current
                .filter(|tracked| {
                    owner_context(session, &tracked.surface).is_some()
                        && descriptor(&tracked.surface, pointer).is_some()
                })
                .map(|tracked| tracked.surface.clone())
        })
}

fn nearest_valid_point(
    desired: Point<f64, Logical>,
    surface_size: Size<i32, Logical>,
    region: Option<&RegionAttributes>,
) -> Option<Point<f64, Logical>> {
    if surface_size.w <= 0 || surface_size.h <= 0 {
        return None;
    }
    let bounds = Rectangle::from_size(surface_size);
    let allowed = |point: Point<i32, Logical>| {
        bounds.contains(point) && region.is_none_or(|region| region.contains(point))
    };
    let rounded: Point<i32, Logical> = desired.to_i32_round();
    if bounds.to_f64().contains(desired)
        && region.is_none_or(|region| region.contains(desired.to_i32_floor()))
    {
        return Some(desired);
    }

    let mut xs = vec![
        0,
        surface_size.w - 1,
        rounded.x.clamp(0, surface_size.w - 1),
    ];
    let mut ys = vec![
        0,
        surface_size.h - 1,
        rounded.y.clamp(0, surface_size.h - 1),
    ];
    if let Some(region) = region {
        for (_, rect) in &region.rects {
            for x in [
                rect.loc.x - 1,
                rect.loc.x,
                rect.loc.x + rect.size.w - 1,
                rect.loc.x + rect.size.w,
            ] {
                xs.push(x.clamp(0, surface_size.w - 1));
            }
            for y in [
                rect.loc.y - 1,
                rect.loc.y,
                rect.loc.y + rect.size.h - 1,
                rect.loc.y + rect.size.h,
            ] {
                ys.push(y.clamp(0, surface_size.h - 1));
            }
        }
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();

    xs.into_iter()
        .flat_map(|x| ys.iter().copied().map(move |y| Point::from((x, y))))
        .filter(|point| allowed(*point))
        .min_by(|left, right| {
            let left_dx = f64::from(left.x) - desired.x;
            let left_dy = f64::from(left.y) - desired.y;
            let right_dx = f64::from(right.x) - desired.x;
            let right_dy = f64::from(right.y) - desired.y;
            (left_dx * left_dx + left_dy * left_dy)
                .total_cmp(&(right_dx * right_dx + right_dy * right_dy))
        })
        .map(Point::to_f64)
}

fn focus_candidate<D: SessionDriver>(
    session: &mut Session<D>,
    pointer: &PointerHandle<Session<D>>,
    surface: &WlSurface,
    context: &OwnerContext,
    constraint: &ConstraintDescriptor,
) -> bool {
    let screen = Point::from(session.pointer.position());
    let desired = constraint
        .position_hint
        .or_else(|| context.presentation.surface_from_screen(surface, screen));
    let Some(local) = desired.and_then(|point| {
        nearest_valid_point(point, context.surface_size, constraint.region.as_ref())
    }) else {
        return false;
    };
    let Some(origin) = context.presentation.surface_origin(surface) else {
        return false;
    };
    let Some(screen) = context.presentation.screen_from_surface(surface, local) else {
        return false;
    };
    let location = origin + local;
    session.pointer.set_position((screen.x, screen.y));
    pointer.motion(
        session,
        Some((surface.clone(), origin)),
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time: session.start_time.elapsed().as_millis() as u32,
        },
    );
    true
}

/// Reconciles Halley's tracked owner, Smithay's protocol state, compositor
/// focus, mapping, and live presentation geometry in one ordered transition.
///
/// Pointer focus is deliberately established before activation. Conversely,
/// an old constraint is explicitly deactivated before this function changes
/// focus, so one-shot lifetime and client notifications remain deterministic.
pub(super) fn reconcile<D: SessionDriver>(
    session: &mut Session<D>,
    pointer: &PointerHandle<Session<D>>,
    preferred: Option<&WlSurface>,
) {
    let tracked = session
        .pointer_constraints
        .active
        .clone()
        .or_else(|| protocol_active_focus(pointer));
    let candidate = candidate_surface(session, pointer, preferred, tracked.as_ref());
    let current_valid = tracked
        .as_ref()
        .is_some_and(|tracked| valid_active_owner(session, pointer, tracked));
    let current_is_candidate = tracked
        .as_ref()
        .zip(candidate.as_ref())
        .is_some_and(|(tracked, candidate)| &tracked.surface == candidate);
    let candidate_descriptor = candidate
        .as_ref()
        .and_then(|surface| descriptor(surface, pointer));
    let candidate_pointer_focused = candidate
        .as_ref()
        .is_some_and(|surface| pointer.current_focus().as_ref() == Some(surface));
    let decision = reconcile_decision(
        tracked.is_some(),
        current_is_candidate,
        current_valid,
        candidate.is_some(),
        candidate_pointer_focused,
        candidate_descriptor
            .as_ref()
            .is_some_and(|constraint| constraint.active),
    );

    if decision.deactivate_current
        && let Some(tracked) = tracked.as_ref()
    {
        deactivate_tracked(session, pointer, tracked);
        session.pointer_constraints.active = None;
    }

    let Some(candidate) = candidate else {
        session.pointer_constraints.active = None;
        return;
    };
    let Some(context) = owner_context(session, &candidate) else {
        session.pointer_constraints.active = None;
        return;
    };
    let Some(mut constraint) = descriptor(&candidate, pointer) else {
        session.pointer_constraints.active = None;
        return;
    };

    if decision.establish_focus
        && !focus_candidate(session, pointer, &candidate, &context, &constraint)
    {
        session.pointer_constraints.active = None;
        return;
    }
    if decision.activate_candidate {
        if !activate(&candidate, pointer) {
            session.pointer_constraints.active = None;
            return;
        }
        constraint.active = true;
    }
    if pointer.current_focus().as_ref() == Some(&candidate) && constraint.active {
        session.pointer_constraints.active = Some(TrackedConstraint {
            surface: candidate,
            kind: constraint.kind,
            position_hint: constraint.position_hint,
        });
        session.request_redraw();
    } else {
        session.pointer_constraints.active = None;
    }
}

pub(super) fn deactivate_before_focus_change<D: SessionDriver>(
    session: &mut Session<D>,
    next_focused_root: Option<&WlSurface>,
) {
    let Some(pointer) = session.seat.get_pointer() else {
        return;
    };
    let should_deactivate = session
        .pointer_constraints
        .active
        .as_ref()
        .is_some_and(|tracked| {
            let root = crate::wayland::compositor::root_surface(&tracked.surface);
            Some(&root) != next_focused_root
        });
    if should_deactivate {
        if let Some(tracked) = session.pointer_constraints.active.take() {
            deactivate_tracked(session, &pointer, &tracked);
        }
        pointer.frame(session);
    }
}

pub(super) fn deactivate_before_pointer_focus_change<D: SessionDriver>(
    session: &mut Session<D>,
    next_focus: Option<&WlSurface>,
) {
    let Some(pointer) = session.seat.get_pointer() else {
        return;
    };
    let should_deactivate = session
        .pointer_constraints
        .active
        .as_ref()
        .is_some_and(|tracked| Some(&tracked.surface) != next_focus);
    if should_deactivate {
        if let Some(tracked) = session.pointer_constraints.active.take() {
            deactivate_tracked(session, &pointer, &tracked);
        }
        pointer.frame(session);
    }
}

pub(super) fn deactivate_before_unmap<D: SessionDriver>(
    session: &mut Session<D>,
    root: &WlSurface,
) {
    let Some(pointer) = session.seat.get_pointer() else {
        return;
    };
    let should_deactivate = session
        .pointer_constraints
        .active
        .as_ref()
        .is_some_and(|tracked| crate::wayland::compositor::root_surface(&tracked.surface) == *root);
    if should_deactivate {
        if let Some(tracked) = session.pointer_constraints.active.take() {
            deactivate_tracked(session, &pointer, &tracked);
        }
        pointer.frame(session);
    }
}

pub(super) fn active<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
) -> Option<ActiveConstraint> {
    let tracked = session.pointer_constraints.active.as_ref()?;
    if !valid_active_owner(session, pointer, tracked) {
        return None;
    }
    let context = owner_context(session, &tracked.surface)?;
    let descriptor = descriptor(&tracked.surface, pointer)?;
    Some(ActiveConstraint {
        surface: tracked.surface.clone(),
        origin: context.presentation.surface_origin(&tracked.surface)?,
        kind: tracked.kind,
        region: descriptor.region,
        presentation: context.presentation,
    })
}

pub(super) fn has_active_lock<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
) -> bool {
    active(session, pointer).is_some_and(|constraint| constraint.kind == ConstraintKind::Locked)
}

pub(super) struct ActiveConstraint {
    pub surface: WlSurface,
    pub origin: Point<f64, Logical>,
    pub kind: ConstraintKind,
    region: Option<RegionAttributes>,
    presentation: WindowPresentation,
}

pub(super) fn cursor_visible<D: SessionDriver>(session: &Session<D>) -> bool {
    session.seat.get_pointer().is_none_or(|pointer| {
        active(session, &pointer).is_none_or(|constraint| constraint.kind != ConstraintKind::Locked)
    })
}

pub(super) fn allows_current_position<D: SessionDriver>(
    session: &Session<D>,
    constraint: &ActiveConstraint,
) -> bool {
    let screen = Point::from(session.pointer.position());
    constraint
        .presentation
        .surface_from_screen(&constraint.surface, screen)
        .is_some_and(|local| {
            constraint
                .region
                .as_ref()
                .is_none_or(|region| region.contains(local.to_i32_round()))
        })
}

pub(super) fn apply_position_hint<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
    location: Point<f64, Logical>,
) {
    if let Some(tracked) = session.pointer_constraints.active.as_mut()
        && tracked.surface == *surface
    {
        tracked.position_hint = Some(location);
    }
    if let Some(context) = owner_context(session, surface)
        && let Some(local) = nearest_valid_point(
            location,
            context.surface_size,
            descriptor(surface, pointer)
                .and_then(|state| state.region)
                .as_ref(),
        )
        && let Some(screen) = context.presentation.screen_from_surface(surface, local)
    {
        session.pointer.set_position((screen.x, screen.y));
        session.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::wayland::compositor::RectangleKind;

    fn decision(
        current_present: bool,
        current_is_candidate: bool,
        current_valid: bool,
        candidate_present: bool,
        candidate_pointer_focused: bool,
        candidate_protocol_active: bool,
    ) -> ReconcileDecision {
        reconcile_decision(
            current_present,
            current_is_candidate,
            current_valid,
            candidate_present,
            candidate_pointer_focused,
            candidate_protocol_active,
        )
    }

    #[test]
    fn startup_churn_retains_an_already_valid_owner() {
        assert_eq!(
            decision(true, true, true, true, true, true),
            ReconcileDecision {
                deactivate_current: false,
                establish_focus: false,
                activate_candidate: false,
            }
        );
    }

    #[test]
    fn stale_focus_after_resolution_change_is_reestablished_before_activation() {
        assert_eq!(
            decision(true, true, false, true, false, false),
            ReconcileDecision {
                deactivate_current: true,
                establish_focus: true,
                activate_candidate: true,
            }
        );
    }

    #[test]
    fn replacement_constraint_refreshes_geometry_even_on_the_same_surface() {
        assert_eq!(
            decision(true, true, false, true, true, false),
            ReconcileDecision {
                deactivate_current: true,
                establish_focus: true,
                activate_candidate: true,
            }
        );
    }

    #[test]
    fn focus_loss_and_unmap_deactivate_without_replacement() {
        assert_eq!(
            decision(true, false, false, false, false, false),
            ReconcileDecision {
                deactivate_current: true,
                establish_focus: false,
                activate_candidate: false,
            }
        );
    }

    #[test]
    fn persistent_constraint_reactivates_only_after_remap_and_exact_focus() {
        let unmapped = decision(true, false, false, false, false, false);
        let remapped_without_focus = decision(false, false, false, false, false, false);
        let remapped_and_focused = decision(false, false, false, true, true, false);

        assert!(unmapped.deactivate_current);
        assert_eq!(
            remapped_without_focus,
            ReconcileDecision {
                deactivate_current: false,
                establish_focus: false,
                activate_candidate: false,
            }
        );
        assert_eq!(
            remapped_and_focused,
            ReconcileDecision {
                deactivate_current: false,
                establish_focus: true,
                activate_candidate: true,
            }
        );
    }

    #[test]
    fn removed_oneshot_constraint_does_not_reactivate_after_remap() {
        let remapped_without_protocol_candidate = decision(false, false, false, false, true, false);
        assert_eq!(
            remapped_without_protocol_candidate,
            ReconcileDecision {
                deactivate_current: false,
                establish_focus: false,
                activate_candidate: false,
            }
        );
    }

    #[test]
    fn owner_replacement_deactivates_then_focuses_and_activates() {
        assert_eq!(
            decision(true, false, false, true, false, false),
            ReconcileDecision {
                deactivate_current: true,
                establish_focus: true,
                activate_candidate: true,
            }
        );
    }

    #[test]
    fn unfocused_or_unmapped_constraints_have_no_candidate() {
        assert_eq!(
            decision(false, false, false, false, false, false),
            ReconcileDecision {
                deactivate_current: false,
                establish_focus: false,
                activate_candidate: false,
            }
        );
    }

    #[test]
    fn confinement_chooses_nearest_point_outside_a_subtracted_hole() {
        let region = RegionAttributes {
            rects: vec![
                (
                    RectangleKind::Add,
                    Rectangle::new((0, 0).into(), (100, 100).into()),
                ),
                (
                    RectangleKind::Subtract,
                    Rectangle::new((40, 40).into(), (20, 20).into()),
                ),
            ],
        };

        assert_eq!(
            nearest_valid_point((50.0, 50.0).into(), (100, 100).into(), Some(&region)),
            Some(Point::from((50.0, 60.0)))
        );
    }

    #[test]
    fn locked_motion_is_relative_only() {
        assert_eq!(
            motion_disposition(Some(ConstraintKind::Locked), true),
            MotionDisposition::RelativeOnly
        );
    }
}
