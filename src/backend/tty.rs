use std::error::Error;

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::udev;
use smithay::output::OutputModeSource;
use smithay::reexports::drm::control::{Mode, connector, crtc};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Physical, Point, Scale, Size, Transform};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::Renderable;
use crate::cursor::CursorImage;

type TtyDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

type TtyDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// The tty (DRM/KMS) backend - real hardware output, no host compositor
/// involved. Wraps exactly what rendering needs, mirroring how
/// `WinitBackend` wraps exactly `WinitGraphicsBackend<GlesRenderer>`.
///
/// One entry per connected connector at startup (no hotplug) - every
/// connected output must be initialized, not just the first, or its CRTC is
/// left in stale, un-negotiated state during another CRTC's atomic modeset
/// commit, which caused a real system freeze on this project's AMD hardware.
///
/// `session` is kept only for later VT-switch handling (`pause`/`resume`) -
/// `render()` never reaches into it, matching `Renderable`'s narrow contract.
pub struct TtyBackend {
    session: LibSeatSession,
    renderer: GlesRenderer,
    drm_output_manager: TtyDrmOutputManager,
    drm_outputs: Vec<(crtc::Handle, TtyDrmOutput)>,
    /// The first successfully initialized output's size - used only to
    /// clamp the single shared pointer position (see `Pointer`'s doc
    /// comment on the multi-output simplification). Not per-output layout;
    /// real per-output pointer coordinate spaces are future multi-monitor
    /// work.
    output_size: Size<i32, Physical>,
}

impl TtyBackend {
    /// Opens the seat, the primary GPU, and the first connected
    /// connector/CRTC/mode found on it. Notifiers are returned rather than
    /// owned by `TtyBackend` - whatever drives the event loop inserts them,
    /// exactly like `main.rs` owns `winit_source` today rather than
    /// `WinitBackend` doing so itself.
    pub fn new() -> Result<(TtyBackend, LibSeatSessionNotifier, DrmDeviceNotifier), Box<dyn Error>> {
        let (mut session, session_notifier) = LibSeatSession::new()?;

        let gpu_path = udev::all_gpus(session.seat())?
            .into_iter()
            .next()
            .ok_or("no GPU found on seat")?;

        let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
        let fd = session.open(&gpu_path, flags)?;
        let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));

        let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), false)?;

        // Every connected connector is collected here (not just the first) -
        // leaving a second monitor's CRTC untouched while committing an
        // atomic modeset on another CRTC of the same AMD GPU is what caused
        // a real system freeze during testing (see the plan for the full
        // diagnosis). Matches anvil's and niri's own connector-handling
        // pattern, both confirmed via source to loop over every connector.
        let mut scanner: DrmScanner = DrmScanner::new();
        let scan = scanner.scan_connectors(&drm)?;
        let mut connected: Vec<(connector::Handle, crtc::Handle, Mode)> = Vec::new();
        for event in scan {
            if let DrmScanEvent::Connected { connector, crtc } = event
                && let (Some(crtc), Some(mode)) = (crtc, connector.modes().first().copied())
            {
                connected.push((connector.handle(), crtc, mode));
            }
        }
        if connected.is_empty() {
            return Err("no connected connector/CRTC/mode found".into());
        }

        let gbm = GbmDevice::new(drm_fd)?;
        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );

        let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
        let egl_context = EGLContext::new(&egl_display)?;
        let mut renderer = unsafe { GlesRenderer::new(egl_context)? };
        let renderer_formats: Vec<Format> = renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();

        let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::All);
        let mut drm_output_manager: TtyDrmOutputManager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            [Fourcc::Argb8888],
            renderer_formats,
        );

        // Initialize every connected connector, not just one - a connector
        // whose output fails to initialize is logged and skipped rather than
        // failing the whole backend, so a working primary still comes up
        // even if a secondary monitor can't be driven for some reason.
        let mut drm_outputs = Vec::new();
        let mut output_size = None;
        for (connector, crtc, mode) in connected {
            let (width, height) = mode.size();
            let size = Size::from((width as i32, height as i32));
            let output_mode_source = OutputModeSource::Static {
                size,
                scale: Scale::from(1.0),
                transform: Transform::Normal,
            };

            let result = drm_output_manager
                .lock()
                .initialize_output::<GlesRenderer, SolidColorRenderElement>(
                    crtc,
                    mode,
                    &[connector],
                    output_mode_source,
                    None,
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                );

            match result {
                Ok(drm_output) => {
                    output_size.get_or_insert(size);
                    drm_outputs.push((crtc, drm_output));
                }
                Err(err) => eprintln!("failed to initialize output for {connector:?}: {err}"),
            }
        }

        let Some(output_size) = output_size else {
            return Err("no output could be initialized".into());
        };

        let backend = TtyBackend {
            session,
            renderer,
            drm_output_manager,
            drm_outputs,
            output_size,
        };

        Ok((backend, session_notifier, drm_notifier))
    }

    /// The size used to clamp the shared pointer position - see the
    /// `output_size` field's doc comment for the multi-output caveat.
    pub fn output_size(&self) -> Size<i32, Physical> {
        self.output_size
    }

    /// Reacquire DRM master and resync KMS state after a VT switch back.
    /// Kept separate from `Renderable` (like `WinitBackend::request_redraw()`)
    /// since there's no shared shape with winit worth forcing into one trait
    /// method - the session-event closure only ever needs `&mut TtyBackend`,
    /// never the whole compositor state (the flaw old halley's
    /// `apply_tty_reload(..., st: &mut Halley, ...)` had).
    pub fn resume(&mut self) -> Result<(), Box<dyn Error>> {
        self.drm_output_manager.lock().activate(false)?;
        Ok(())
    }

    /// Drop DRM master before a VT switch away.
    pub fn pause(&mut self) {
        self.drm_output_manager.pause();
    }

    /// A cheap clone of the session, for building a libinput context
    /// externally - matches the existing "notifiers aren't owned by the
    /// backend, whatever drives the loop inserts them" pattern, extended to
    /// input. `LibSeatSession` is `Clone` (internally `Weak`-based), so this
    /// isn't a real second session.
    pub fn session(&self) -> LibSeatSession {
        self.session.clone()
    }

    /// Acknowledge a page-flip completion for one output, called from the
    /// `DrmEvent::VBlank(crtc)` handler - the DRM-path equivalent of
    /// `WinitBackend::request_redraw()`. Takes a `crtc::Handle` since with
    /// multiple outputs "which one flipped" is no longer implicit. Must be
    /// followed by a fresh `render()` call to queue that output's next frame.
    pub fn frame_submitted(&mut self, crtc: crtc::Handle) -> Result<(), Box<dyn Error>> {
        if let Some((_, drm_output)) = self.drm_outputs.iter_mut().find(|(c, _)| *c == crtc) {
            drm_output.frame_submitted()?;
        }
        Ok(())
    }
}

impl Renderable for TtyBackend {
    fn render(
        &mut self,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
    ) -> Result<(), Box<dyn Error>> {
        // A single bad output shouldn't hide a working one - failures are
        // logged per-output, and only surfaced to the caller if literally
        // every output failed.
        let mut ok_count = 0;
        let mut last_err: Option<Box<dyn Error>> = None;

        for (crtc, drm_output) in &mut self.drm_outputs {
            // Built before render_frame() borrows the renderer again -
            // from_buffer() only needs it transiently to import the texture.
            let cursor_element = match MemoryRenderBufferRenderElement::from_buffer(
                &mut self.renderer,
                Point::<f64, Physical>::from(cursor_position),
                &cursor.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => element,
                Err(err) => {
                    eprintln!("failed to build cursor element for {crtc:?}: {err}");
                    last_err = Some(Box::new(err));
                    continue;
                }
            };

            match drm_output.render_frame::<_, MemoryRenderBufferRenderElement<GlesRenderer>>(
                &mut self.renderer,
                &[cursor_element],
                clear,
                FrameFlags::empty(),
            ) {
                Ok(result) => {
                    ok_count += 1;
                    if !result.is_empty
                        && let Err(err) = drm_output.queue_frame(())
                    {
                        eprintln!("queue_frame failed for {crtc:?}: {err}");
                    }
                }
                Err(err) => {
                    eprintln!("render_frame failed for {crtc:?}: {err}");
                    last_err = Some(Box::new(err));
                }
            }
        }

        if ok_count == 0
            && let Some(err) = last_err
        {
            return Err(err);
        }

        Ok(())
    }
}
