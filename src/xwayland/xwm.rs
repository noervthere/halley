use std::os::fd::OwnedFd;
use std::sync::Mutex;

use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::session::{Session, SessionDriver};
use crate::window::lifecycle::{Placement, WindowKind, WindowLifecycle};

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

pub(super) fn surface_associated<D: SessionDriver>(
    session: &mut Session<D>,
    _wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    surface: X11Surface,
) {
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

    if surface.is_fullscreen() {
        enter_fullscreen(session, surface);
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
    if let Err(err) = surface.set_fullscreen(true) {
        eventline::warn!("xwayland: failed to set fullscreen state: {err}");
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    if let Some(geometry) = session
        .fullscreen
        .request_external(&mut session.wayland, &window)
        && let Err(err) = surface.configure(geometry)
    {
        eventline::warn!("xwayland: failed to configure fullscreen window: {err}");
    }
}

fn leave_fullscreen<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    if let Err(err) = surface.set_fullscreen(false) {
        eventline::warn!("xwayland: failed to clear fullscreen state: {err}");
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    if let Some(geometry) = session
        .fullscreen
        .unrequest_external(&mut session.wayland, &window)
        && let Err(err) = surface.configure(geometry)
    {
        eventline::warn!("xwayland: failed to restore fullscreen window: {err}");
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
    if let Err(err) = surface.set_maximized(true) {
        eventline::warn!("xwayland: failed to set maximized state: {err}");
    }
    if let Err(err) = surface.configure(geometry) {
        eventline::warn!("xwayland: failed to maximize window: {err}");
    }
    session
        .wayland
        .space
        .map_element(window, geometry.loc, true);
}

fn restore_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    if let Err(err) = surface.set_maximized(false) {
        eventline::warn!("xwayland: failed to clear maximized state: {err}");
    }
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
    let placement = session
        .wayland
        .windows
        .window(&key)
        .and_then(|window| current_placement(session, window));
    let Some(transition) = session.wayland.windows.unmap(&key, placement) else {
        return;
    };
    if let Some(wl_surface) = transition
        .window
        .wl_surface()
        .map(|surface| surface.into_owned())
    {
        session.fullscreen.remove(&wl_surface);
        session.fullscreen_textures.remove(&wl_surface);
        session.window_open_animations.remove(&wl_surface);
        crate::input::grab::forget_resize_anchor(&mut session.resize_anchor, &wl_surface);
        if session.wayland.focused_window.as_ref() == Some(&wl_surface) {
            session.wayland.focused_window = None;
        }
    }
    session.wayland.space.unmap_elem(&transition.window);
}

fn destroy_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    unmap_window(session, surface);
    let key = WindowLifecycle::x11_key(surface);
    session.wayland.windows.destroy(&key);
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
        let window = transition.window;
        let output = transition
            .placement
            .as_ref()
            .and_then(|placement| placement.output.as_deref())
            .and_then(|name| {
                self.wayland
                    .space
                    .outputs()
                    .find(|output| output.name() == name)
            })
            .cloned()
            .or_else(|| crate::wayland::focus::selected_output(&self.wayland).cloned());
        let location = transition
            .placement
            .map(|placement| placement.location)
            .or_else(|| {
                output.as_ref().map(|output| {
                    crate::wayland::xdg_shell::centered_location(
                        &self.wayland,
                        &self.cameras,
                        output,
                        &window,
                    )
                })
            })
            .unwrap_or_else(|| (0, 0).into());
        if let Some(output) = output.as_ref() {
            crate::wayland::set_window_output(&window, output);
        }
        self.wayland
            .space
            .map_element(window.clone(), location, true);
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
        crate::session::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        destroy_window(self, &surface);
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
        let Some(window) = window_for_surface(self, &surface) else {
            return;
        };
        self.wayland
            .space
            .map_element(window.clone(), geometry.loc, false);
        let key = WindowLifecycle::x11_key(&surface);
        self.wayland.windows.update_placement(
            &key,
            Placement {
                location: geometry.loc,
                output: crate::wayland::window_output_name(&window),
            },
        );
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
        self.xwayland.clear();
    }
}
