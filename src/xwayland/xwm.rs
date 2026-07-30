use std::cmp::Reverse;
use std::os::fd::OwnedFd;
use std::sync::Mutex;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, WmWindowProperty, WmWindowType, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::session::{Session, SessionDriver};
use crate::wayland::fullscreen::{ExternalConfigureResult, ExternalTransactionRequest};

use super::lifecycle::{MapAdmission, OpeningPlacement, map_admission};
use super::{
    OverrideRedirectPlacement, PendingOverrideRedirect, PendingWindow, SavedNormalSize,
    WindowIdentity,
};

#[derive(Clone, Debug)]
struct MaximizeRestore {
    geometry: Rectangle<i32, Logical>,
    output: Option<String>,
}

#[derive(Default, Debug)]
struct MaximizeFullscreenState {
    active: bool,
    restore: Option<MaximizeRestore>,
}

#[derive(Default)]
struct MaximizeFullscreen(Mutex<MaximizeFullscreenState>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenRequestOrigin {
    Initial,
    Client,
    Compositor,
    Maximize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaximizeToggleAction {
    Enter,
    Exit,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalPresentationPolicy {
    Initial,
    Opening,
    Confined,
    Animated,
}

fn external_presentation_policy(
    origin: FullscreenRequestOrigin,
    opening: bool,
    confined: bool,
) -> ExternalPresentationPolicy {
    if confined {
        ExternalPresentationPolicy::Confined
    } else if opening {
        ExternalPresentationPolicy::Opening
    } else if origin == FullscreenRequestOrigin::Initial {
        ExternalPresentationPolicy::Initial
    } else {
        ExternalPresentationPolicy::Animated
    }
}

fn window_for_surface<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
) -> Option<Window> {
    session
        .wayland
        .space
        .elements()
        .find(|window| {
            window
                .x11_surface()
                .is_some_and(|candidate| candidate == surface)
        })
        .cloned()
        .or_else(|| {
            session
                .nodes
                .records()
                .find(|record| {
                    record
                        .window
                        .x11_surface()
                        .is_some_and(|candidate| candidate == surface)
                })
                .map(|record| record.window.clone())
        })
}

fn window_identity(surface: &X11Surface) -> Option<WindowIdentity> {
    if surface.is_override_redirect()
        || surface.is_transient_for().is_some()
        || !matches!(surface.window_type(), None | Some(WmWindowType::Normal))
    {
        return None;
    }
    let instance = surface.instance();
    let class = surface.class();
    if instance.is_empty() || class.is_empty() {
        return None;
    }
    Some(WindowIdentity {
        instance,
        class,
        title: surface.title(),
    })
}

fn is_steam_main_window(surface: &X11Surface) -> bool {
    window_identity(surface).is_some_and(|identity| {
        identity.instance == "steamwebhelper"
            && identity.class.eq_ignore_ascii_case("steam")
            && identity.title == "Steam"
    })
}

fn maximize_restore(surface: &X11Surface) -> Option<MaximizeRestore> {
    surface
        .user_data()
        .get::<MaximizeFullscreen>()
        .and_then(|state| {
            let state = state.0.lock().expect("X11 maximize state lock poisoned");
            state.active.then(|| state.restore.clone()).flatten()
        })
}

fn maximize_origin_active(surface: &X11Surface) -> bool {
    surface
        .user_data()
        .get::<MaximizeFullscreen>()
        .is_some_and(|state| {
            state
                .0
                .lock()
                .expect("X11 maximize state lock poisoned")
                .active
        })
}

fn remember_normal_size<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    size: Size<i32, Logical>,
) {
    if size.w <= 0
        || size.h <= 0
        || surface.is_fullscreen()
        || surface.is_maximized()
        || maximize_origin_active(surface)
    {
        return;
    }
    if is_steam_main_window(surface)
        && window_for_surface(session, surface)
            .and_then(|window| crate::wayland::window_output_name(&window))
            .and_then(|name| output_named(session, &name))
            .and_then(|output| session.wayland.space.output_geometry(&output))
            .is_some_and(|geometry| size_fills_output(size, geometry.size))
    {
        return;
    }
    let Some(identity) = window_identity(surface) else {
        return;
    };
    session
        .xwayland
        .normal_sizes
        .insert(identity, SavedNormalSize(size));
}

fn cached_normal_size<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
) -> Option<Size<i32, Logical>> {
    let identity = window_identity(surface)?;
    session
        .xwayland
        .normal_sizes
        .get(&identity)
        .map(|saved| saved.0)
}

fn size_fills_output(size: Size<i32, Logical>, output_size: Size<i32, Logical>) -> bool {
    size.w >= output_size.w && size.h >= output_size.h
}

fn steam_recovery_size(
    surface: &X11Surface,
    output_size: Size<i32, Logical>,
) -> Size<i32, Logical> {
    recovery_window_size(output_size, surface.min_size(), surface.max_size())
}

fn recovery_window_size(
    output_size: Size<i32, Logical>,
    minimum: Option<Size<i32, Logical>>,
    maximum: Option<Size<i32, Logical>>,
) -> Size<i32, Logical> {
    let mut size = Size::from((
        output_size.w.saturating_mul(3) / 4,
        output_size.h.saturating_mul(3) / 4,
    ));
    if let Some(minimum) = minimum {
        size.w = size.w.max(minimum.w);
        size.h = size.h.max(minimum.h);
    }
    if let Some(maximum) = maximum {
        size.w = size.w.min(maximum.w);
        size.h = size.h.min(maximum.h);
    }
    size.w = size.w.clamp(1, output_size.w.max(1));
    size.h = size.h.clamp(1, output_size.h.max(1));
    size
}

pub(super) fn surface_associated<D: SessionDriver>(
    session: &mut Session<D>,
    wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    surface: X11Surface,
) {
    consider_map(session, surface.window_id(), Some(&wl_surface));
}

pub(super) fn handle_commit<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    let xid = session
        .xwayland
        .pending_windows
        .iter()
        .find_map(|(xid, pending)| {
            pending
                .surface
                .wl_surface()
                .is_some_and(|candidate| candidate == *surface)
                .then_some(*xid)
        });
    if let Some(xid) = xid {
        consider_map(session, xid, Some(surface));
    }
}

fn consider_map<D: SessionDriver>(
    session: &mut Session<D>,
    xid: u32,
    associated: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
) {
    let pending = session.xwayland.pending_windows.contains_key(&xid);
    let surface = session
        .xwayland
        .pending_windows
        .get(&xid)
        .and_then(|pending| pending.surface.wl_surface());
    let associated = associated.or(surface.as_ref());
    let has_buffer = associated.is_some_and(|surface| {
        with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false)
    });
    match map_admission(pending, associated.is_some(), has_buffer) {
        MapAdmission::Wait => {
            eventline::debug!(
                "xwayland: mapping pending xid={xid} associated={} buffer={has_buffer}",
                associated.is_some()
            );
        }
        MapAdmission::Ignore => {}
        MapAdmission::Admit => admit_window(session, xid),
    }
}

fn admit_window<D: SessionDriver>(session: &mut Session<D>, xid: u32) {
    let Some(PendingWindow {
        surface,
        window,
        initial_size,
    }) = session.xwayland.pending_windows.remove(&xid)
    else {
        return;
    };
    let saved_maximize = maximize_restore(&surface);
    let mut opening_size = OpeningPlacement::preferred_size(initial_size, window.geometry().size);
    let initial_placement = crate::window::routing::initial_window_placement(
        &session.wayland,
        &session.cameras,
        session.driver.primary_output(),
        opening_size,
    );
    let mut output = initial_placement.output;
    let mut location = initial_placement.location;

    if let Some(restore) = saved_maximize.as_ref() {
        opening_size = restore.geometry.size;
        if let Some(saved_output) = restore
            .output
            .as_deref()
            .and_then(|name| output_named(session, name))
        {
            output = saved_output;
            location = restore.geometry.loc;
        } else {
            let placement = crate::window::routing::initial_window_placement(
                &session.wayland,
                &session.cameras,
                session.driver.primary_output(),
                opening_size,
            );
            output = placement.output;
            location = placement.location;
        }
    } else if is_steam_main_window(&surface) {
        let output_size = session
            .wayland
            .space
            .output_geometry(&output)
            .map(|geometry| geometry.size)
            .unwrap_or(opening_size);
        let suspicious = surface.is_fullscreen()
            || surface.is_maximized()
            || size_fills_output(opening_size, output_size);
        let recovered = cached_normal_size(session, &surface)
            .filter(|size| !size_fills_output(*size, output_size))
            .or_else(|| suspicious.then(|| steam_recovery_size(&surface, output_size)));
        if let Some(recovered) = recovered {
            opening_size = Size::from((
                recovered.w.clamp(1, output_size.w.max(1)),
                recovered.h.clamp(1, output_size.h.max(1)),
            ));
            let placement = crate::window::routing::initial_window_placement(
                &session.wayland,
                &session.cameras,
                session.driver.primary_output(),
                opening_size,
            );
            output = placement.output;
            location = placement.location;
            if let Err(err) = surface.set_fullscreen(false) {
                eventline::warn!("xwayland: failed to clear stale Steam fullscreen state: {err}");
            }
            if let Err(err) = surface.set_maximized(false) {
                eventline::warn!("xwayland: failed to clear stale Steam maximized state: {err}");
            }
            eventline::debug!(
                "xwayland: recovered Steam main window xid={xid} to {}x{}",
                opening_size.w,
                opening_size.h
            );
        }
    }

    let opening_geometry = Rectangle::new(location, opening_size);
    if let Err(err) = surface.configure(opening_geometry) {
        eventline::warn!("xwayland: failed to prepare centered opening geometry: {err}");
    }
    crate::wayland::set_window_output(&window, &output);
    if let Some(wl_surface) = window.wl_surface() {
        crate::session::opening::prepare(session, wl_surface.into_owned(), &output);
    }
    session
        .wayland
        .space
        .map_element(window.clone(), location, true);
    if let Some(wl_surface) = window.wl_surface() {
        session.nodes.register_mapped(
            &session.wayland.space,
            wl_surface.as_ref(),
            session.start_time.elapsed().as_millis() as u64,
        );
        crate::nodes::reconcile_landmarks(session, Some(&output.name()));
        crate::session::closing::mapped(session, wl_surface.as_ref());
    }
    session.xwayland.opening_placements.insert(
        xid,
        OpeningPlacement::new(
            opening_geometry,
            saved_maximize
                .as_ref()
                .map_or(initial_size, |restore| restore.geometry.size),
        ),
    );
    let started = window.wl_surface().is_some_and(|wl_surface| {
        crate::session::opening::start(
            session,
            wl_surface.into_owned(),
            &output,
            crate::frame_clock::monotonic_now(),
        )
    });
    if saved_maximize.is_some() {
        if let Err(err) = surface.set_maximized(true) {
            eventline::warn!("xwayland: failed to retain remapped maximized state: {err}");
        }
        enter_fullscreen(session, &surface, FullscreenRequestOrigin::Maximize);
    } else if surface.is_fullscreen() {
        enter_fullscreen(session, &surface, FullscreenRequestOrigin::Initial);
    } else if surface.is_maximized() {
        // Startup maximize hints are not user decoration clicks. Keeping the
        // centered opening prevents monitor-sized remaps from becoming the
        // client's next remembered "normal" size (notably Steam).
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to suppress initial maximized state: {err}");
        }
    }
    if !surface.is_override_redirect() {
        crate::session::focus_window(session, &window, SERIAL_COUNTER.next_serial());
    }
    refresh_override_redirect_owners(session);
    eventline::debug!(
        "xwayland: mapping admitted xid={xid} fullscreen={} maximized={} animated={started}",
        surface.is_fullscreen(),
        surface.is_maximized()
    );
    session.request_redraw();
}

fn output_for_geometry<D: SessionDriver>(
    session: &Session<D>,
    geometry: Rectangle<i32, Logical>,
) -> Option<Output> {
    let center = Point::<f64, Logical>::from((
        f64::from(geometry.loc.x) + f64::from(geometry.size.w) / 2.0,
        f64::from(geometry.loc.y) + f64::from(geometry.size.h) / 2.0,
    ));
    session
        .wayland
        .space
        .output_under(center)
        .next()
        .cloned()
        .or_else(|| crate::wayland::focus::selected_output(&session.wayland).cloned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverrideRedirectOwnerSource {
    Transient,
    WindowGroup,
}

struct OverrideRedirectOwner {
    window: Window,
    source: OverrideRedirectOwnerSource,
}

struct OverrideRedirectResolution {
    owner: Option<OverrideRedirectOwner>,
    output: Option<Output>,
}

fn point_rect_distance_squared(
    point: Point<i32, Logical>,
    geometry: Rectangle<i32, Logical>,
) -> i64 {
    let right = geometry.loc.x.saturating_add(geometry.size.w);
    let bottom = geometry.loc.y.saturating_add(geometry.size.h);
    let dx = if point.x < geometry.loc.x {
        i64::from(geometry.loc.x - point.x)
    } else if point.x > right {
        i64::from(point.x - right)
    } else {
        0
    };
    let dy = if point.y < geometry.loc.y {
        i64::from(geometry.loc.y - point.y)
    } else if point.y > bottom {
        i64::from(point.y - bottom)
    } else {
        0
    };
    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
}

#[derive(Clone, Copy)]
struct OverrideRedirectIdentity<'a> {
    pid: Option<u32>,
    class: &'a str,
    instance: &'a str,
}

fn override_redirect_owner_rank(
    popup: OverrideRedirectIdentity<'_>,
    candidate: OverrideRedirectIdentity<'_>,
    center: Point<i32, Logical>,
    candidate_geometry: Rectangle<i32, Logical>,
    stack_index: usize,
) -> (bool, bool, bool, Reverse<i64>, usize) {
    (
        popup.pid.is_some() && popup.pid == candidate.pid,
        !popup.class.is_empty()
            && popup.class == candidate.class
            && popup.instance == candidate.instance,
        candidate_geometry.contains(center),
        Reverse(point_rect_distance_squared(center, candidate_geometry)),
        stack_index,
    )
}

fn mapped_managed_x11_window<D: SessionDriver>(session: &Session<D>, xid: u32) -> Option<Window> {
    session.wayland.space.elements().find_map(|window| {
        window
            .x11_surface()
            .filter(|candidate| candidate.window_id() == xid && !candidate.is_override_redirect())
            .map(|_| window.clone())
    })
}

fn transient_override_redirect_owner<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
) -> Option<Window> {
    let mut xid = surface.is_transient_for()?;
    let mut visited = Vec::new();
    for _ in 0..16 {
        if visited.contains(&xid) {
            return None;
        }
        visited.push(xid);
        if let Some(owner) = mapped_managed_x11_window(session, xid) {
            return Some(owner);
        }
        let candidate = session.wayland.space.elements().find_map(|window| {
            window
                .x11_surface()
                .filter(|candidate| candidate.window_id() == xid)
                .cloned()
        })?;
        xid = candidate.is_transient_for()?;
    }
    None
}

fn window_group_override_redirect_owner<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
    geometry: Rectangle<i32, Logical>,
) -> Option<Window> {
    let group = surface.hints()?.window_group?;
    let center = Point::from((
        geometry.loc.x.saturating_add(geometry.size.w / 2),
        geometry.loc.y.saturating_add(geometry.size.h / 2),
    ));
    let popup_pid = surface.pid();
    let popup_class = surface.class();
    let popup_instance = surface.instance();

    session
        .wayland
        .space
        .elements()
        .enumerate()
        .filter_map(|(stack_index, window)| {
            let candidate = window.x11_surface()?;
            if candidate.is_override_redirect() || candidate.hints()?.window_group != Some(group) {
                return None;
            }
            let candidate_geometry = session.wayland.space.element_geometry(window)?;
            Some((
                override_redirect_owner_rank(
                    OverrideRedirectIdentity {
                        pid: popup_pid,
                        class: &popup_class,
                        instance: &popup_instance,
                    },
                    OverrideRedirectIdentity {
                        pid: candidate.pid(),
                        class: &candidate.class(),
                        instance: &candidate.instance(),
                    },
                    center,
                    candidate_geometry,
                    stack_index,
                ),
                window.clone(),
            ))
        })
        .max_by_key(|(key, _)| *key)
        .map(|(_, window)| window)
}

fn resolve_override_redirect_owner<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
    geometry: Rectangle<i32, Logical>,
) -> Option<OverrideRedirectOwner> {
    transient_override_redirect_owner(session, surface)
        .map(|window| OverrideRedirectOwner {
            window,
            source: OverrideRedirectOwnerSource::Transient,
        })
        .or_else(|| {
            window_group_override_redirect_owner(session, surface, geometry).map(|window| {
                OverrideRedirectOwner {
                    window,
                    source: OverrideRedirectOwnerSource::WindowGroup,
                }
            })
        })
}

fn output_named<D: SessionDriver>(session: &Session<D>, name: &str) -> Option<Output> {
    session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == name)
        .cloned()
}

fn override_redirect_resolution<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
    geometry: Rectangle<i32, Logical>,
) -> OverrideRedirectResolution {
    let owner = resolve_override_redirect_owner(session, surface, geometry);
    let output = owner
        .as_ref()
        .and_then(|owner| crate::wayland::window_output_name(&owner.window))
        .and_then(|name| output_named(session, &name))
        .or_else(|| output_for_geometry(session, geometry));
    OverrideRedirectResolution { owner, output }
}

fn apply_override_redirect_resolution(window: &Window, resolution: &OverrideRedirectResolution) {
    if let Some(owner) = resolution.owner.as_ref() {
        if !crate::wayland::inherit_window_output(window, &owner.window)
            && let Some(output) = resolution.output.as_ref()
        {
            crate::wayland::set_window_output(window, output);
        }
        if let Some(surface) = owner.window.x11_surface() {
            crate::wayland::set_window_presentation_owner(window, Some(surface.window_id()));
        }
    } else {
        crate::wayland::set_window_presentation_owner(window, None);
        if let Some(output) = resolution.output.as_ref() {
            crate::wayland::set_window_output(window, output);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverrideRedirectStackAction {
    Bottom,
    Above(u32),
    PreserveMissing(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverrideRedirectMapAdmission {
    ImmediateFresh,
    ImmediateConfigured,
    AwaitConfigure,
}

fn override_redirect_map_admission(
    first_map: bool,
    has_authoritative_placement: bool,
) -> OverrideRedirectMapAdmission {
    if has_authoritative_placement {
        OverrideRedirectMapAdmission::ImmediateConfigured
    } else if first_map {
        OverrideRedirectMapAdmission::ImmediateFresh
    } else {
        OverrideRedirectMapAdmission::AwaitConfigure
    }
}

fn override_redirect_stack_action(
    above_sibling: Option<u32>,
    sibling_is_mapped: bool,
) -> OverrideRedirectStackAction {
    match above_sibling {
        None => OverrideRedirectStackAction::Bottom,
        Some(sibling) if sibling_is_mapped => OverrideRedirectStackAction::Above(sibling),
        Some(sibling) => OverrideRedirectStackAction::PreserveMissing(sibling),
    }
}

fn restack_override_redirect<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    above_sibling: Option<u32>,
) -> OverrideRedirectStackAction {
    // ConfigureNotify reports the X server's completed stack operation. This
    // only mirrors that order in the compositor scene; it never sends a WM
    // restack request back to an override-redirect client.
    let reference = above_sibling.and_then(|sibling| {
        session
            .wayland
            .space
            .elements()
            .find(|candidate| {
                candidate
                    .x11_surface()
                    .is_some_and(|surface| surface.window_id() == sibling)
            })
            .cloned()
    });
    let action = override_redirect_stack_action(above_sibling, reference.is_some());
    match (&action, reference) {
        (OverrideRedirectStackAction::Bottom, _) => {
            // X11 defines a missing above_sibling as the bottom of the sibling
            // stack, not the top.
            session.wayland.space.lower_element(window);
        }
        (OverrideRedirectStackAction::Above(_), Some(reference)) => {
            // The configured window is immediately on top of above_sibling.
            // The sibling may be a managed owner or another popup.
            session
                .wayland
                .space
                .raise_element_above(window, &reference, false);
        }
        (OverrideRedirectStackAction::PreserveMissing(_), _) => {
            // Smithay may report an X sibling which has no compositor element
            // (or was destroyed earlier in this dispatch). Relocation already
            // preserves the last known order, which is safer than hiding the
            // popup or spuriously raising it.
        }
        (OverrideRedirectStackAction::Above(_), None) => {
            unreachable!("mapped sibling action requires a reference")
        }
    }
    action
}

fn override_redirect_output_name(window: &Window) -> Option<String> {
    crate::wayland::window_output_name(window)
}

fn describe_override_redirect_map(
    surface: &X11Surface,
    geometry: Rectangle<i32, Logical>,
    output: Option<&str>,
    resolution: &OverrideRedirectResolution,
) {
    let owner = resolution
        .owner
        .as_ref()
        .and_then(|owner| owner.window.x11_surface())
        .map(|owner| owner.window_id());
    let owner_source = resolution.owner.as_ref().map(|owner| owner.source);
    eventline::debug!(
        "xwayland: mapped override-redirect xid={} owner={owner:?} owner_source={owner_source:?} output={output:?} geometry={geometry:?}",
        surface.window_id(),
    );
}

fn describe_override_redirect_configure(
    surface: &X11Surface,
    geometry: Rectangle<i32, Logical>,
    output: Option<&str>,
    resolution: &OverrideRedirectResolution,
    above_sibling: Option<u32>,
    action: OverrideRedirectStackAction,
) {
    let owner = resolution
        .owner
        .as_ref()
        .and_then(|owner| owner.window.x11_surface())
        .map(|owner| owner.window_id());
    let owner_source = resolution.owner.as_ref().map(|owner| owner.source);
    eventline::debug!(
        "xwayland: configured override-redirect xid={} owner={owner:?} owner_source={owner_source:?} output={output:?} geometry={geometry:?} above_sibling={above_sibling:?} stack={action:?}",
        surface.window_id(),
    );
}

fn map_override_redirect<D: SessionDriver>(
    session: &mut Session<D>,
    surface: X11Surface,
    geometry: Rectangle<i32, Logical>,
    above: Option<u32>,
    mirror_configure_stack: bool,
) {
    if window_for_surface(session, &surface).is_some() {
        return;
    }
    let resolution = override_redirect_resolution(session, &surface, geometry);
    let window = Window::new_x11_window(surface.clone());
    apply_override_redirect_resolution(&window, &resolution);
    // ICCCM override-redirect windows are client-managed. Mapping one must
    // not activate it or transfer WM focus away from its managed owner.
    session
        .wayland
        .space
        .map_element(window.clone(), geometry.loc, false);
    if mirror_configure_stack {
        restack_override_redirect(session, &window, above);
    }
    let output = override_redirect_output_name(&window);
    describe_override_redirect_map(&surface, geometry, output.as_deref(), &resolution);
    session.request_redraw();
}

fn cancel_pending_override_redirect<D: SessionDriver>(
    session: &mut Session<D>,
    xid: u32,
) -> Option<PendingOverrideRedirect> {
    let pending = session.xwayland.pending_override_redirects.remove(&xid)?;
    session.xwayland.loop_handle.remove(pending.timer);
    Some(pending)
}

fn defer_override_redirect_remap<D: SessionDriver>(
    session: &mut Session<D>,
    surface: X11Surface,
    geometry: Rectangle<i32, Logical>,
) {
    const REMAP_GRACE: Duration = Duration::from_millis(8);

    let xid = surface.window_id();
    let loop_handle = session.xwayland.loop_handle.clone();
    let timer = loop_handle.insert_source(Timer::from_duration(REMAP_GRACE), move |_, _, session| {
        if let Some(pending) = session.xwayland.pending_override_redirects.remove(&xid) {
            eventline::debug!(
                "xwayland: override-redirect xid={xid} remapped without a new configure; using retained geometry"
            );
            map_override_redirect(session, pending.surface, pending.geometry, None, false);
        }
        TimeoutAction::Drop
    });
    match timer {
        Ok(timer) => {
            session.xwayland.pending_override_redirects.insert(
                xid,
                PendingOverrideRedirect {
                    surface,
                    geometry,
                    timer,
                },
            );
            eventline::debug!(
                "xwayland: deferring reused override-redirect xid={xid} for authoritative placement"
            );
        }
        Err(err) => {
            eventline::warn!("xwayland: failed to defer override-redirect xid={xid} remap: {err}");
            map_override_redirect(session, surface, geometry, None, false);
        }
    }
}

fn refresh_override_redirect_owners<D: SessionDriver>(session: &mut Session<D>) {
    let windows = session
        .wayland
        .space
        .elements()
        .filter_map(|window| {
            let surface = window.x11_surface()?.clone();
            surface.is_override_redirect().then(|| {
                let geometry = surface.geometry();
                (window.clone(), surface, geometry)
            })
        })
        .collect::<Vec<_>>();
    let mut changed = false;
    for (window, surface, geometry) in windows {
        let previous_owner = crate::wayland::window_presentation_owner(&window);
        let previous_output = crate::wayland::window_output_name(&window);
        let resolution = override_redirect_resolution(session, &surface, geometry);
        apply_override_redirect_resolution(&window, &resolution);
        let owner = crate::wayland::window_presentation_owner(&window);
        let output = crate::wayland::window_output_name(&window);
        if previous_owner != owner || previous_output != output {
            eventline::debug!(
                "xwayland: refreshed override-redirect xid={} owner={previous_owner:?}->{owner:?} output={previous_output:?}->{output:?}",
                surface.window_id()
            );
            changed = true;
        }
    }
    if changed {
        session.request_redraw();
    }
}

fn enter_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    origin: FullscreenRequestOrigin,
) {
    set_external_fullscreen(session, surface, true, origin);
}

fn leave_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    origin: FullscreenRequestOrigin,
) {
    set_external_fullscreen(session, surface, false, origin);
}

fn set_external_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    fullscreen: bool,
    origin: FullscreenRequestOrigin,
) {
    // Maximize uses the fullscreen presentation machinery internally, but it
    // must remain EWMH maximize to the X11 client. Advertising both
    // _NET_WM_STATE_MAXIMIZED_* and _NET_WM_STATE_FULLSCREEN confuses Steam's
    // client-side decoration state, so its second button press never asks to
    // unmaximize. Hyprland keeps this same client/internal-state distinction.
    let update_client_fullscreen = origin != FullscreenRequestOrigin::Maximize;
    if fullscreen && origin != FullscreenRequestOrigin::Maximize {
        let mut maximize = surface
            .user_data()
            .get_or_insert_threadsafe(MaximizeFullscreen::default)
            .0
            .lock()
            .expect("X11 maximize state lock poisoned");
        maximize.active = false;
        maximize.restore = None;
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to clear maximized state for fullscreen: {err}");
        }
    }
    if session
        .xwayland
        .pending_windows
        .contains_key(&surface.window_id())
    {
        if update_client_fullscreen && let Err(err) = surface.set_fullscreen(fullscreen) {
            eventline::warn!("xwayland: failed to update pending fullscreen state: {err}");
        }
        return;
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    if update_client_fullscreen && let Err(err) = surface.set_fullscreen(fullscreen) {
        eventline::warn!("xwayland: failed to update fullscreen state: {err}");
    }
    if session
        .fullscreen
        .external_desired_matches(&window, fullscreen)
    {
        eventline::debug!(
            "xwayland: coalesced fullscreen request xid={} fullscreen={fullscreen} \
             origin={origin:?}",
            surface.window_id()
        );
        crate::session::reconcile_pointer_constraints(session);
        return;
    }
    let opening = window.wl_surface().is_some_and(|wl_surface| {
        session
            .window_open_animations
            .is_animating(wl_surface.as_ref(), crate::frame_clock::monotonic_now())
    });
    let policy = external_presentation_policy(
        origin,
        opening,
        crate::session::has_active_pointer_confinement(session),
    );
    if policy == ExternalPresentationPolicy::Opening {
        preserve_opening_center(session, surface, &window);
    }
    let animate = policy == ExternalPresentationPolicy::Animated
        && capture_fullscreen_snapshot(session, &window, fullscreen);
    if policy == ExternalPresentationPolicy::Opening {
        request_opening_fullscreen(session, surface, &window, fullscreen);
    } else if animate {
        let request = if fullscreen {
            session
                .fullscreen
                .request_external_animated(&mut session.wayland, &window)
        } else {
            session.fullscreen.unrequest_external_animated(&window)
        };
        match request {
            Some(ExternalTransactionRequest::Configure(geometry)) => {
                if let Err(err) = surface.configure(geometry) {
                    eventline::warn!("xwayland: failed to configure animated fullscreen: {err}");
                    remove_fullscreen_snapshot(session, &window);
                    settle_external_immediately(session, surface, &window, fullscreen);
                } else {
                    eventline::debug!(
                        "xwayland: fullscreen policy xid={} fullscreen={fullscreen} \
                         origin={origin:?} policy=animated",
                        surface.window_id(),
                    );
                }
            }
            Some(ExternalTransactionRequest::NoChange) => {
                remove_fullscreen_snapshot(session, &window);
            }
            None => {
                remove_fullscreen_snapshot(session, &window);
                settle_external_immediately(session, surface, &window, fullscreen);
            }
        }
    } else {
        let reason = presentation_policy_name(policy);
        eventline::debug!(
            "xwayland: fullscreen policy xid={} fullscreen={fullscreen} \
             origin={origin:?} policy={reason}",
            surface.window_id(),
        );
        settle_external_immediately(session, surface, &window, fullscreen);
    }
    crate::session::reconcile_pointer_constraints(session);
}

fn preserve_opening_center<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
) {
    let Some(placement) = session
        .xwayland
        .opening_placements
        .get(&surface.window_id())
        .copied()
    else {
        return;
    };
    let centered = placement.centered(window.geometry().size);
    session.wayland.space.relocate_element(window, centered.loc);
}

fn request_opening_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
    fullscreen: bool,
) {
    let now = crate::frame_clock::monotonic_now();
    let Some((output, current_bounds)) = opening_presentation_bounds(session, window, now) else {
        settle_external_immediately(session, surface, window, fullscreen);
        return;
    };
    let request = if fullscreen {
        let restore = session
            .xwayland
            .opening_placements
            .get(&surface.window_id())
            .and_then(|placement| placement.restore_geometry());
        session
            .fullscreen
            .request_external_opening(&mut session.wayland, window, restore)
    } else {
        session.fullscreen.unrequest_external_opening(window)
    };
    match request {
        Some(ExternalTransactionRequest::Configure(geometry)) => {
            let target_bounds = opening_target_bounds(session, &output, geometry, fullscreen)
                .unwrap_or_else(|| Rectangle::new((0, 0).into(), geometry.size.to_physical(1)));
            let Some(wl_surface) = window.wl_surface() else {
                settle_external_immediately(session, surface, window, fullscreen);
                return;
            };
            session.window_open_animations.retarget(
                wl_surface.as_ref(),
                now,
                current_bounds,
                target_bounds,
            );
            if let Err(err) = surface.configure(geometry) {
                eventline::warn!("xwayland: failed to configure opening fullscreen: {err}");
                settle_external_immediately(session, surface, window, fullscreen);
            } else {
                eventline::debug!(
                    "xwayland: fullscreen policy xid={} fullscreen={fullscreen} policy=opening",
                    surface.window_id()
                );
            }
        }
        Some(ExternalTransactionRequest::NoChange) => {}
        None => settle_external_immediately(session, surface, window, fullscreen),
    }
}

fn opening_presentation_bounds<D: SessionDriver>(
    session: &Session<D>,
    window: &Window,
    now: std::time::Duration,
) -> Option<(Output, Rectangle<i32, smithay::utils::Physical>)> {
    let output = crate::wayland::window_output_name(window)
        .and_then(|name| {
            session
                .wayland
                .space
                .outputs()
                .find(|output| output.name() == name)
        })
        .or_else(|| crate::wayland::focus::selected_output(&session.wayland))
        .cloned()?;
    let output_geometry = session.wayland.space.output_geometry(&output)?;
    let presentation = crate::input::presentation::WindowPresentation::for_window(
        &session.wayland.space,
        &session.cameras,
        &session.window_open_animations,
        &session.fullscreen,
        &session.maximize,
        window,
        &output,
        now,
    )?;
    let local = presentation.visual_geometry().loc - output_geometry.loc;
    Some((
        output,
        Rectangle::new(
            local.to_physical(1),
            presentation.visual_geometry().size.to_physical(1),
        ),
    ))
}

fn opening_target_bounds<D: SessionDriver>(
    session: &Session<D>,
    output: &Output,
    geometry: Rectangle<i32, Logical>,
    fullscreen: bool,
) -> Option<Rectangle<i32, smithay::utils::Physical>> {
    let output_geometry = session.wayland.space.output_geometry(output)?;
    let output_size = output_geometry.size.to_physical(1);
    if fullscreen {
        return Some(Rectangle::new((0, 0).into(), output_size));
    }
    let view = session.cameras.view(&output.name())?;
    Some(crate::backend::camera_rect(
        geometry.to_physical(1),
        crate::camera::global_center(view.center, output_geometry),
        output_size,
        view.scale,
    ))
}

fn presentation_policy_name(policy: ExternalPresentationPolicy) -> &'static str {
    match policy {
        ExternalPresentationPolicy::Initial => "initial",
        ExternalPresentationPolicy::Opening => "opening",
        ExternalPresentationPolicy::Confined => "confined",
        ExternalPresentationPolicy::Animated => "snapshot-fallback",
    }
}

fn settle_external_immediately<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
    fullscreen: bool,
) {
    let geometry = if fullscreen {
        session
            .fullscreen
            .request_external(&mut session.wayland, window)
    } else {
        session
            .fullscreen
            .unrequest_external(&mut session.wayland, window)
    };
    if let Some(geometry) = geometry
        && let Err(err) = surface.configure(geometry)
    {
        eventline::warn!("xwayland: failed to configure fullscreen window: {err}");
    }
}

fn capture_fullscreen_snapshot<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    fullscreen: bool,
) -> bool {
    let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
        return false;
    };
    if !session
        .fullscreen
        .should_capture_external_snapshot(&surface, fullscreen)
    {
        return false;
    }
    let textures = &mut session.fullscreen_textures;
    match session
        .driver
        .with_renderer(|renderer| textures.capture_previous(renderer, window))
    {
        Ok(()) => true,
        Err(err) => {
            eventline::warn!("fullscreen: failed to capture X11 window texture: {err}");
            false
        }
    }
}

fn remove_fullscreen_snapshot<D: SessionDriver>(session: &mut Session<D>, window: &Window) {
    if let Some(surface) = window.wl_surface() {
        session.fullscreen_textures.remove(surface.as_ref());
    }
}

pub(super) fn set_window_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    fullscreen: bool,
) {
    let Some(surface) = window.x11_surface().cloned() else {
        return;
    };
    if fullscreen {
        enter_fullscreen(session, &surface, FullscreenRequestOrigin::Compositor);
    } else {
        leave_fullscreen(session, &surface, FullscreenRequestOrigin::Compositor);
    }
}

fn maximize_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    if session
        .xwayland
        .pending_windows
        .contains_key(&surface.window_id())
    {
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to suppress initial maximized state: {err}");
        }
        return;
    }
    let maximize_origin_active = maximize_origin_active(surface);
    let fullscreen_active = window_for_surface(session, surface)
        .is_some_and(|window| session.fullscreen.external_desired_matches(&window, true));
    match maximize_toggle_action(maximize_origin_active, fullscreen_active) {
        MaximizeToggleAction::Exit => {
            restore_window(session, surface);
            return;
        }
        MaximizeToggleAction::Ignore => {
            if let Err(err) = surface.set_maximized(false) {
                eventline::warn!(
                    "xwayland: ignored maximize while another fullscreen origin owns the window: {err}"
                );
            }
            return;
        }
        MaximizeToggleAction::Enter => {}
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    let restore_geometry = session
        .wayland
        .space
        .element_geometry(&window)
        .unwrap_or_else(|| surface.geometry());
    remember_normal_size(session, surface, restore_geometry.size);
    let mut maximize = surface
        .user_data()
        .get_or_insert_threadsafe(MaximizeFullscreen::default)
        .0
        .lock()
        .expect("X11 maximize state lock poisoned");
    maximize.active = true;
    maximize.restore = Some(MaximizeRestore {
        geometry: restore_geometry,
        output: crate::wayland::window_output_name(&window),
    });
    drop(maximize);
    if let Err(err) = surface.set_maximized(true) {
        eventline::warn!("xwayland: failed to set maximized state: {err}");
    }
    enter_fullscreen(session, surface, FullscreenRequestOrigin::Maximize);
}

fn restore_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    if session
        .xwayland
        .pending_windows
        .contains_key(&surface.window_id())
    {
        if let Some(state) = surface.user_data().get::<MaximizeFullscreen>() {
            let mut state = state.0.lock().expect("X11 maximize state lock poisoned");
            state.active = false;
            state.restore = None;
        }
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to clear pending maximized state: {err}");
        }
        if let Err(err) = surface.set_fullscreen(false) {
            eventline::warn!("xwayland: failed to clear pending fullscreen state: {err}");
        }
        return;
    }
    if let Err(err) = surface.set_maximized(false) {
        eventline::warn!("xwayland: failed to clear maximized state: {err}");
    }
    let restore = surface
        .user_data()
        .get::<MaximizeFullscreen>()
        .and_then(|state| {
            let mut state = state.0.lock().expect("X11 maximize state lock poisoned");
            let restore = state.active.then(|| state.restore.clone()).flatten();
            state.active = false;
            state.restore = None;
            restore
        });
    if let Some(restore) = restore {
        let window = window_for_surface(session, surface);
        let fullscreen_active = window
            .as_ref()
            .is_some_and(|window| session.fullscreen.external_desired_matches(window, true));
        leave_fullscreen(session, surface, FullscreenRequestOrigin::Maximize);
        if !fullscreen_active && let Some(window) = window {
            if let Some(output) = restore
                .output
                .as_deref()
                .and_then(|name| output_named(session, name))
            {
                crate::wayland::set_window_output(&window, &output);
            }
            session
                .wayland
                .space
                .relocate_element(&window, restore.geometry.loc);
            if let Err(err) = surface.configure(restore.geometry) {
                eventline::warn!("xwayland: failed to restore maximized geometry: {err}");
            }
        }
    }
}

fn maximize_toggle_action(
    maximize_origin_active: bool,
    fullscreen_active: bool,
) -> MaximizeToggleAction {
    if maximize_origin_active {
        MaximizeToggleAction::Exit
    } else if fullscreen_active {
        MaximizeToggleAction::Ignore
    } else {
        MaximizeToggleAction::Enter
    }
}

fn resize_handle(edge: ResizeEdge) -> crate::input::grab::ResizeHandle {
    use crate::input::grab::ResizeHandle;

    match edge {
        ResizeEdge::Top => ResizeHandle::Top,
        ResizeEdge::Bottom => ResizeHandle::Bottom,
        ResizeEdge::Left => ResizeHandle::Left,
        ResizeEdge::TopLeft => ResizeHandle::TopLeft,
        ResizeEdge::BottomLeft => ResizeHandle::BottomLeft,
        ResizeEdge::Right => ResizeHandle::Right,
        ResizeEdge::TopRight => ResizeHandle::TopRight,
        ResizeEdge::BottomRight => ResizeHandle::BottomRight,
    }
}

fn evdev_button(x11_button: u32) -> Option<u32> {
    match x11_button {
        1 => Some(0x110),
        2 => Some(0x112),
        3 => Some(0x111),
        _ => None,
    }
}

pub(super) fn reconfigure_fullscreen(windows: Vec<(Window, Rectangle<i32, Logical>)>) {
    for (window, geometry) in windows {
        let Some(surface) = window.x11_surface() else {
            continue;
        };
        if let Err(err) = surface.configure(geometry) {
            eventline::warn!("xwayland: failed to reconfigure fullscreen output: {err}");
        }
    }
}

pub(super) fn configure_window(window: &Window, geometry: Rectangle<i32, Logical>) {
    let Some(surface) = window.x11_surface() else {
        return;
    };
    if let Err(err) = surface.configure(geometry) {
        eventline::warn!("xwayland: failed to configure window geometry: {err}");
    }
}

fn forget_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    session
        .xwayland
        .pending_windows
        .remove(&surface.window_id());
    session
        .xwayland
        .opening_placements
        .remove(&surface.window_id());
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    if let Some(geometry) = session.wayland.space.element_geometry(&window) {
        remember_normal_size(session, surface, geometry.size);
    }
    crate::session::closing::capture_window(session, &window);
    if let Some(wl_surface) = window.wl_surface().map(|surface| surface.into_owned()) {
        let preparation = crate::session::prepare_window_unmap(session, &wl_surface);
        session.wayland.space.unmap_elem(&window);
        crate::session::finish_window_unmap(session, preparation);
        session.nodes.remove_surface(&wl_surface);
    } else {
        session.wayland.space.unmap_elem(&window);
    }
}

impl<D: SessionDriver> XwmHandler for Session<D> {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwayland
            .xwm
            .as_mut()
            .expect("XWM event delivered without an active XWM")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let xid = window.window_id();
        cancel_pending_override_redirect(self, xid);
        self.xwayland.known_override_redirects.remove(&xid);
        self.xwayland.override_redirect_placements.remove(&xid);
    }

    fn map_window_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if window_for_surface(self, &surface).is_some()
            || self
                .xwayland
                .pending_windows
                .contains_key(&surface.window_id())
        {
            eventline::debug!(
                "xwayland: ignored duplicate map request xid={}",
                surface.window_id()
            );
            return;
        }
        let initial_size = surface.geometry().size;
        if let Err(err) = surface.set_mapped(true) {
            eventline::warn!("xwayland: failed to map window: {err}");
            return;
        }
        let window = Window::new_x11_window(surface.clone());
        self.xwayland.pending_windows.insert(
            surface.window_id(),
            PendingWindow {
                surface: surface.clone(),
                window,
                initial_size,
            },
        );
        eventline::debug!(
            "xwayland: map requested xid={} fullscreen={} maximized={} initial={}x{}",
            surface.window_id(),
            surface.is_fullscreen(),
            surface.is_maximized(),
            initial_size.w,
            initial_size.h
        );
        consider_map(self, surface.window_id(), None);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        let xid = surface.window_id();
        if window_for_surface(self, &surface).is_some()
            || self.xwayland.pending_override_redirects.contains_key(&xid)
        {
            eventline::debug!("xwayland: ignored duplicate override-redirect map xid={xid}");
            return;
        }
        let first_map = self.xwayland.known_override_redirects.insert(xid);
        let configured = self.xwayland.override_redirect_placements.remove(&xid);
        match override_redirect_map_admission(first_map, configured.is_some()) {
            OverrideRedirectMapAdmission::ImmediateFresh => {
                map_override_redirect(self, surface.clone(), surface.geometry(), None, false);
            }
            OverrideRedirectMapAdmission::ImmediateConfigured => {
                let geometry = configured
                    .expect("configured admission requires placement")
                    .geometry;
                map_override_redirect(self, surface, geometry, None, false);
            }
            OverrideRedirectMapAdmission::AwaitConfigure => {
                let geometry = surface.geometry();
                defer_override_redirect_remap(self, surface, geometry);
            }
        }
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        cancel_pending_override_redirect(self, surface.window_id());
        self.xwayland
            .override_redirect_placements
            .remove(&surface.window_id());
        forget_window(self, &surface);
        if !surface.is_override_redirect()
            && let Err(err) = surface.set_mapped(false)
        {
            eventline::warn!("xwayland: failed to acknowledge unmap: {err}");
        }
        refresh_override_redirect_owners(self);
        crate::session::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        cancel_pending_override_redirect(self, surface.window_id());
        self.xwayland
            .known_override_redirects
            .remove(&surface.window_id());
        self.xwayland
            .override_redirect_placements
            .remove(&surface.window_id());
        forget_window(self, &surface);
        refresh_override_redirect_owners(self);
        crate::session::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // ICCCM override-redirect clients configure themselves directly.
        // Smithay reports their resulting ConfigureNotify separately.
        if surface.is_override_redirect() {
            return;
        }
        let mut geometry = surface.geometry();
        geometry.size.w = width
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(geometry.size.w);
        geometry.size.h = height
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(geometry.size.h);
        if let Err(err) = surface.configure(geometry) {
            eventline::warn!("xwayland: configure request failed: {err}");
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<u32>,
    ) {
        if self
            .xwayland
            .pending_windows
            .contains_key(&surface.window_id())
        {
            consider_map(self, surface.window_id(), surface.wl_surface().as_ref());
            return;
        }
        if surface.is_override_redirect() {
            let xid = surface.window_id();
            if cancel_pending_override_redirect(self, xid).is_some() {
                map_override_redirect(self, surface, geometry, above, true);
                return;
            }
            let Some(window) = window_for_surface(self, &surface) else {
                self.xwayland
                    .override_redirect_placements
                    .insert(xid, OverrideRedirectPlacement { geometry });
                return;
            };
            let previous_output = crate::wayland::window_output_name(&window);
            let resolution = override_redirect_resolution(self, &surface, geometry);
            apply_override_redirect_resolution(&window, &resolution);
            let current_output = crate::wayland::window_output_name(&window);
            if previous_output != current_output {
                eventline::debug!(
                    "xwayland: override-redirect xid={} moved output {:?} -> {:?}",
                    surface.window_id(),
                    previous_output,
                    current_output
                );
            }
            self.wayland.space.relocate_element(&window, geometry.loc);
            let action = restack_override_redirect(self, &window, above);
            let output = override_redirect_output_name(&window);
            describe_override_redirect_configure(
                &surface,
                geometry,
                output.as_deref(),
                &resolution,
                above,
                action,
            );
            self.request_redraw();
            return;
        }
        let Some(window) = window_for_surface(self, &surface) else {
            return;
        };
        let now = crate::frame_clock::monotonic_now();
        match self
            .fullscreen
            .settle_external_configure(&mut self.wayland, &window, geometry, now)
        {
            ExternalConfigureResult::NotPending => {
                let opening = window.wl_surface().is_some_and(|wl_surface| {
                    self.window_open_animations
                        .is_animating(wl_surface.as_ref(), now)
                });
                let grabbed = window.wl_surface().is_some_and(|wl_surface| {
                    crate::input::grab::belongs_to_surface(&self.grab, wl_surface.as_ref())
                });
                let fullscreen = window.wl_surface().is_some_and(|wl_surface| {
                    self.fullscreen
                        .is_fullscreen_or_pending(wl_surface.as_ref())
                });
                let placement = if opening && !grabbed && !fullscreen {
                    self.xwayland
                        .opening_placements
                        .get(&surface.window_id())
                        .copied()
                } else {
                    self.xwayland
                        .opening_placements
                        .remove(&surface.window_id());
                    None
                };
                if let Some(placement) = placement {
                    let centered = placement.centered(geometry.size);
                    self.wayland.space.relocate_element(&window, centered.loc);
                    if centered != geometry
                        && let Err(err) = surface.configure(centered)
                    {
                        eventline::warn!(
                            "xwayland: failed to preserve opening window center: {err}"
                        );
                    }
                } else {
                    self.wayland
                        .space
                        .map_element(window.clone(), geometry.loc, false);
                }
                self.fullscreen
                    .update_external_windowed_placement(&self.wayland, &window);
                remember_normal_size(self, &surface, geometry.size);
            }
            ExternalConfigureResult::Waiting => {}
            ExternalConfigureResult::Settled {
                fullscreen,
                animated,
            } => {
                eventline::debug!(
                    "xwayland: fullscreen configure settled xid={} fullscreen={fullscreen} animated={animated}",
                    surface.window_id()
                );
                if !animated {
                    remove_fullscreen_snapshot(self, &window);
                }
                if !fullscreen {
                    remember_normal_size(self, &surface, geometry.size);
                }
            }
        }
        crate::session::reconcile_pointer_constraints(self);
        self.request_redraw();
    }

    fn property_notify(&mut self, _xwm: XwmId, _surface: X11Surface, property: WmWindowProperty) {
        if matches!(
            property,
            WmWindowProperty::Hints
                | WmWindowProperty::TransientFor
                | WmWindowProperty::Class
                | WmWindowProperty::Pid
        ) {
            refresh_override_redirect_owners(self);
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        eventline::debug!(
            "xwayland: maximize request xid={} client_maximized={} client_fullscreen={} \
             maximize_origin={}",
            surface.window_id(),
            surface.is_maximized(),
            surface.is_fullscreen(),
            maximize_origin_active(&surface),
        );
        maximize_window(self, &surface);
        self.request_redraw();
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        eventline::debug!(
            "xwayland: unmaximize request xid={} client_maximized={} client_fullscreen={} \
             maximize_origin={}",
            surface.window_id(),
            surface.is_maximized(),
            surface.is_fullscreen(),
            maximize_origin_active(&surface),
        );
        restore_window(self, &surface);
        self.request_redraw();
    }

    fn minimize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        let changed = window_for_surface(self, &surface)
            .and_then(|window| {
                window
                    .wl_surface()
                    .map(|wl_surface| wl_surface.into_owned())
            })
            .and_then(|wl_surface| self.nodes.id_for_surface(&wl_surface))
            .is_some_and(|id| crate::nodes::collapse(self, id, SERIAL_COUNTER.next_serial()));
        if !changed {
            // Keep EWMH state truthful when a minimize cannot be honored
            // (for example, while another fullscreen transition owns it).
            if let Err(err) = surface.set_hidden(false) {
                eventline::warn!("xwayland: failed to reject minimize request: {err}");
            }
        }
        self.request_redraw();
    }

    fn unminimize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(id) = window_for_surface(self, &surface)
            .and_then(|window| {
                window
                    .wl_surface()
                    .map(|wl_surface| wl_surface.into_owned())
            })
            .and_then(|wl_surface| self.nodes.id_for_surface(&wl_surface))
        {
            let _ = crate::nodes::restore(self, id, SERIAL_COUNTER.next_serial());
        }
        self.request_redraw();
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        enter_fullscreen(self, &surface, FullscreenRequestOrigin::Client);
        self.request_redraw();
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        leave_fullscreen(self, &surface, FullscreenRequestOrigin::Client);
        self.request_redraw();
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        button: u32,
        resize_edge: ResizeEdge,
    ) {
        let Some(button) = evdev_button(button) else {
            return;
        };
        let Some(window) = window_for_surface(self, &surface) else {
            return;
        };
        crate::session::begin_pointer_resize(self, &window, resize_handle(resize_edge), button);
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    fn active_window_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        _timestamp: u32,
        _currently_active_window: Option<X11Surface>,
    ) {
        if surface.is_override_redirect() {
            eventline::debug!(
                "xwayland: ignored activation request for override-redirect xid={}",
                surface.window_id()
            );
            return;
        }
        if let Some(window) = window_for_surface(self, &surface) {
            crate::session::focus_window(self, &window, SERIAL_COUNTER.next_serial());
            self.request_redraw();
        }
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        super::selection::allow_access(self, xwm)
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        super::selection::send(self, selection, mime_type, fd);
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        super::selection::set(self, selection, mime_types);
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        super::selection::clear(self, selection);
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        eventline::warn!("xwayland: window manager disconnected");
        self.xwayland.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExternalPresentationPolicy, FullscreenRequestOrigin, MaximizeToggleAction,
        OverrideRedirectIdentity, OverrideRedirectMapAdmission, OverrideRedirectStackAction,
        external_presentation_policy, maximize_toggle_action, override_redirect_map_admission,
        override_redirect_owner_rank, override_redirect_stack_action, recovery_window_size,
        size_fills_output,
    };
    use smithay::utils::{Logical, Point, Rectangle, Size};

    #[test]
    fn first_override_redirect_map_is_never_delayed() {
        assert_eq!(
            override_redirect_map_admission(true, false),
            OverrideRedirectMapAdmission::ImmediateFresh
        );
    }

    #[test]
    fn reused_override_redirect_waits_for_new_placement() {
        assert_eq!(
            override_redirect_map_admission(false, false),
            OverrideRedirectMapAdmission::AwaitConfigure
        );
    }

    #[test]
    fn preconfigured_override_redirect_remap_is_immediate() {
        assert_eq!(
            override_redirect_map_admission(false, true),
            OverrideRedirectMapAdmission::ImmediateConfigured
        );
    }

    #[test]
    fn override_redirect_stacks_immediately_above_a_mapped_sibling() {
        assert_eq!(
            override_redirect_stack_action(Some(41), true),
            OverrideRedirectStackAction::Above(41)
        );
    }

    #[test]
    fn override_redirect_without_an_above_sibling_is_at_the_bottom() {
        assert_eq!(
            override_redirect_stack_action(None, false),
            OverrideRedirectStackAction::Bottom
        );
    }

    #[test]
    fn override_redirect_preserves_order_when_the_x_sibling_is_unmapped() {
        assert_eq!(
            override_redirect_stack_action(Some(41), false),
            OverrideRedirectStackAction::PreserveMissing(41)
        );
    }

    #[test]
    fn steam_group_owner_prefers_matching_window_containing_popup() {
        let center = Point::<i32, Logical>::from((1230, 540));
        let owner = override_redirect_owner_rank(
            OverrideRedirectIdentity {
                pid: Some(77),
                class: "steam",
                instance: "steamwebhelper",
            },
            OverrideRedirectIdentity {
                pid: Some(77),
                class: "steam",
                instance: "steamwebhelper",
            },
            center,
            Rectangle::new((1090, 425).into(), (1920, 1200).into()),
            2,
        );
        let other_group_window = override_redirect_owner_rank(
            OverrideRedirectIdentity {
                pid: Some(77),
                class: "steam",
                instance: "steamwebhelper",
            },
            OverrideRedirectIdentity {
                pid: Some(77),
                class: "steam",
                instance: "steamwebhelper",
            },
            center,
            Rectangle::new((3300, 200).into(), (800, 600).into()),
            3,
        );
        let unrelated_overlap = override_redirect_owner_rank(
            OverrideRedirectIdentity {
                pid: Some(77),
                class: "steam",
                instance: "steamwebhelper",
            },
            OverrideRedirectIdentity {
                pid: Some(88),
                class: "other",
                instance: "other",
            },
            center,
            Rectangle::new((1100, 450).into(), (500, 500).into()),
            4,
        );

        assert!(owner > other_group_window);
        assert!(owner > unrelated_overlap);
    }

    #[test]
    fn steam_cache_miss_uses_bounded_three_quarter_recovery() {
        assert_eq!(
            recovery_window_size(
                Size::from((1920, 1200)),
                Some(Size::from((1010, 600))),
                None,
            ),
            Size::from((1440, 900))
        );
        assert_eq!(
            recovery_window_size(
                Size::from((1920, 1200)),
                Some(Size::from((1800, 1000))),
                Some(Size::from((1850, 1100))),
            ),
            Size::from((1800, 1000))
        );
        assert_eq!(
            recovery_window_size(
                Size::from((1920, 1200)),
                None,
                Some(Size::from((1200, 700))),
            ),
            Size::from((1200, 700))
        );
    }

    #[test]
    fn output_sized_steam_geometry_is_not_a_normal_restore_size() {
        let output = Size::from((1920, 1200));
        assert!(size_fills_output(Size::from((1920, 1200)), output));
        assert!(size_fills_output(Size::from((2560, 1440)), output));
        assert!(!size_fills_output(Size::from((1440, 900)), output));
    }

    #[test]
    fn initial_fullscreen_uses_the_existing_opening_when_available() {
        assert_eq!(
            external_presentation_policy(FullscreenRequestOrigin::Initial, false, false),
            ExternalPresentationPolicy::Initial
        );
        assert_eq!(
            external_presentation_policy(FullscreenRequestOrigin::Initial, true, false),
            ExternalPresentationPolicy::Opening
        );
    }

    #[test]
    fn opening_fullscreen_retargets_the_existing_opening() {
        for origin in [
            FullscreenRequestOrigin::Client,
            FullscreenRequestOrigin::Compositor,
        ] {
            assert_eq!(
                external_presentation_policy(origin, true, false),
                ExternalPresentationPolicy::Opening
            );
        }
    }

    #[test]
    fn confined_fullscreen_never_animates() {
        for origin in [
            FullscreenRequestOrigin::Initial,
            FullscreenRequestOrigin::Client,
            FullscreenRequestOrigin::Compositor,
        ] {
            assert_eq!(
                external_presentation_policy(origin, true, true),
                ExternalPresentationPolicy::Confined
            );
        }
    }

    #[test]
    fn settled_or_locked_fullscreen_animates_when_not_confined() {
        for origin in [
            FullscreenRequestOrigin::Client,
            FullscreenRequestOrigin::Compositor,
        ] {
            assert_eq!(
                external_presentation_policy(origin, false, false),
                ExternalPresentationPolicy::Animated
            );
        }
    }

    #[test]
    fn maximize_button_toggles_without_hijacking_other_fullscreen_origins() {
        assert_eq!(
            maximize_toggle_action(false, false),
            MaximizeToggleAction::Enter
        );
        assert_eq!(
            maximize_toggle_action(true, true),
            MaximizeToggleAction::Exit
        );
        assert_eq!(
            maximize_toggle_action(false, true),
            MaximizeToggleAction::Ignore
        );
    }
}
