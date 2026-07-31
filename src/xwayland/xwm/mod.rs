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
mod lifecycle;
mod override_redirect;
mod presentation;

use override_redirect::*;
pub(super) use presentation::restore_maximized_window;
use presentation::*;
pub(super) use presentation::{configure_window, reconfigure_fullscreen, set_window_fullscreen};

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

impl FullscreenRequestOrigin {
    fn presentation_origin(self) -> crate::wayland::fullscreen::FullscreenOrigin {
        match self {
            Self::Compositor => crate::wayland::fullscreen::FullscreenOrigin::Compositor,
            Self::Maximize => crate::wayland::fullscreen::FullscreenOrigin::Maximize,
            Self::Initial | Self::Client => crate::wayland::fullscreen::FullscreenOrigin::Client,
        }
    }
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
    let rule = session.window_rules.track_window(&window);
    if saved_maximize.is_none()
        && let Some((width, height)) = rule.initial_size
    {
        opening_size = Size::from((
            i32::try_from(width).unwrap_or(i32::MAX).max(96),
            i32::try_from(height).unwrap_or(i32::MAX).max(72),
        ));
    }
    let initial_placement = crate::window::routing::initial_window_placement(
        crate::window::routing::InitialWindowPlacementRequest {
            wayland: &session.wayland,
            cameras: &session.cameras,
            primary_output: session.driver.primary_output(),
            window: Some(&window),
            window_size: opening_size,
            placement: rule.spawn_placement,
            cursor_position: smithay::utils::Point::from(session.pointer.position()),
            gap: session.field_config.gap,
        },
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
                crate::window::routing::InitialWindowPlacementRequest {
                    wayland: &session.wayland,
                    cameras: &session.cameras,
                    primary_output: session.driver.primary_output(),
                    window: Some(&window),
                    window_size: opening_size,
                    placement: halley_config::WindowSpawnPlacement::Default,
                    cursor_position: smithay::utils::Point::from(session.pointer.position()),
                    gap: session.field_config.gap,
                },
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
                crate::window::routing::InitialWindowPlacementRequest {
                    wayland: &session.wayland,
                    cameras: &session.cameras,
                    primary_output: session.driver.primary_output(),
                    window: Some(&window),
                    window_size: opening_size,
                    placement: rule.spawn_placement,
                    cursor_position: smithay::utils::Point::from(session.pointer.position()),
                    gap: session.field_config.gap,
                },
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
        if let Some(id) = session.nodes.id_for_surface(wl_surface.as_ref()) {
            if session.clusters.admit_mapped_window(
                &mut session.nodes.field,
                &output.name(),
                id,
                rule.cluster_participation,
                smithay::desktop::layer_map_for_output(&output).non_exclusive_zone(),
                crate::frame_clock::monotonic_now(),
            ) {
                session.request_redraw();
            }
            crate::nodes::displace_landmarks_for_new_window(session, id);
        }
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
    let stack_managed = window
        .wl_surface()
        .and_then(|surface| session.nodes.id_for_surface(surface.as_ref()))
        .and_then(|id| session.clusters.active_layout_for_member(id))
        == Some(halley_core::cluster::layout::ClusterWorkspaceLayoutKind::Stacking);
    let started = !stack_managed
        && window.wl_surface().is_some_and(|wl_surface| {
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

#[cfg(test)]
mod tests {
    use super::{
        ExternalPresentationPolicy, FullscreenRequestOrigin, OverrideRedirectIdentity,
        OverrideRedirectMapAdmission, OverrideRedirectStackAction,
        compositor_fullscreen_should_raise, external_presentation_policy,
        override_redirect_map_admission, override_redirect_owner_rank,
        override_redirect_stack_action, recovery_window_size, size_fills_output,
    };
    use smithay::utils::{Logical, Point, Rectangle, Size};

    #[test]
    fn external_fullscreen_origin_preserves_compositor_ownership() {
        use crate::wayland::fullscreen::FullscreenOrigin;

        assert_eq!(
            FullscreenRequestOrigin::Compositor.presentation_origin(),
            FullscreenOrigin::Compositor
        );
        assert_eq!(
            FullscreenRequestOrigin::Maximize.presentation_origin(),
            FullscreenOrigin::Maximize
        );
        for origin in [
            FullscreenRequestOrigin::Initial,
            FullscreenRequestOrigin::Client,
        ] {
            assert_eq!(origin.presentation_origin(), FullscreenOrigin::Client);
        }
    }

    #[test]
    fn only_compositor_fullscreen_entry_promotes_the_x11_window() {
        assert!(compositor_fullscreen_should_raise(
            true,
            FullscreenRequestOrigin::Compositor
        ));
        assert!(!compositor_fullscreen_should_raise(
            false,
            FullscreenRequestOrigin::Compositor
        ));
        for origin in [
            FullscreenRequestOrigin::Initial,
            FullscreenRequestOrigin::Client,
            FullscreenRequestOrigin::Maximize,
        ] {
            assert!(!compositor_fullscreen_should_raise(true, origin));
        }
    }

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
}
