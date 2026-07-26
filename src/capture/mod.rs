pub mod picker;
mod screenshot;

use std::path::PathBuf;

use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};

use picker::RegionPicker;
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
}

enum PendingCapture {
    Local,
    Portal {
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
}

impl CaptureState {
    pub fn is_active(&self) -> bool {
        self.selection.is_some()
    }

    pub fn selects_window(&self) -> bool {
        matches!(self.selection, Some(Selection::Window { .. }))
    }

    pub fn region(&self) -> Option<Rectangle<i32, Logical>> {
        match self.selection.as_ref()? {
            Selection::Area => self.picker.region(),
            Selection::Window { geometry, .. } => *geometry,
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
            Some(Selection::Window { .. }) => true,
            None => false,
        }
    }

    pub fn motion(&mut self, position: (f64, f64)) -> bool {
        match self.selection {
            Some(Selection::Area) => self.picker.motion(Point::from(position)),
            Some(Selection::Window { .. }) => true,
            None => false,
        }
    }

    pub fn release(&mut self) -> bool {
        match self.selection {
            Some(Selection::Area) => self.picker.release(),
            Some(Selection::Window { .. }) => true,
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
            let pending = PendingCapture::Portal {
                request_handle: request.request_handle,
                reply,
            };
            if let Err(PendingCapture::Portal { reply, .. }) =
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
            let pending = PendingCapture::Portal {
                request_handle: request.request_handle,
                reply,
            };
            if let Err(PendingCapture::Portal { reply, .. }) = session.capture.begin_window(pending)
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

pub fn cancel_portal<D: SessionDriver>(session: &mut Session<D>, request_handle: &str) -> bool {
    let matches = matches!(
        session.capture.pending.as_ref(),
        Some(PendingCapture::Portal {
            request_handle: active,
            ..
        }) if active == request_handle
    );
    if !matches {
        return false;
    }
    if let Some(PendingCapture::Portal { reply, .. }) = session.capture.cancel() {
        let _ = reply.send(
            halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Cancelled),
            Vec::new(),
        );
    }
    session.request_redraw();
    true
}

pub fn accept_selected<D: SessionDriver>(session: &mut Session<D>) -> bool {
    let Some(accepted) = session.capture.accept() else {
        return false;
    };
    let (result, remembered_region) = match accepted.target {
        AcceptedTarget::Area(region) => (save_region(session, region), Some(region)),
        AcceptedTarget::Window(surface) => (save_window(session, &surface), None),
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
        PendingCapture::Portal { reply, .. } => reply_with_capture(reply, result),
    }
    true
}

pub fn cancel_selected<D: SessionDriver>(session: &mut Session<D>) -> bool {
    let Some(pending) = session.capture.cancel() else {
        return false;
    };
    if let PendingCapture::Portal { reply, .. } = pending {
        let _ = reply.send(
            halley_ipc::Response::Screenshot(halley_ipc::ScreenshotResponse::Cancelled),
            Vec::new(),
        );
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
