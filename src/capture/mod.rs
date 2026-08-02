pub mod menu;
pub mod picker;
pub mod screencast;
mod screenshot;
pub mod source_chooser;

use std::path::PathBuf;

use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::seat::WaylandFocus;

use menu::{ScreenshotMenu, ScreenshotMode};
use picker::RegionPicker;
pub(crate) use screenshot::{capture_monitor_region_pixels, render_monitor_region_dmabuf};
pub(crate) use screenshot::{capture_source_pixels, capture_surface_tree, render_source_dmabuf};
pub use screenshot::{save_region, save_window};
use source_chooser::{SourceChooser, SourceMode, SourcePhase};

use crate::session::{Session, SessionDriver};

pub(crate) fn window_chrome_visible<D: SessionDriver>(
    session: &Session<D>,
    window: &Window,
) -> bool {
    !crate::xwayland::is_fullscreen(window)
        && window
            .wl_surface()
            .is_none_or(|surface| !session.fullscreen.suppresses_chrome(surface.as_ref()))
}

pub(crate) fn window_capture_size<D: SessionDriver>(
    session: &Session<D>,
    window: &Window,
) -> smithay::utils::Size<i32, Logical> {
    let client = window.geometry().size;
    if window_chrome_visible(session, window) {
        crate::titlebar::outer_size_for_client(
            window,
            client,
            &session.settings.decorations,
            &session.settings.font,
        )
    } else {
        client
    }
}

pub(crate) fn window_capture_client_offset<D: SessionDriver>(
    session: &Session<D>,
    window: &Window,
) -> Point<i32, Logical> {
    if !window_chrome_visible(session, window) {
        return (0, 0).into();
    }
    let outer = crate::titlebar::outer_rect_for_client(
        window,
        Rectangle::new((0, 0).into(), window.geometry().size),
        &session.settings.decorations,
        &session.settings.font,
    );
    (-outer.loc.x, -outer.loc.y).into()
}

pub(crate) fn window_capture_visual_geometry<D: SessionDriver>(
    session: &Session<D>,
    window: &Window,
    client: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    if !window_chrome_visible(session, window) {
        return client;
    }
    let native_client = Rectangle::<i32, Logical>::from_size(window.geometry().size);
    let native_outer = crate::titlebar::outer_rect_for_client(
        window,
        native_client,
        &session.settings.decorations,
        &session.settings.font,
    );
    let scale_x = client.size.w as f64 / native_client.size.w.max(1) as f64;
    let scale_y = client.size.h as f64 / native_client.size.h.max(1) as f64;
    let left = ((native_client.loc.x - native_outer.loc.x) as f64 * scale_x).round() as i32;
    let right = ((native_outer.size.w
        - native_client.size.w
        - (native_client.loc.x - native_outer.loc.x)) as f64
        * scale_x)
        .round() as i32;
    let top = ((native_client.loc.y - native_outer.loc.y) as f64 * scale_y).round() as i32;
    let bottom = ((native_outer.size.h
        - native_client.size.h
        - (native_client.loc.y - native_outer.loc.y)) as f64
        * scale_y)
        .round() as i32;
    Rectangle::new(
        (client.loc.x - left, client.loc.y - top).into(),
        (
            client
                .size
                .w
                .saturating_add(left)
                .saturating_add(right)
                .max(1),
            client
                .size
                .h
                .saturating_add(top)
                .saturating_add(bottom)
                .max(1),
        )
            .into(),
    )
}

#[derive(Default)]
pub struct CaptureState {
    picker: RegionPicker,
    source_chooser: SourceChooser,
    selection: Option<Selection>,
    pending: Option<PendingCapture>,
}

enum Selection {
    Menu,
    Area,
    Screen {
        geometry: Rectangle<i32, Logical>,
    },
    Window {
        surface: Option<WlSurface>,
        geometry: Option<Rectangle<i32, Logical>>,
    },
}

enum PendingCapture {
    Local {
        menu: ScreenshotMenu,
    },
    Screenshot {
        request_handle: String,
        reply: crate::ipc::ReplySender,
    },
    Source {
        request_handle: String,
        reply: crate::ipc::ReplySender,
    },
}

struct AcceptedCapture {
    target: AcceptedTarget,
    pending: PendingCapture,
}

enum AcceptedTarget {
    Area(Rectangle<i32, Logical>),
    Screen(Rectangle<i32, Logical>),
    Window(WlSurface),
    Source(halley_ipc::CaptureSource),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    Menu,
    Area,
    Screen,
    Window,
    Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePress {
    Consumed,
    ActivateScreenshot(ScreenshotMode),
    ActivateSource(SourceMode),
    Accept,
}

#[derive(Clone, Copy, Debug)]
pub enum CaptureOverlay<'a> {
    None,
    Region(Rectangle<i32, Logical>),
    Highlight(Rectangle<i32, Logical>),
    Menu {
        output_name: &'a str,
        selected: usize,
        hovered: Option<usize>,
        window_available: bool,
    },
    SourceMenu {
        output_name: &'a str,
        selected: usize,
        hovered: Option<usize>,
        monitor_available: bool,
        window_available: bool,
    },
}

impl CaptureState {
    pub fn is_active(&self) -> bool {
        self.selection.is_some() || self.source_chooser.is_active()
    }

    pub fn kind(&self) -> Option<CaptureKind> {
        if self.source_chooser.is_active() {
            return Some(CaptureKind::Source);
        }
        match self.selection.as_ref()? {
            Selection::Menu => Some(CaptureKind::Menu),
            Selection::Area => Some(CaptureKind::Area),
            Selection::Screen { .. } => Some(CaptureKind::Screen),
            Selection::Window { .. } => Some(CaptureKind::Window),
        }
    }

    pub fn menu_is_active(&self) -> bool {
        matches!(self.selection, Some(Selection::Menu))
            || self.source_chooser.phase() == Some(SourcePhase::Menu)
    }

    pub fn overlay(&self) -> CaptureOverlay<'_> {
        match self.selection.as_ref() {
            Some(Selection::Menu) => {
                let Some(menu) = self.local_menu() else {
                    return CaptureOverlay::None;
                };
                CaptureOverlay::Menu {
                    output_name: menu.output_name(),
                    selected: menu.selected(),
                    hovered: menu.hovered(),
                    window_available: menu.window_available(),
                }
            }
            Some(Selection::Area) => self
                .picker
                .region()
                .map(CaptureOverlay::Region)
                .unwrap_or(CaptureOverlay::None),
            Some(Selection::Screen { geometry, .. }) => CaptureOverlay::Highlight(*geometry),
            Some(Selection::Window { geometry, .. }) => geometry
                .map(CaptureOverlay::Highlight)
                .unwrap_or(CaptureOverlay::None),
            None if self.source_chooser.phase() == Some(SourcePhase::Menu) => {
                CaptureOverlay::SourceMenu {
                    output_name: self.source_chooser.output_name(),
                    selected: self.source_chooser.selected(),
                    hovered: self.source_chooser.hovered(),
                    monitor_available: self.source_chooser.monitor_available(),
                    window_available: self.source_chooser.window_available(),
                }
            }
            None => self
                .source_chooser
                .selection_geometry()
                .map(CaptureOverlay::Highlight)
                .unwrap_or(CaptureOverlay::None),
        }
    }

    pub fn begin_menu(
        &mut self,
        space: &Space<Window>,
        preferred_output: Option<&str>,
        window_available: bool,
    ) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let output = preferred_output
            .and_then(|name| space.outputs().find(|output| output.name() == name))
            .or_else(|| space.outputs().next());
        let Some(output) = output else {
            return false;
        };
        let Some(geometry) = space.output_geometry(output) else {
            return false;
        };
        self.selection = Some(Selection::Menu);
        self.pending = Some(PendingCapture::Local {
            menu: ScreenshotMenu::new(output.name(), geometry, window_available),
        });
        true
    }

    fn begin_area(
        &mut self,
        space: &Space<Window>,
        preferred_output: Option<&str>,
        pending: PendingCapture,
    ) -> Result<(), PendingCapture> {
        if self.pending.is_some() {
            return Err(pending);
        }
        let bounds = space
            .outputs()
            .filter_map(|output| space.output_geometry(output))
            .reduce(Rectangle::merge);
        let active = preferred_output
            .and_then(|name| {
                space
                    .outputs()
                    .find(|output| output.name() == name)
                    .and_then(|output| space.output_geometry(output))
            })
            .or_else(|| {
                space
                    .outputs()
                    .next()
                    .and_then(|output| space.output_geometry(output))
            });
        let (Some(bounds), Some(active)) = (bounds, active) else {
            return Err(pending);
        };
        self.picker.begin(bounds, active);
        self.selection = Some(Selection::Area);
        self.pending = Some(pending);
        Ok(())
    }

    fn begin_window(&mut self, pending: PendingCapture) -> Result<(), PendingCapture> {
        if self.pending.is_some() {
            return Err(pending);
        }
        self.selection = Some(Selection::Window {
            surface: None,
            geometry: None,
        });
        self.pending = Some(pending);
        Ok(())
    }

    fn begin_source(
        &mut self,
        space: &Space<Window>,
        preferred_output: Option<&str>,
        source_types: u32,
        pending: PendingCapture,
    ) -> Result<(), PendingCapture> {
        if self.pending.is_some() {
            return Err(pending);
        }
        let output = preferred_output
            .and_then(|name| space.outputs().find(|output| output.name() == name))
            .or_else(|| space.outputs().next());
        let Some(output) = output else {
            return Err(pending);
        };
        let Some(geometry) = space.output_geometry(output) else {
            return Err(pending);
        };
        self.source_chooser
            .begin(source_types, output.name(), geometry);
        self.pending = Some(pending);
        Ok(())
    }

    pub fn update_layout(&mut self, space: &Space<Window>) {
        if let Some(bounds) = space
            .outputs()
            .filter_map(|output| space.output_geometry(output))
            .reduce(Rectangle::merge)
        {
            self.picker.update_bounds(bounds);
        }
        if self.source_chooser.is_active()
            && let Some(output) = space
                .outputs()
                .find(|output| output.name() == self.source_chooser.output_name())
            && let Some(geometry) = space.output_geometry(output)
        {
            self.source_chooser.update_output_geometry(geometry);
        }
    }

    pub fn press(&mut self, position: (f64, f64)) -> Option<CapturePress> {
        match self.kind()? {
            CaptureKind::Menu => {
                let mode = self
                    .local_menu()
                    .and_then(|menu| menu.hit_test(Point::from(position)))
                    .map(|index| ScreenshotMode::ALL[index]);
                Some(
                    mode.map(CapturePress::ActivateScreenshot)
                        .unwrap_or(CapturePress::Consumed),
                )
            }
            CaptureKind::Area => {
                self.picker.press(Point::from(position));
                Some(CapturePress::Consumed)
            }
            CaptureKind::Source if self.source_chooser.phase() == Some(SourcePhase::Menu) => Some(
                self.source_chooser
                    .hit_test(Point::from(position))
                    .map(|index| CapturePress::ActivateSource(SourceMode::ALL[index]))
                    .unwrap_or(CapturePress::Consumed),
            ),
            CaptureKind::Screen | CaptureKind::Window | CaptureKind::Source => {
                Some(CapturePress::Accept)
            }
        }
    }

    pub fn motion(&mut self, position: (f64, f64)) -> bool {
        if matches!(self.selection, Some(Selection::Menu)) {
            return self
                .local_menu_mut()
                .is_some_and(|menu| menu.hover(Point::from(position)));
        }
        if self.source_chooser.phase() == Some(SourcePhase::Menu) {
            return self.source_chooser.hover_menu(Point::from(position));
        }
        match self.selection {
            Some(Selection::Area) => self.picker.motion(Point::from(position)),
            Some(Selection::Screen { .. } | Selection::Window { .. }) => true,
            Some(Selection::Menu) => unreachable!("handled above"),
            None => self.source_chooser.is_active(),
        }
    }

    pub fn release(&mut self) -> bool {
        match self.selection {
            Some(Selection::Menu) => true,
            Some(Selection::Area) => self.picker.release(),
            Some(Selection::Screen { .. } | Selection::Window { .. }) => true,
            None => self.source_chooser.is_active(),
        }
    }

    pub fn move_menu_selection(&mut self, delta: i32) -> bool {
        if self.source_chooser.phase() == Some(SourcePhase::Menu) {
            self.source_chooser.move_selection(delta)
        } else {
            matches!(self.selection, Some(Selection::Menu))
                && self
                    .local_menu_mut()
                    .is_some_and(|menu| menu.move_selection(delta))
        }
    }

    pub fn activate_selected_menu(&mut self, space: &Space<Window>) -> bool {
        if self.source_chooser.phase() == Some(SourcePhase::Menu) {
            return self.source_chooser.activate_selected();
        }
        let Some(mode) = self.local_menu().map(ScreenshotMenu::selected_mode) else {
            return false;
        };
        self.activate_menu(mode, space)
    }

    pub fn activate_source(&mut self, mode: SourceMode) -> bool {
        self.source_chooser.activate(mode)
    }

    pub fn activate_menu(&mut self, mode: ScreenshotMode, space: &Space<Window>) -> bool {
        if !matches!(self.selection, Some(Selection::Menu)) {
            return false;
        }
        let Some(menu) = self.local_menu().cloned() else {
            return false;
        };
        match mode {
            ScreenshotMode::Region => {
                let Some(bounds) = desktop_bounds(space) else {
                    return false;
                };
                self.picker.begin(bounds, menu.output_geometry());
                self.selection = Some(Selection::Area);
            }
            ScreenshotMode::Screen => {
                self.selection = Some(Selection::Screen {
                    geometry: menu.output_geometry(),
                });
            }
            ScreenshotMode::Window if menu.window_available() => {
                self.selection = Some(Selection::Window {
                    surface: None,
                    geometry: None,
                });
            }
            ScreenshotMode::Window => return false,
        }
        true
    }

    pub fn return_to_menu(&mut self) -> bool {
        if self.source_chooser.is_active() {
            return self.source_chooser.return_to_menu();
        }
        if matches!(self.selection, Some(Selection::Menu)) || self.local_menu().is_none() {
            return false;
        }
        if matches!(self.selection, Some(Selection::Area)) {
            self.picker.cancel();
        }
        self.selection = Some(Selection::Menu);
        true
    }

    pub fn hover_screen(&mut self, geometry: Rectangle<i32, Logical>) -> bool {
        let Some(Selection::Screen {
            geometry: selected_geometry,
        }) = self.selection.as_mut()
        else {
            return false;
        };
        *selected_geometry = geometry;
        true
    }

    pub fn hover_window(
        &mut self,
        surface: Option<WlSurface>,
        geometry: Option<Rectangle<i32, Logical>>,
    ) -> bool {
        let Some(Selection::Window {
            surface: selected_surface,
            geometry: selected_geometry,
        }) = self.selection.as_mut()
        else {
            return false;
        };
        *selected_surface = surface;
        *selected_geometry = geometry;
        true
    }

    pub fn hover_source(
        &mut self,
        monitor: halley_ipc::CaptureSource,
        window: Option<(halley_ipc::CaptureSource, Rectangle<i32, Logical>)>,
        monitor_geometry: Rectangle<i32, Logical>,
    ) -> bool {
        self.source_chooser
            .hover_source(monitor, window, monitor_geometry)
    }

    fn accept(&mut self) -> Option<AcceptedCapture> {
        if self.source_chooser.is_active() {
            let source = self.source_chooser.take_selected()?;
            return Some(AcceptedCapture {
                target: AcceptedTarget::Source(source),
                pending: self.pending.take()?,
            });
        }
        let target = match self.selection.take()? {
            Selection::Menu => {
                self.selection = Some(Selection::Menu);
                return None;
            }
            Selection::Area => AcceptedTarget::Area(self.picker.accept()?),
            Selection::Screen { geometry, .. } => AcceptedTarget::Screen(geometry),
            Selection::Window {
                surface: Some(surface),
                ..
            } => AcceptedTarget::Window(surface),
            selection @ Selection::Window { surface: None, .. } => {
                self.selection = Some(selection);
                return None;
            }
        };
        Some(AcceptedCapture {
            target,
            pending: self.pending.take()?,
        })
    }

    fn cancel(&mut self) -> Option<PendingCapture> {
        if matches!(self.selection.take(), Some(Selection::Area)) {
            self.picker.cancel();
        }
        self.source_chooser.cancel();
        self.pending.take()
    }

    fn local_menu(&self) -> Option<&ScreenshotMenu> {
        match self.pending.as_ref() {
            Some(PendingCapture::Local { menu }) => Some(menu),
            _ => None,
        }
    }

    fn local_menu_mut(&mut self) -> Option<&mut ScreenshotMenu> {
        match self.pending.as_mut() {
            Some(PendingCapture::Local { menu }) => Some(menu),
            _ => None,
        }
    }
}

pub fn begin_local<D: SessionDriver>(
    session: &mut Session<D>,
    preferred_output: Option<&str>,
    window_available: bool,
) -> bool {
    if session.session_lock.active() {
        return false;
    }
    if !session
        .capture
        .begin_menu(&session.wayland.space, preferred_output, window_available)
    {
        return false;
    }
    begin_modal_capture(session);
    true
}

pub fn request_screenshot<D: SessionDriver>(
    session: &mut Session<D>,
    request: halley_ipc::ScreenshotRequest,
    reply: crate::ipc::ReplySender,
) {
    if session.session_lock.active() {
        let _ = reply.send(
            halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Failed {
                message: "session is locked".to_string(),
            }),
            Vec::new(),
        );
        return;
    }
    match request.target {
        halley_ipc::ScreenshotTarget::Area => {
            let preferred = crate::wayland::focus::selected_output(&session.wayland)
                .map(|output| output.name());
            let pending = PendingCapture::Screenshot {
                request_handle: request.request_handle,
                reply,
            };
            if let Err(PendingCapture::Screenshot { reply, .. }) =
                session
                    .capture
                    .begin_area(&session.wayland.space, preferred.as_deref(), pending)
            {
                let _ = reply.send(
                    halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Failed {
                        message: "another capture is already active".to_string(),
                    }),
                    Vec::new(),
                );
                return;
            }
            begin_modal_capture(session);
        }
        halley_ipc::ScreenshotTarget::Screen => {
            let Some(region) = desktop_bounds(&session.wayland.space) else {
                let _ = reply.send(
                    halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Failed {
                        message: "no active outputs".to_string(),
                    }),
                    Vec::new(),
                );
                return;
            };
            reply_with_capture(reply, save_region(session, region));
        }
        halley_ipc::ScreenshotTarget::Window => {
            let pending = PendingCapture::Screenshot {
                request_handle: request.request_handle,
                reply,
            };
            if let Err(PendingCapture::Screenshot { reply, .. }) =
                session.capture.begin_window(pending)
            {
                let _ = reply.send(
                    halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Failed {
                        message: "another capture is already active".to_string(),
                    }),
                    Vec::new(),
                );
                return;
            }
            begin_modal_capture(session);
        }
    }
}

pub fn request_source<D: SessionDriver>(
    session: &mut Session<D>,
    request: halley_ipc::SourceChooserRequest,
    reply: crate::ipc::ReplySender,
) {
    if session.session_lock.active() {
        let _ = reply.send(
            halley_ipc::Response::Source(halley_ipc::SourceChooserResponse::Failed {
                message: "session is locked".to_string(),
            }),
            Vec::new(),
        );
        return;
    }
    let supported = request.source_types & (halley_ipc::SOURCE_MONITOR | halley_ipc::SOURCE_WINDOW);
    if supported == 0 {
        let _ = reply.send(
            halley_ipc::Response::Source(halley_ipc::SourceChooserResponse::Failed {
                message: "no supported source type was requested".to_string(),
            }),
            Vec::new(),
        );
        return;
    }
    let pending = PendingCapture::Source {
        request_handle: request.request_handle,
        reply,
    };
    let preferred =
        crate::wayland::focus::selected_output(&session.wayland).map(|output| output.name());
    if let Err(PendingCapture::Source { reply, .. }) = session.capture.begin_source(
        &session.wayland.space,
        preferred.as_deref(),
        supported,
        pending,
    ) {
        let _ = reply.send(
            halley_ipc::Response::Source(halley_ipc::SourceChooserResponse::Failed {
                message: "another capture is already active".to_string(),
            }),
            Vec::new(),
        );
        return;
    }
    begin_modal_capture(session);
}

pub fn cancel_portal<D: SessionDriver>(session: &mut Session<D>, request_handle: &str) -> bool {
    let matches = matches!(
        session.capture.pending.as_ref(),
        Some(PendingCapture::Screenshot { request_handle: active, .. }
            | PendingCapture::Source { request_handle: active, .. })
            if active == request_handle
    );
    if !matches {
        return false;
    }
    match session.capture.cancel() {
        Some(PendingCapture::Screenshot { reply, .. }) => {
            let _ = reply.send(
                halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Cancelled),
                Vec::new(),
            );
        }
        Some(PendingCapture::Source { reply, .. }) => {
            let _ = reply.send(
                halley_ipc::Response::Source(halley_ipc::SourceChooserResponse::Cancelled),
                Vec::new(),
            );
        }
        _ => {}
    }
    finish_modal_capture(session);
    true
}

pub fn accept_selected<D: SessionDriver>(session: &mut Session<D>) -> bool {
    let Some(accepted) = session.capture.accept() else {
        return false;
    };
    if let AcceptedTarget::Source(source) = accepted.target {
        if let PendingCapture::Source { reply, .. } = accepted.pending {
            let _ = reply.send(
                halley_ipc::Response::Source(halley_ipc::SourceChooserResponse::Selected(source)),
                Vec::new(),
            );
        }
        finish_modal_capture(session);
        return true;
    }
    let result = match accepted.target {
        AcceptedTarget::Area(region) | AcceptedTarget::Screen(region) => {
            save_region(session, region)
        }
        AcceptedTarget::Window(surface) => save_window(session, &surface),
        AcceptedTarget::Source(_) => unreachable!("handled above"),
    };
    match accepted.pending {
        PendingCapture::Local { menu } => match result {
            Ok(path) => {
                eventline::info!("screenshot saved to {}", path.display());
                let directory = path.parent().unwrap_or(path.as_path());
                session.shell.overlays.show_screenshot_saved(
                    menu.output_name().to_string(),
                    directory,
                    session.settings.overlays.notifications.success_duration_ms,
                    crate::frame_clock::monotonic_now(),
                );
            }
            Err(err) => eventline::error!("screenshot failed: {err}"),
        },
        PendingCapture::Screenshot { reply, .. } => reply_with_capture(reply, result),
        PendingCapture::Source { .. } => {
            unreachable!("source selection returned a screenshot target")
        }
    }
    finish_modal_capture(session);
    true
}

pub fn cancel_selected<D: SessionDriver>(session: &mut Session<D>) -> bool {
    let Some(pending) = session.capture.cancel() else {
        return false;
    };
    match pending {
        PendingCapture::Screenshot { reply, .. } => {
            let _ = reply.send(
                halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Cancelled),
                Vec::new(),
            );
        }
        PendingCapture::Source { reply, .. } => {
            let _ = reply.send(
                halley_ipc::Response::Source(halley_ipc::SourceChooserResponse::Cancelled),
                Vec::new(),
            );
        }
        PendingCapture::Local { .. } => {}
    }
    finish_modal_capture(session);
    true
}

fn reply_with_capture(
    reply: crate::ipc::ReplySender,
    result: Result<PathBuf, Box<dyn std::error::Error>>,
) {
    let response = match result {
        Ok(path) => halley_ipc::ScreenshotResponse::Saved {
            path: path.to_string_lossy().into_owned(),
        },
        Err(err) => halley_ipc::ScreenshotResponse::Failed {
            message: err.to_string(),
        },
    };
    let _ = reply.send(halley_ipc::Response::Screenshot(response), Vec::new());
}

fn desktop_bounds(space: &Space<Window>) -> Option<Rectangle<i32, Logical>> {
    space
        .outputs()
        .filter_map(|output| space.output_geometry(output))
        .reduce(Rectangle::merge)
}

fn begin_modal_capture<D: SessionDriver>(session: &mut Session<D>) {
    let moving_node = match &session.interactions.grab {
        crate::input::grab::Grab::MoveNode { id, .. } => Some(*id),
        _ => None,
    };
    let moving_window = match &session.interactions.grab {
        crate::input::grab::Grab::MoveWindow { id, .. } => *id,
        _ => None,
    };
    if (moving_node.is_some() || moving_window.is_some()) && session.nodes.physics.enabled {
        let _ = crate::nodes::tick_physics(session, crate::frame_clock::monotonic_now());
    }
    if let Some(id) = moving_node {
        session.nodes.clear_direct_motion(id);
    }
    if let Some(id) = moving_window
        && session.nodes.physics.enabled
    {
        session
            .nodes
            .lock_released_window(id, crate::frame_clock::monotonic_now());
    }
    crate::session::cancel_compositor_grab(session);
    crate::session::note_pointer_activity(session);
    let keyboard = session
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup");
    keyboard.set_focus(session, None, smithay::utils::SERIAL_COUNTER.next_serial());
    session.request_redraw();
}

fn finish_modal_capture<D: SessionDriver>(session: &mut Session<D>) {
    crate::session::note_pointer_activity(session);
    crate::session::sync_keyboard_focus(session, smithay::utils::SERIAL_COUNTER.next_serial());
    session.request_redraw();
}

#[cfg(test)]
mod tests {
    use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
    use smithay::utils::{Physical, Size, Transform};

    use super::*;

    fn output(name: &str, size: Size<i32, Physical>) -> Output {
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "halley-next".into(),
                model: "test".into(),
                serial_number: "test".into(),
            },
        );
        let mode = Mode {
            size,
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output
    }

    fn outputs() -> Space<Window> {
        let left = output("DP-1", (1920, 1080).into());
        let right = output("DP-2", (2560, 1440).into());
        let mut space = Space::default();
        space.map_output(&left, (0, 0));
        space.map_output(&right, (1920, 0));
        space
    }

    #[test]
    fn local_menu_opens_on_the_preferred_output() {
        let space = outputs();
        let mut capture = CaptureState::default();
        assert!(capture.begin_menu(&space, Some("DP-2"), true));
        assert!(matches!(
            capture.overlay(),
            CaptureOverlay::Menu {
                output_name: "DP-2",
                selected: 0,
                window_available: true,
                ..
            }
        ));
    }

    #[test]
    fn escape_from_a_local_selector_returns_to_the_menu() {
        let space = outputs();
        let mut capture = CaptureState::default();
        assert!(capture.begin_menu(&space, Some("DP-2"), true));
        assert!(capture.activate_menu(ScreenshotMode::Region, &space));
        assert_eq!(capture.kind(), Some(CaptureKind::Area));
        assert!(capture.return_to_menu());
        assert_eq!(capture.kind(), Some(CaptureKind::Menu));
        assert!(!capture.return_to_menu());
    }

    #[test]
    fn returning_to_menu_retains_the_adjusted_region() {
        let space = outputs();
        let mut capture = CaptureState::default();
        assert!(capture.begin_menu(&space, Some("DP-2"), true));
        assert!(capture.activate_menu(ScreenshotMode::Region, &space));
        capture.press((3000.0, 700.0));
        capture.motion((3100.0, 750.0));
        capture.release();
        let CaptureOverlay::Region(adjusted) = capture.overlay() else {
            panic!("region selector should be active");
        };

        assert!(capture.return_to_menu());
        assert!(capture.activate_menu(ScreenshotMode::Region, &space));
        assert!(matches!(
            capture.overlay(),
            CaptureOverlay::Region(region) if region == adjusted
        ));
    }

    #[test]
    fn screen_selection_accepts_the_output_last_hovered() {
        let space = outputs();
        let mut capture = CaptureState::default();
        assert!(capture.begin_menu(&space, Some("DP-2"), true));
        assert!(capture.activate_menu(ScreenshotMode::Screen, &space));
        let left = Rectangle::new((0, 0).into(), (1920, 1080).into());
        assert!(capture.hover_screen(left));
        let accepted = capture.accept().expect("screen capture should be ready");
        assert!(matches!(accepted.target, AcceptedTarget::Screen(region) if region == left));
    }
}
