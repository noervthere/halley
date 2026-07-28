use std::os::fd::OwnedFd;
use std::sync::Mutex;

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, X11SurfaceError, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::session::{Session, SessionDriver};
use crate::window::lifecycle::{Placement, WindowKey, WindowKind, WindowLifecycle};

#[derive(Default)]
struct RestoreGeometry(Mutex<Option<Rectangle<i32, Logical>>>);

fn window_for_surface<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
) -> Option<Window> {
    session
        .wayland
        .windows
        .window(&WindowLifecycle::x11_key(surface))
        .cloned()
}

fn authoritative_fullscreen_geometry<D: SessionDriver>(
    session: &Session<D>,
    surface: &X11Surface,
) -> Option<Rectangle<i32, Logical>> {
    let window = window_for_surface(session, surface)?;
    if let Some(wl_surface) = window.wl_surface()
        && let Some(geometry) = session
            .fullscreen
            .external_geometry(&session.wayland, wl_surface.as_ref())
    {
        return Some(geometry);
    }
    if !surface.is_fullscreen() {
        return None;
    }
    crate::wayland::window_output_name(&window)
        .and_then(|name| {
            session
                .wayland
                .space
                .outputs()
                .find(|output| output.name() == name)
        })
        .or_else(|| crate::wayland::focus::selected_output(&session.wayland))
        .and_then(|output| session.wayland.space.output_geometry(output))
}

pub(super) fn handle_surface_commit<D: SessionDriver>(
    session: &mut Session<D>,
    wl_surface: &WlSurface,
) {
    let Some(key @ WindowKey::X11(_)) = session.wayland.windows.key_for_wl_surface(wl_surface)
    else {
        return;
    };
    if !session.wayland.windows.is_mapped(&key) {
        return;
    }
    let Some(window) = session.wayland.windows.window(&key).cloned() else {
        return;
    };
    let now = crate::frame_clock::monotonic_now();
    if session.fullscreen.is_transitioning(wl_surface, now)
        && !session.fullscreen_textures.contains(wl_surface)
        && with_renderer_surface_state(wl_surface, |state| state.buffer().is_some())
            .unwrap_or(false)
    {
        let textures = &mut session.fullscreen_textures;
        if let Err(err) = session
            .driver
            .with_renderer(|renderer| textures.capture_previous(renderer, &window))
        {
            eventline::warn!("fullscreen: failed to capture XWayland texture on commit: {err}");
        }
    }

    let committed_size =
        with_renderer_surface_state(wl_surface, |state| state.surface_size()).flatten();
    let Some(committed_size) = committed_size else {
        return;
    };
    let Some(update) =
        session
            .fullscreen
            .commit_external(&session.wayland, &window, committed_size, now)
    else {
        return;
    };
    apply_external_fullscreen_presentation(session, &window, &update);
    eventline::debug!(
        "xwayland: presented fullscreen={} for window={key:?} at {}x{}",
        update.fullscreen,
        update.geometry.size.w,
        update.geometry.size.h
    );
}

pub(super) fn surface_associated<D: SessionDriver>(
    session: &mut Session<D>,
    wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    surface: X11Surface,
) {
    eventline::debug!(
        "window: X11 0x{:x} associated with {}",
        surface.window_id(),
        wl_surface.id()
    );
    if let Some(previous) = session
        .wayland
        .windows
        .associate_x11(&surface, wl_surface.clone())
        && previous != wl_surface
    {
        session
            .fullscreen
            .reassociate_external(&previous, wl_surface);
        session.fullscreen_textures.remove(&previous);
        session.window_open_animations.remove(&previous);
    }
    finalize_mapped_window(session, &surface);
    session.request_redraw();
}

fn finalize_mapped_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    let key = WindowLifecycle::x11_key(surface);
    let Some(wl_surface) = session
        .wayland
        .windows
        .window(&key)
        .and_then(|window| window.wl_surface())
        .map(|surface| surface.into_owned())
    else {
        return;
    };
    let Some(transition) = session.wayland.windows.finalize_map(&key) else {
        return;
    };
    eventline::debug!(
        "window: X11 0x{:x} mapped generation={} first={}",
        surface.window_id(),
        transition.generation,
        transition.first_map
    );

    if surface.is_fullscreen() || session.fullscreen.is_fullscreen_or_pending(&wl_surface) {
        update_fullscreen(
            session,
            surface,
            true,
            FullscreenApplication::MapFinalization,
        );
    } else if surface.is_maximized() {
        maximize_window(session, surface);
    } else if let Some(geometry) = session.wayland.space.element_bbox(&transition.window)
        && let Err(err) = surface.configure(geometry)
    {
        eventline::warn!("xwayland: failed to configure mapped window: {err}");
    }

    if transition.first_map && !surface.is_fullscreen() {
        session
            .window_open_animations
            .start(wl_surface, crate::frame_clock::monotonic_now());
    }
    if transition.kind.is_managed() {
        crate::session::focus_window(session, &transition.window, SERIAL_COUNTER.next_serial());
    }
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

fn enter_fullscreen<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    update_fullscreen(session, surface, true, FullscreenApplication::StateRequest);
}

fn leave_fullscreen<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    update_fullscreen(session, surface, false, FullscreenApplication::StateRequest);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenApplication {
    StateRequest,
    MapFinalization,
}

fn should_apply_fullscreen_update(changed: bool, application: FullscreenApplication) -> bool {
    changed || application == FullscreenApplication::MapFinalization
}

fn authoritative_configure(
    surface: &X11Surface,
    geometry: Rectangle<i32, Logical>,
) -> Result<(), X11SurfaceError> {
    surface.configure(authoritative_configure_geometry(
        surface.geometry(),
        geometry,
    ))
}

fn authoritative_configure_geometry(
    current: Rectangle<i32, Logical>,
    authoritative: Rectangle<i32, Logical>,
) -> Option<Rectangle<i32, Logical>> {
    (current != authoritative).then_some(authoritative)
}

fn update_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    fullscreen: bool,
    application: FullscreenApplication,
) {
    let mapped = session
        .wayland
        .windows
        .is_mapped(&WindowLifecycle::x11_key(surface));
    let window = mapped
        .then(|| window_for_surface(session, surface))
        .flatten();

    if let Err(err) = surface.set_fullscreen(fullscreen) {
        let action = if fullscreen { "set" } else { "clear" };
        eventline::warn!("xwayland: failed to {action} fullscreen state: {err}");
    }
    let Some(window) = window else {
        return;
    };

    let geometry = if fullscreen {
        session
            .fullscreen
            .request_external(&session.wayland, &window)
    } else {
        session
            .fullscreen
            .unrequest_external(&session.wayland, &window)
    };
    let Some(update) = geometry else {
        return;
    };
    if update.changed && update.pending {
        let _ = capture_fullscreen_snapshot(session, &window, fullscreen);
    } else if !update.pending
        && let Some(wl_surface) = window.wl_surface().map(|surface| surface.into_owned())
    {
        session.fullscreen_textures.remove(&wl_surface);
    }
    apply_external_fullscreen_update(session, surface, &window, update, application);
}

fn apply_external_fullscreen_update<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
    update: crate::wayland::fullscreen::ExternalFullscreenUpdate,
    application: FullscreenApplication,
) {
    if !should_apply_fullscreen_update(update.changed, application) {
        eventline::debug!(
            "xwayland: ignored duplicate fullscreen state for window=0x{:x}",
            surface.window_id()
        );
        return;
    }
    if application == FullscreenApplication::MapFinalization {
        apply_external_fullscreen_presentation(session, window, &update);
    }
    if let Err(err) = authoritative_configure(surface, update.geometry) {
        eventline::warn!("xwayland: failed to configure fullscreen transition: {err}");
    }
}

fn apply_external_fullscreen_presentation<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    update: &crate::wayland::fullscreen::ExternalFullscreenUpdate,
) {
    if let Some(output) = update.output.as_ref() {
        crate::wayland::set_window_output(window, output);
    }
    session
        .wayland
        .space
        .map_element(window.clone(), update.location, true);
}

fn capture_fullscreen_snapshot<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    fullscreen: bool,
) -> Option<WlSurface> {
    let surface = window.wl_surface().map(|surface| surface.into_owned())?;
    if !session
        .fullscreen
        .should_capture_external_snapshot(&surface, fullscreen)
    {
        return None;
    }
    let ready =
        with_renderer_surface_state(&surface, |state| state.buffer().is_some()).unwrap_or(false);
    if !ready {
        return None;
    }

    let textures = &mut session.fullscreen_textures;
    match session
        .driver
        .with_renderer(|renderer| textures.capture_previous(renderer, window))
    {
        Ok(()) => Some(surface),
        Err(err) => {
            eventline::warn!("fullscreen: failed to capture previous XWayland texture: {err}");
            None
        }
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
        enter_fullscreen(session, &surface);
    } else {
        leave_fullscreen(session, &surface);
    }
}

fn maximize_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    if let Err(err) = surface.set_maximized(true) {
        eventline::warn!("xwayland: failed to set maximized state: {err}");
    }
    if !session
        .wayland
        .windows
        .is_mapped(&WindowLifecycle::x11_key(surface))
    {
        return;
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    let Some(output) = crate::wayland::window_output_name(&window)
        .and_then(|name| {
            session
                .wayland
                .space
                .outputs()
                .find(|output| output.name() == name)
        })
        .or_else(|| crate::wayland::focus::selected_output(&session.wayland))
        .cloned()
    else {
        return;
    };
    let Some(geometry) = session.wayland.space.output_geometry(&output) else {
        return;
    };
    let restore = surface
        .user_data()
        .get_or_insert_threadsafe(RestoreGeometry::default);
    let mut restore = restore
        .0
        .lock()
        .expect("X11 restore geometry lock poisoned");
    if restore.is_none() {
        *restore = session.wayland.space.element_bbox(&window);
    }
    drop(restore);
    if let Err(err) = surface.configure(geometry) {
        eventline::warn!("xwayland: failed to maximize window: {err}");
    }
    session
        .wayland
        .space
        .map_element(window, geometry.loc, true);
}

fn restore_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    if let Err(err) = surface.set_maximized(false) {
        eventline::warn!("xwayland: failed to clear maximized state: {err}");
    }
    if !session
        .wayland
        .windows
        .is_mapped(&WindowLifecycle::x11_key(surface))
    {
        return;
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    let restore = surface
        .user_data()
        .get::<RestoreGeometry>()
        .and_then(|restore| {
            restore
                .0
                .lock()
                .expect("X11 restore geometry lock poisoned")
                .take()
        });
    if let Some(geometry) = restore {
        if let Err(err) = surface.configure(geometry) {
            eventline::warn!("xwayland: failed to restore window: {err}");
        }
        session
            .wayland
            .space
            .map_element(window, geometry.loc, true);
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

fn current_placement<D: SessionDriver>(session: &Session<D>, window: &Window) -> Option<Placement> {
    Some(Placement {
        location: session.wayland.space.element_location(window)?,
        output: crate::wayland::window_output_name(window),
    })
}

fn unmap_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    let key = WindowLifecycle::x11_key(surface);
    let placement = session.wayland.windows.window(&key).and_then(|window| {
        let fullscreen = window.wl_surface().is_some_and(|wl_surface| {
            session
                .fullscreen
                .is_fullscreen_or_pending(wl_surface.as_ref())
        });
        (!fullscreen)
            .then(|| current_placement(session, window))
            .flatten()
    });
    let Some(transition) = session.wayland.windows.unmap(&key, placement) else {
        return;
    };
    eventline::debug!("window: X11 0x{:x} unmapped", surface.window_id());
    crate::session::retire_window_mapping(
        session,
        transition,
        crate::session::WindowRetirement::Unmapped,
    );
}

fn destroy_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    let key = WindowLifecycle::x11_key(surface);
    if let Some(transition) = session.wayland.windows.destroy(&key) {
        crate::session::retire_window_mapping(
            session,
            transition,
            crate::session::WindowRetirement::Destroyed,
        );
    }
}

impl<D: SessionDriver> XwmHandler for Session<D> {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwayland
            .xwm
            .as_mut()
            .expect("XWM event delivered without an active XWM")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.wayland.windows.register_x11(window, WindowKind::X11);
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.wayland
            .windows
            .register_x11(window, WindowKind::X11OverrideRedirect);
    }

    fn map_window_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Err(err) = surface.set_mapped(true) {
            eventline::warn!("xwayland: failed to map window: {err}");
            return;
        }
        let key = self.wayland.windows.ensure_x11(&surface, WindowKind::X11);
        let Some(transition) = self.wayland.windows.begin_map(&key) else {
            return;
        };
        eventline::debug!(
            "window: X11 0x{:x} map requested generation={} first={}",
            surface.window_id(),
            transition.generation,
            transition.first_map
        );
        crate::window::place_mapping(&mut self.wayland, &self.cameras, &transition);
        finalize_mapped_window(self, &surface);
        self.request_redraw();
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        let geometry = surface.geometry();
        let key = self
            .wayland
            .windows
            .ensure_x11(&surface, WindowKind::X11OverrideRedirect);
        let Some(transition) = self.wayland.windows.begin_map(&key) else {
            return;
        };
        let window = transition.window;
        if let Some(output) = output_for_geometry(self, geometry) {
            crate::wayland::set_window_output(&window, &output);
        }
        self.wayland.space.map_element(window, geometry.loc, true);
        self.wayland.windows.finalize_map(&key);
        self.request_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        unmap_window(self, &surface);
        if !surface.is_override_redirect()
            && let Err(err) = surface.set_mapped(false)
        {
            eventline::warn!("xwayland: failed to acknowledge unmap: {err}");
        }
        self.request_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        destroy_window(self, &surface);
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
        if let Some(geometry) = authoritative_fullscreen_geometry(self, &surface) {
            // A fullscreen window has one geometry owner. Accepting transient
            // game resize requests here creates a windowed/fullscreen configure
            // loop and can move XWayland's relative-pointer anchor.
            eventline::debug!(
                "xwayland: kept fullscreen geometry for window=0x{:x} requested={:?}x{:?} target={}x{}",
                surface.window_id(),
                width,
                height,
                geometry.size.w,
                geometry.size.h
            );
            if let Err(err) = authoritative_configure(&surface, geometry) {
                eventline::warn!("xwayland: fullscreen configure acknowledgement failed: {err}");
            }
            return;
        }

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
        let key = WindowLifecycle::x11_key(&surface);
        if !self.wayland.windows.is_mapped(&key) {
            return;
        }
        let Some(window) = window_for_surface(self, &surface) else {
            return;
        };
        if window
            .wl_surface()
            .is_some_and(|wl_surface| self.fullscreen.external_is_pending(wl_surface.as_ref()))
        {
            eventline::debug!(
                "xwayland: held scene placement for pending fullscreen window=0x{:x}",
                surface.window_id()
            );
            return;
        }
        let fullscreen_geometry = authoritative_fullscreen_geometry(self, &surface);
        let location = fullscreen_geometry
            .map(|geometry| geometry.loc)
            .unwrap_or(geometry.loc);
        self.wayland
            .space
            .map_element(window.clone(), location, false);
        if fullscreen_geometry.is_none() {
            self.wayland.windows.update_placement(
                &key,
                Placement {
                    location,
                    output: crate::wayland::window_output_name(&window),
                },
            );
        }
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
        enter_fullscreen(self, &surface);
        self.request_redraw();
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        leave_fullscreen(self, &surface);
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
        for transition in self.wayland.windows.clear_x11() {
            crate::session::retire_window_mapping(
                self,
                transition,
                crate::session::WindowRetirement::Destroyed,
            );
        }
        self.xwayland.clear();
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_state_request_does_not_reapply_fullscreen() {
        assert!(!should_apply_fullscreen_update(
            false,
            FullscreenApplication::StateRequest
        ));
        assert!(should_apply_fullscreen_update(
            true,
            FullscreenApplication::StateRequest
        ));
    }

    #[test]
    fn map_finalization_reconciles_existing_fullscreen_state() {
        assert!(should_apply_fullscreen_update(
            false,
            FullscreenApplication::MapFinalization
        ));
    }

    #[test]
    fn authoritative_geometry_rejects_only_real_divergence() {
        let fullscreen = Rectangle::new((2560, 0).into(), (1920, 1200).into());
        let windowed = Rectangle::new((3200, 360).into(), (640, 480).into());

        assert_eq!(
            authoritative_configure_geometry(fullscreen, fullscreen),
            None
        );
        assert_eq!(
            authoritative_configure_geometry(windowed, fullscreen),
            Some(fullscreen)
        );
    }
}
