use std::os::fd::OwnedFd;
use std::sync::Mutex;

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::session::{Session, SessionDriver};
use crate::wayland::fullscreen::{ExternalConfigureResult, ExternalTransactionRequest};

use super::PendingWindow;
use super::lifecycle::{MapAdmission, OpeningPlacement, map_admission};

#[derive(Default)]
struct MaximizeFullscreen(Mutex<bool>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenRequestOrigin {
    Initial,
    Client,
    Compositor,
    Maximize,
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
    let opening_size = OpeningPlacement::preferred_size(initial_size, window.geometry().size);
    let placement = crate::window::routing::initial_window_placement(
        &session.wayland,
        &session.cameras,
        session.driver.primary_output(),
        opening_size,
    );
    let output = placement.output;
    let location = placement.location;
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
    session
        .xwayland
        .opening_placements
        .insert(xid, OpeningPlacement::new(opening_geometry, initial_size));
    let started = window.wl_surface().is_some_and(|wl_surface| {
        crate::session::opening::start(
            session,
            wl_surface.into_owned(),
            &output,
            crate::frame_clock::monotonic_now(),
        )
    });
    if surface.is_fullscreen() {
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
    if fullscreen && origin != FullscreenRequestOrigin::Maximize {
        *surface
            .user_data()
            .get_or_insert_threadsafe(MaximizeFullscreen::default)
            .0
            .lock()
            .expect("X11 maximize origin lock poisoned") = false;
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to clear maximized state for fullscreen: {err}");
        }
    }
    if session
        .xwayland
        .pending_windows
        .contains_key(&surface.window_id())
    {
        if let Err(err) = surface.set_fullscreen(fullscreen) {
            eventline::warn!("xwayland: failed to update pending fullscreen state: {err}");
        }
        return;
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    if let Err(err) = surface.set_fullscreen(fullscreen) {
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
        &session.fullscreen,
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
    *surface
        .user_data()
        .get_or_insert_threadsafe(MaximizeFullscreen::default)
        .0
        .lock()
        .expect("X11 maximize origin lock poisoned") = true;
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
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to clear pending maximized state: {err}");
        }
        return;
    }
    if let Err(err) = surface.set_maximized(false) {
        eventline::warn!("xwayland: failed to clear maximized state: {err}");
    }
    let was_maximize_fullscreen =
        surface
            .user_data()
            .get::<MaximizeFullscreen>()
            .is_some_and(|origin| {
                let mut origin = origin.0.lock().expect("X11 maximize origin lock poisoned");
                let was_set = *origin;
                *origin = false;
                was_set
            });
    if was_maximize_fullscreen {
        leave_fullscreen(session, surface, FullscreenRequestOrigin::Maximize);
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

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

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
        let geometry = surface.geometry();
        let window = Window::new_x11_window(surface);
        if let Some(output) = output_for_geometry(self, geometry) {
            crate::wayland::set_window_output(&window, &output);
        }
        self.wayland.space.map_element(window, geometry.loc, true);
        self.request_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        forget_window(self, &surface);
        if !surface.is_override_redirect()
            && let Err(err) = surface.set_mapped(false)
        {
            eventline::warn!("xwayland: failed to acknowledge unmap: {err}");
        }
        crate::session::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        forget_window(self, &surface);
        crate::session::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geometry = surface.geometry();
        if surface.is_override_redirect() {
            geometry.loc.x = x.unwrap_or(geometry.loc.x);
            geometry.loc.y = y.unwrap_or(geometry.loc.y);
        }
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
        _above: Option<u32>,
    ) {
        if self
            .xwayland
            .pending_windows
            .contains_key(&surface.window_id())
        {
            consider_map(self, surface.window_id(), surface.wl_surface().as_ref());
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
            }
        }
        crate::session::reconcile_pointer_constraints(self);
        self.request_redraw();
    }

    fn maximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        maximize_window(self, &surface);
        self.request_redraw();
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        restore_window(self, &surface);
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
        ExternalPresentationPolicy, FullscreenRequestOrigin, external_presentation_policy,
    };

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
