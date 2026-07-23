use std::error::Error;

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::renderer::Color32F;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::udev;
use smithay::output::OutputModeSource;
use smithay::reexports::drm::control::{connector, crtc, Mode};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Scale, Size, Transform};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::Renderable;

type TtyDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

type TtyDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// The tty (DRM/KMS) backend - real hardware output, no host compositor
/// involved. Wraps exactly what one-output rendering needs, mirroring how
/// `WinitBackend` wraps exactly `WinitGraphicsBackend<GlesRenderer>`.
///
/// `session` is kept only for later VT-switch handling (`pause`/`resume`) -
/// `render()` never reaches into it, matching `Renderable`'s narrow contract.
pub struct TtyBackend {
    session: LibSeatSession,
    renderer: GlesRenderer,
    drm_output_manager: TtyDrmOutputManager,
    drm_output: TtyDrmOutput,
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
            if let DrmScanEvent::Connected { connector, crtc } = event {
                if let (Some(crtc), Some(mode)) = (crtc, connector.modes().first().copied()) {
                    connected.push((connector.handle(), crtc, mode));
                }
            }
        }
        let (connector, crtc, mode) = *connected
            .first()
            .ok_or("no connected connector/CRTC/mode found")?;

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

        let (width, height) = mode.size();
        let output_mode_source = OutputModeSource::Static {
            size: Size::from((width as i32, height as i32)),
            scale: Scale::from(1.0),
            transform: Transform::Normal,
        };

        let drm_output = drm_output_manager
            .lock()
            .initialize_output::<GlesRenderer, SolidColorRenderElement>(
                crtc,
                mode,
                &[connector],
                output_mode_source,
                None,
                &mut renderer,
                &DrmOutputRenderElements::default(),
            )?;

        let backend = TtyBackend {
            session,
            renderer,
            drm_output_manager,
            drm_output,
        };

        Ok((backend, session_notifier, drm_notifier))
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

    /// Acknowledge a page-flip completion, called from the `DrmEvent::VBlank`
    /// handler - the DRM-path equivalent of `WinitBackend::request_redraw()`.
    /// Must be followed by a fresh `render()` call to queue the next frame.
    pub fn frame_submitted(&mut self) -> Result<(), Box<dyn Error>> {
        self.drm_output.frame_submitted()?;
        Ok(())
    }
}

impl Renderable for TtyBackend {
    fn render(&mut self, clear: Color32F) -> Result<(), Box<dyn Error>> {
        let result = self.drm_output.render_frame::<_, SolidColorRenderElement>(
            &mut self.renderer,
            &[],
            clear,
            FrameFlags::empty(),
        )?;

        if !result.is_empty {
            self.drm_output.queue_frame(())?;
        }

        Ok(())
    }
}
