pub mod picker;
mod screenshot;

use std::path::PathBuf;

use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};

use picker::RegionPicker;
pub(crate) use screenshot::{capture_source_pixels, render_source_dmabuf};
pub use screenshot::{save_region, save_window};

use crate::session::{Session, SessionDriver};

#[derive(Default)]
pub struct CaptureState {
    picker: RegionPicker,
    selection: Option<Selection>,
    pending: Option<PendingCapture>,
}

enum Selection {
    Area,
    Window {
        surface: Option<WlSurface>,
        geometry: Option<Rectangle<i32, Logical>>,
    },
    Source {
        source_types: u32,
        selected: Option<halley_ipc::CaptureSource>,
        geometry: Option<Rectangle<i32, Logical>>,
    },
}

enum PendingCapture {
    Local,
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
    Window(WlSurface),
    Source(halley_ipc::CaptureSource),
}

impl CaptureState {
    pub fn is_active(&self) -> bool {
        self.selection.is_some()
    }

    pub fn selects_window(&self) -> bool {
        matches!(self.selection, Some(Selection::Window { .. }))
    }

    pub fn selects_source(&self) -> bool {
        matches!(self.selection, Some(Selection::Source { .. }))
    }

    pub fn region(&self) -> Option<Rectangle<i32, Logical>> {
        match self.selection.as_ref()? {
            Selection::Area => self.picker.region(),
            Selection::Window { geometry, .. } => *geometry,
            Selection::Source { geometry, .. } => *geometry,
        }
    }

    pub fn begin_region(&mut self, space: &Space<Window>, preferred_output: Option<&str>) -> bool {
        self.begin_area(space, preferred_output, PendingCapture::Local)
            .is_ok()
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
        source_types: u32,
        pending: PendingCapture,
    ) -> Result<(), PendingCapture> {
        if self.pending.is_some() {
            return Err(pending);
        }
        self.selection = Some(Selection::Source {
            source_types,
            selected: None,
            geometry: None,
        });
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
    }

    pub fn press(&mut self, position: (f64, f64)) -> bool {
        match self.selection {
            Some(Selection::Area) => self.picker.press(Point::from(position)),
            Some(Selection::Window { .. } | Selection::Source { .. }) => true,
            None => false,
        }
    }

    pub fn motion(&mut self, position: (f64, f64)) -> bool {
        match self.selection {
            Some(Selection::Area) => self.picker.motion(Point::from(position)),
            Some(Selection::Window { .. } | Selection::Source { .. }) => true,
            None => false,
        }
    }

    pub fn release(&mut self) -> bool {
        match self.selection {
            Some(Selection::Area) => self.picker.release(),
            Some(Selection::Window { .. } | Selection::Source { .. }) => true,
            None => false,
        }
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
        let Some(Selection::Source {
            source_types,
            selected,
            geometry,
        }) = self.selection.as_mut()
        else {
            return false;
        };
        let window_allowed = *source_types & halley_ipc::SOURCE_WINDOW != 0;
        let monitor_allowed = *source_types & halley_ipc::SOURCE_MONITOR != 0;
        let choice = window
            .filter(|_| window_allowed)
            .or_else(|| monitor_allowed.then_some((monitor, monitor_geometry)));
        let (source, bounds) = choice
            .map(|(source, bounds)| (Some(source), Some(bounds)))
            .unwrap_or((None, None));
        *selected = source;
        *geometry = bounds;
        true
    }

    fn accept(&mut self) -> Option<AcceptedCapture> {
        let target = match self.selection.take()? {
            Selection::Area => AcceptedTarget::Area(self.picker.accept()?),
            Selection::Window {
                surface: Some(surface),
                ..
            } => AcceptedTarget::Window(surface),
            selection @ Selection::Window { surface: None, .. } => {
                self.selection = Some(selection);
                return None;
            }
            Selection::Source {
                selected: Some(source),
                ..
            } => AcceptedTarget::Source(source),
            selection @ Selection::Source { selected: None, .. } => {
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
        self.pending.take()
    }

    pub fn remember_successful(&mut self, region: Rectangle<i32, Logical>) {
        self.picker.remember_successful(region);
    }
}

pub fn request_screenshot<D: SessionDriver>(
    session: &mut Session<D>,
    request: halley_ipc::ScreenshotRequest,
    reply: crate::ipc::ReplySender,
) {
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
    if let Err(PendingCapture::Source { reply, .. }) =
        session.capture.begin_source(supported, pending)
    {
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
    session.request_redraw();
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
        return true;
    }
    let (result, remembered_region) = match accepted.target {
        AcceptedTarget::Area(region) => (save_region(session, region), Some(region)),
        AcceptedTarget::Window(surface) => (save_window(session, &surface), None),
        AcceptedTarget::Source(_) => unreachable!("handled above"),
    };
    if result.is_ok()
        && let Some(region) = remembered_region
    {
        session.capture.remember_successful(region);
    }
    match accepted.pending {
        PendingCapture::Local => match result {
            Ok(path) => eventline::info!("screenshot saved to {}", path.display()),
            Err(err) => eventline::error!("screenshot failed: {err}"),
        },
        PendingCapture::Screenshot { reply, .. } => reply_with_capture(reply, result),
        PendingCapture::Source { .. } => {
            unreachable!("source selection returned a screenshot target")
        }
    }
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
        PendingCapture::Local => {}
    }
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
    let keyboard = session
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup");
    keyboard.set_focus(session, None, smithay::utils::SERIAL_COUNTER.next_serial());
    session.request_redraw();
}
