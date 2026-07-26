use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::{
    RegionAttributes, SubsurfaceCachedState, get_parent, with_states,
};
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};

use super::{Session, SessionDriver};

type SurfaceOrigin = (WlSurface, Point<f64, Logical>);
type SurfaceChain = Vec<SurfaceOrigin>;

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

pub(super) struct ActiveConstraint {
    pub surface: WlSurface,
    pub origin: Point<f64, Logical>,
    pub kind: ConstraintKind,
    region: Option<RegionAttributes>,
}

fn surface_chain(surface: WlSurface, origin: Point<f64, Logical>) -> SurfaceChain {
    let mut chain = Vec::new();
    let mut current = surface;
    let mut current_origin = origin;
    loop {
        chain.push((current.clone(), current_origin));
        let Some(parent) = get_parent(&current) else {
            break;
        };
        let location = with_states(&current, |states| {
            states
                .cached_state
                .get::<SubsurfaceCachedState>()
                .current()
                .location
        });
        current_origin -= location.to_f64();
        current = parent;
    }
    chain
}

fn routed_surface_chain<D: SessionDriver>(session: &Session<D>) -> SurfaceChain {
    super::input::route_client_pointer(session)
        .and_then(|route| route.focus)
        .map_or_else(Vec::new, |(surface, origin)| surface_chain(surface, origin))
}

fn focused_route_chain<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
) -> Option<(Point<f64, Logical>, SurfaceChain)> {
    let route = super::input::route_client_pointer(session)?;
    let (surface, origin) = route.focus?;
    if pointer.current_focus().as_ref() != Some(&surface) {
        return None;
    }
    Some((route.location, surface_chain(surface, origin)))
}

fn constraint_at<D: SessionDriver>(
    surface: &WlSurface,
    origin: Point<f64, Logical>,
    pointer: &PointerHandle<Session<D>>,
) -> Option<ActiveConstraint> {
    with_pointer_constraint(surface, pointer, |constraint| {
        let constraint = constraint?;
        if !constraint.is_active() {
            return None;
        }
        let kind = match &*constraint {
            PointerConstraint::Confined(_) => ConstraintKind::Confined,
            PointerConstraint::Locked(_) => ConstraintKind::Locked,
        };
        Some(ActiveConstraint {
            surface: surface.clone(),
            origin,
            kind,
            region: constraint.region().cloned(),
        })
    })
}

fn active_locked_for_focus<D: SessionDriver>(
    pointer: &PointerHandle<Session<D>>,
) -> Option<ActiveConstraint> {
    let focus = pointer.current_focus()?;
    surface_chain(focus, pointer.current_location())
        .into_iter()
        .filter_map(|(surface, origin)| constraint_at(&surface, origin, pointer))
        .find(|constraint| constraint.kind == ConstraintKind::Locked)
}

pub(super) fn has_active_lock<D: SessionDriver>(pointer: &PointerHandle<Session<D>>) -> bool {
    active_locked_for_focus(pointer).is_some()
}

pub(super) fn active<D: SessionDriver>(
    session: &Session<D>,
    pointer: &PointerHandle<Session<D>>,
) -> Option<ActiveConstraint> {
    focused_route_chain(session, pointer)
        .and_then(|(_, chain)| {
            chain
                .into_iter()
                .find_map(|(surface, origin)| constraint_at(&surface, origin, pointer))
        })
        .or_else(|| active_locked_for_focus(pointer))
}

fn activate_at<D: SessionDriver>(
    surface: &WlSurface,
    origin: Point<f64, Logical>,
    location: Point<f64, Logical>,
    pointer: &PointerHandle<Session<D>>,
) -> bool {
    with_pointer_constraint(surface, pointer, |constraint| {
        let Some(constraint) = constraint else {
            return false;
        };
        if constraint.is_active()
            || constraint
                .region()
                .is_some_and(|region| !region.contains((location - origin).to_i32_round()))
        {
            return false;
        }
        constraint.activate();
        true
    })
}

fn chain_has_active<D: SessionDriver>(
    chain: &SurfaceChain,
    pointer: &PointerHandle<Session<D>>,
) -> bool {
    chain.iter().any(|(surface, _)| {
        with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|constraint| constraint.is_active())
        })
    })
}

pub(super) fn activate_new<D: SessionDriver>(
    session: &mut Session<D>,
    requested_surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
) {
    let Some((location, chain)) = focused_route_chain(session, pointer) else {
        return;
    };
    if chain_has_active(&chain, pointer) {
        return;
    }
    let Some((_, origin)) = chain
        .into_iter()
        .find(|(surface, _)| surface == requested_surface)
    else {
        return;
    };
    if activate_at(requested_surface, origin, location, pointer) {
        session.request_redraw();
    }
}

pub(super) fn activate_focused<D: SessionDriver>(
    session: &mut Session<D>,
    pointer: &PointerHandle<Session<D>>,
) {
    let Some((location, chain)) = focused_route_chain(session, pointer) else {
        return;
    };
    if chain_has_active(&chain, pointer) {
        return;
    }
    if chain
        .into_iter()
        .any(|(surface, origin)| activate_at(&surface, origin, location, pointer))
    {
        session.request_redraw();
    }
}

pub(super) fn cursor_visible<D: SessionDriver>(session: &Session<D>) -> bool {
    session
        .seat
        .get_pointer()
        .is_none_or(|pointer| !has_active_lock(&pointer))
}

pub(super) fn allows_current_position<D: SessionDriver>(
    session: &Session<D>,
    constraint: &ActiveConstraint,
) -> bool {
    let Some(route) = super::input::route_client_pointer(session) else {
        return false;
    };
    let Some((focus, origin)) = route.focus else {
        return false;
    };
    let Some((_, constraint_origin)) = surface_chain(focus, origin)
        .into_iter()
        .find(|(surface, _)| surface == &constraint.surface)
    else {
        return false;
    };
    constraint
        .region
        .as_ref()
        .is_none_or(|region| region.contains((route.location - constraint_origin).to_i32_round()))
}

pub(super) fn apply_position_hint<D: SessionDriver>(
    session: &Session<D>,
    surface: &WlSurface,
    pointer: &PointerHandle<Session<D>>,
    location: Point<f64, Logical>,
) {
    let active = with_pointer_constraint(surface, pointer, |constraint| {
        constraint.is_some_and(|constraint| constraint.is_active())
    });
    if !active {
        return;
    }
    if let Some((_, origin)) = routed_surface_chain(session)
        .into_iter()
        .find(|(candidate, _)| candidate == surface)
    {
        pointer.set_location(origin + location);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_motion_is_relative_only() {
        assert_eq!(
            motion_disposition(Some(ConstraintKind::Locked), true),
            MotionDisposition::RelativeOnly
        );
    }

    #[test]
    fn confinement_holds_only_positions_outside_its_region() {
        assert_eq!(
            motion_disposition(Some(ConstraintKind::Confined), false),
            MotionDisposition::Hold
        );
        assert_eq!(
            motion_disposition(Some(ConstraintKind::Confined), true),
            MotionDisposition::Apply
        );
        assert_eq!(motion_disposition(None, false), MotionDisposition::Apply);
    }
}
