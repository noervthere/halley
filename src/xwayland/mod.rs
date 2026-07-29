mod focus;
mod lifecycle;
mod selection;
mod xwm;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::process::Stdio;

use calloop::LoopHandle;
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xwayland::keyboard_grab::zv1::server::{
    zwp_xwayland_keyboard_grab_manager_v1::ZwpXwaylandKeyboardGrabManagerV1,
    zwp_xwayland_keyboard_grab_v1::ZwpXwaylandKeyboardGrabV1,
};
use smithay::reexports::wayland_protocols::xwayland::shell::v1::server::{
    xwayland_shell_v1::XwaylandShellV1, xwayland_surface_v1::XwaylandSurfaceV1,
};
use smithay::reexports::wayland_server::{Dispatch, DisplayHandle, GlobalDispatch};
use smithay::wayland::compositor::CompositorClientState;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::xwayland_keyboard_grab::{
    XWaylandKeyboardGrabHandler, XWaylandKeyboardGrabState,
};
use smithay::wayland::xwayland_shell::{
    XWaylandShellHandler, XWaylandShellState, XWaylandSurfaceUserData,
};
use smithay::xwayland::{X11Wm, XWayland, XWaylandClientData, XWaylandEvent};
use smithay::{delegate_xwayland_keyboard_grab, delegate_xwayland_shell};

use crate::session::{Session, SessionDriver};

pub use focus::KeyboardFocusTarget;

struct PendingWindow {
    surface: smithay::xwayland::X11Surface,
    window: Window,
    initial_size: smithay::utils::Size<i32, smithay::utils::Logical>,
}

pub struct State {
    shell_state: XWaylandShellState,
    _keyboard_grab_state: XWaylandKeyboardGrabState,
    xwm: Option<X11Wm>,
    display: Option<u32>,
    pending_windows: HashMap<u32, PendingWindow>,
    opening_placements: HashMap<u32, lifecycle::OpeningPlacement>,
}

impl State {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: XWaylandShellHandler + XWaylandKeyboardGrabHandler + 'static,
        D: GlobalDispatch<XwaylandShellV1, ()>,
        D: Dispatch<XwaylandShellV1, ()>,
        D: Dispatch<XwaylandSurfaceV1, XWaylandSurfaceUserData>,
        D: GlobalDispatch<ZwpXwaylandKeyboardGrabManagerV1, ()>,
        D: Dispatch<ZwpXwaylandKeyboardGrabManagerV1, ()>,
        D: Dispatch<ZwpXwaylandKeyboardGrabV1, ()>,
    {
        Self {
            shell_state: XWaylandShellState::new::<D>(display),
            _keyboard_grab_state: XWaylandKeyboardGrabState::new::<D>(display),
            xwm: None,
            display: None,
            pending_windows: HashMap::new(),
            opening_placements: HashMap::new(),
        }
    }

    pub fn display_name(&self) -> Option<OsString> {
        self.display
            .map(|display| OsString::from(format!(":{display}")))
    }

    pub fn raise_window(&mut self, window: &smithay::desktop::Window) {
        let Some(surface) = window.x11_surface() else {
            return;
        };
        let Some(xwm) = self.xwm.as_mut() else {
            return;
        };
        if let Err(err) = xwm.raise_window(surface) {
            eventline::warn!("xwayland: failed to raise window: {err}");
        }
    }

    fn clear(&mut self) {
        self.xwm = None;
        self.display = None;
        self.pending_windows.clear();
        self.opening_placements.clear();
    }
}

pub fn start<D>(
    loop_handle: &LoopHandle<'static, Session<D>>,
    session: &mut Session<D>,
    publish_environment: bool,
) -> Result<(), Box<dyn std::error::Error>>
where
    D: SessionDriver,
{
    let display_handle = session.wayland.display_handle.clone();
    let (source, client) = XWayland::spawn(
        &display_handle,
        None,
        session.launch_environment(),
        true,
        Stdio::null(),
        Stdio::null(),
        |_| {},
    )?;
    let xwm_loop_handle = loop_handle.clone();
    loop_handle.insert_source(source, move |event, _, session| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => {
            if let Some(data) = client.get_data::<XWaylandClientData>() {
                data.compositor_state.set_client_scale(1.0);
            }
            match X11Wm::start_wm(
                xwm_loop_handle.clone(),
                &display_handle,
                x11_socket,
                client.clone(),
            ) {
                Ok(xwm) => {
                    session.xwayland.xwm = Some(xwm);
                    session.xwayland.display = Some(display_number);
                    let display = format!(":{display_number}");
                    if publish_environment {
                        crate::session::environment::activate_xwayland(OsStr::new(&display));
                    }
                    eventline::info!("xwayland: ready, DISPLAY={display}");
                    session.run_autostart_once();
                    session.request_redraw();
                }
                Err(err) => {
                    eventline::error!("xwayland: failed to attach window manager: {err}");
                    session.xwayland.clear();
                    session.run_autostart_once();
                }
            }
        }
        XWaylandEvent::Error => {
            eventline::error!("xwayland: server exited during startup");
            session.xwayland.clear();
            session.run_autostart_once();
        }
    })?;
    Ok(())
}

pub fn reconfigure_fullscreen(
    windows: Vec<(
        smithay::desktop::Window,
        smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    )>,
) {
    xwm::reconfigure_fullscreen(windows);
}

pub fn close_window(window: &smithay::desktop::Window) {
    if let Some(surface) = window.x11_surface()
        && let Err(err) = surface.close()
    {
        eventline::warn!("xwayland: failed to close window: {err}");
    }
}

pub fn configure_window(
    window: &smithay::desktop::Window,
    geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
) {
    xwm::configure_window(window, geometry);
}

pub fn set_window_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    fullscreen: bool,
) {
    xwm::set_window_fullscreen(session, window, fullscreen);
}

pub fn handle_commit<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    xwm::handle_commit(session, surface);
}

pub fn is_override_redirect(window: &smithay::desktop::Window) -> bool {
    window
        .x11_surface()
        .is_some_and(|surface| surface.is_override_redirect())
}

pub fn compositor_client_state(
    client: &smithay::reexports::wayland_server::Client,
) -> Option<&CompositorClientState> {
    client
        .get_data::<XWaylandClientData>()
        .map(|data| &data.compositor_state)
}

impl<D: SessionDriver> XWaylandShellHandler for Session<D> {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland.shell_state
    }

    fn surface_associated(
        &mut self,
        _xwm: smithay::xwayland::xwm::XwmId,
        wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        surface: smithay::xwayland::X11Surface,
    ) {
        xwm::surface_associated(self, wl_surface, surface);
    }
}

impl<D: SessionDriver> XWaylandKeyboardGrabHandler for Session<D> {
    fn keyboard_focus_for_xsurface(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) -> Option<Self::KeyboardFocus> {
        self.wayland
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == surface)
            })
            .and_then(KeyboardFocusTarget::for_window)
    }
}

delegate_xwayland_shell!(@<D: SessionDriver> Session<D>);
delegate_xwayland_keyboard_grab!(@<D: SessionDriver> Session<D>);
