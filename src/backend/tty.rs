use std::error::Error;
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::compositor::PrimaryPlaneElement;
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmNode, NodeType};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::udev;
use smithay::output::{Output, PhysicalProperties, Subpixel};
use smithay::reexports::drm::control::{Mode, connector, crtc};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::DeviceFd;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::scene::SceneElement;
use super::tty_output::{
    OutputState, connector_name, connector_output_info, default_mode, drm_output_mode, output_diff,
    output_target,
};
use super::{FrameSubmission, RenderOutcome, RenderRequest, RenderStatus, Renderable};

type TtyDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    FrameSubmission,
    DrmDeviceFd,
>;

pub(super) type TtyDrmOutput = DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    FrameSubmission,
    DrmDeviceFd,
>;

/// One connected output plus whether it currently has a frame queued and
/// awaiting its VBlank. DRM only produces a VBlank in response to a page
/// flip it was actually asked to do - a scene that settles into "nothing
/// changed" naturally queues no further frame and so never gets another
/// VBlank at all, which was the only thing driving redraws. `pending` lets
/// `render()` be called eagerly (e.g. on every input event, not just on
/// VBlank) without ever double-submitting a commit for an output that's
/// still waiting on its last one.
struct DrmOutputEntry {
    crtc: crtc::Handle,
    connector: connector::Info,
    current_mode: Mode,
    configured_vrr: halley_config::Vrr,
    output: Output,
    drm_output: TtyDrmOutput,
    dmabuf_feedback: Option<super::dmabuf::SurfaceDmabufFeedback>,
    pending: bool,
}

pub struct AppliedOutputChange {
    pub output: Output,
    pub mode_changed: bool,
    pub size_changed: bool,
    pub layout_changed: bool,
}

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
    drm_outputs: Vec<DrmOutputEntry>,
    /// The `wl_output` where newly mapped windows begin.
    primary_output: Output,
    /// Every successfully initialized output, in connector-scan order - each
    /// one has its real name and configured mode/position/transform. Driving
    /// code advertises and maps all of them into Smithay's `Space`.
    outputs: Vec<Output>,
    /// IPC inventory for every physically connected connector, including
    /// connectors which had no usable CRTC/mode or failed initialization.
    /// Connected outputs are discovered at startup; active entries are
    /// updated as configuration changes are applied.
    ipc_output_info: Vec<halley_ipc::OutputInfo>,
    render_node: DrmNode,
}

impl TtyBackend {
    /// Opens the seat and primary GPU, then initializes the connected
    /// connectors with usable CRTC/mode pairs. Notifiers are returned rather than
    /// owned by `TtyBackend` - whatever drives the event loop inserts them,
    /// exactly like `session::winit` owns `winit_source` today rather than
    /// `WinitBackend` doing so itself.
    pub fn new(
        outputs_config: &[halley_config::OutputConfig],
    ) -> Result<(TtyBackend, LibSeatSessionNotifier, DrmDeviceNotifier), Box<dyn Error>> {
        let (mut session, session_notifier) = LibSeatSession::new()?;

        let gpu_path = udev::all_gpus(session.seat())?
            .into_iter()
            .next()
            .ok_or("no GPU found on seat")?;
        let primary_node = DrmNode::from_path(&gpu_path)?;
        let render_node = primary_node
            .node_with_type(NodeType::Render)
            .and_then(Result::ok)
            .unwrap_or(primary_node);

        let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
        let fd = session.open(&gpu_path, flags)?;
        let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));

        let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), false)?;

        // Every connected connector is collected here because the device's
        // atomic output state must cover all active CRTCs.
        // Mode selection happens later, per-connector, once config is
        // loaded (see `select_mode`) - not here, so it can take a
        // configured `output:` block into account.
        let mut scanner: DrmScanner = DrmScanner::new();
        let scan = scanner.scan_connectors(&drm)?;
        let mut connected: Vec<(connector::Info, Option<crtc::Handle>)> = Vec::new();
        for event in scan {
            if let DrmScanEvent::Connected { connector, crtc } = event {
                connected.push((connector, crtc));
            }
        }
        if connected.is_empty() {
            return Err("no connected connector found".into());
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
        let mut primary_output: Option<Output> = None;
        let mut outputs = Vec::new();
        let mut ipc_output_info = Vec::new();
        for (connector, crtc) in connected {
            let name = connector_name(&connector);
            let configured = outputs_config.iter().find(|cfg| cfg.name == name);
            let target = output_target(&connector, configured);
            let offset = target.offset;
            let vrr = target.vrr;

            if connector.modes().is_empty() {
                eventline::warn!("output {name:?}: connected connector advertises no modes");
                ipc_output_info.push(connector_output_info(name, &connector, None, offset, vrr));
                continue;
            }
            let Some(crtc) = crtc else {
                eventline::warn!("output {name:?}: connected connector has no available CRTC");
                ipc_output_info.push(connector_output_info(name, &connector, None, offset, vrr));
                continue;
            };

            let mode = target.mode;
            // Use the live Smithay Output as the mode source so later
            // mode/transform changes resize and transform the compositor's
            // buffers without rebuilding the DRM output.
            let output = Output::new(
                name.clone(),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "halley-next".into(),
                    model: "tty".into(),
                    serial_number: "unknown".into(),
                },
            );
            let output_mode = drm_output_mode(&mode);
            output.change_current_state(
                Some(output_mode),
                Some(target.transform),
                None,
                Some(offset.into()),
            );
            output.set_preferred(drm_output_mode(&default_mode(&connector)));

            let result = drm_output_manager
                .lock()
                .initialize_output::<GlesRenderer, SolidColorRenderElement>(
                    crtc,
                    mode,
                    &[connector.handle()],
                    &output,
                    None,
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                );

            match result {
                Ok(drm_output) => {
                    let dmabuf_feedback = match super::tty_dmabuf::surface_feedback(
                        &drm_output,
                        renderer.dmabuf_formats(),
                        render_node,
                        primary_node,
                    ) {
                        Ok(feedback) => Some(feedback),
                        Err(err) => {
                            eventline::warn!(
                                "output {name:?}: failed to build DMA-BUF scan-out feedback: {err}"
                            );
                            None
                        }
                    };
                    if vrr == halley_config::Vrr::On {
                        eventline::warn!(
                            "output {name:?}: vrr \"on\" is configured but not wired to real hardware VRR yet \
                             (needs lower-level DRM compositor access this backend doesn't have) - ignored for now"
                        );
                    }

                    primary_output.get_or_insert_with(|| output.clone());
                    outputs.push(output.clone());
                    ipc_output_info.push(connector_output_info(
                        name,
                        &connector,
                        Some(mode),
                        offset,
                        vrr,
                    ));
                    drm_outputs.push(DrmOutputEntry {
                        crtc,
                        connector,
                        current_mode: mode,
                        configured_vrr: vrr,
                        output,
                        drm_output,
                        dmabuf_feedback,
                        pending: false,
                    });
                }
                Err(err) => {
                    eventline::error!("failed to initialize output {name:?}: {err}");
                    ipc_output_info
                        .push(connector_output_info(name, &connector, None, offset, vrr));
                }
            }
        }

        let Some(primary_output) = primary_output else {
            return Err("no output could be initialized".into());
        };

        let backend = TtyBackend {
            session,
            renderer,
            drm_output_manager,
            drm_outputs,
            primary_output,
            outputs,
            ipc_output_info,
            render_node,
        };

        Ok((backend, session_notifier, drm_notifier))
    }

    /// The `wl_output` used for primary-only window rendering and frame
    /// callbacks.
    pub fn primary_output(&self) -> &Output {
        &self.primary_output
    }

    /// Every initialized `wl_output`, for registering globals and mapping
    /// the configured layout into Smithay's `Space`.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.outputs.iter()
    }

    pub fn output_for_crtc(&self, crtc: crtc::Handle) -> Option<&Output> {
        self.drm_outputs
            .iter()
            .find(|entry| entry.crtc == crtc)
            .map(|entry| &entry.output)
    }

    pub fn dmabuf_capabilities(&self) -> super::dmabuf::DmabufCapabilities {
        super::dmabuf::DmabufCapabilities::new(
            Some(self.render_node.dev_id()),
            self.renderer.dmabuf_formats(),
        )
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self.renderer.import_dmabuf(dmabuf, None) {
            Ok(_) => {
                dmabuf.set_node(Some(self.render_node));
                true
            }
            Err(err) => {
                eventline::debug!("tty: failed to import DMA-BUF: {err}");
                false
            }
        }
    }

    pub fn dmabuf_feedback(
        &self,
        output: &Output,
    ) -> Option<&super::dmabuf::SurfaceDmabufFeedback> {
        self.drm_outputs
            .iter()
            .find(|entry| &entry.output == output)
            .and_then(|entry| entry.dmabuf_feedback.as_ref())
    }

    /// One output's refresh interval, used to time its estimated-VBlank
    /// fallback.
    pub fn refresh_interval(&self, crtc: crtc::Handle) -> Duration {
        let refresh_mhz = self
            .drm_outputs
            .iter()
            .find(|entry| entry.crtc == crtc)
            .and_then(|entry| entry.output.current_mode())
            .map(|mode| mode.refresh)
            .filter(|&refresh| refresh > 0)
            .unwrap_or(60_000);
        Duration::from_secs_f64(1000.0 / refresh_mhz as f64)
    }

    pub fn refresh_interval_for_output(&self, output: &Output) -> Duration {
        self.drm_outputs
            .iter()
            .find(|entry| &entry.output == output)
            .map(|entry| self.refresh_interval(entry.crtc))
            .unwrap_or_else(|| Duration::from_secs_f64(1.0 / 60.0))
    }

    /// Applies only effective per-output differences. In particular, a
    /// changed DP-1 block never calls `use_mode` or `change_current_state`
    /// for DP-2.
    pub fn apply_output_config(
        &mut self,
        outputs_config: &[halley_config::OutputConfig],
    ) -> Vec<AppliedOutputChange> {
        let mut changes = Vec::new();

        for index in 0..self.drm_outputs.len() {
            let (target, diff) = {
                let entry = &self.drm_outputs[index];
                let configured = outputs_config
                    .iter()
                    .find(|cfg| cfg.name == entry.output.name());
                let target = output_target(&entry.connector, configured);
                let location = entry.output.current_location();
                let current = OutputState {
                    mode: drm_output_mode(&entry.current_mode),
                    offset: (location.x, location.y),
                    transform: entry.output.current_transform(),
                    vrr: entry.configured_vrr,
                };
                let requested = OutputState {
                    mode: drm_output_mode(&target.mode),
                    offset: target.offset,
                    transform: target.transform,
                    vrr: target.vrr,
                };
                (target, output_diff(current, requested))
            };

            if !(diff.mode_changed
                || diff.offset_changed
                || diff.transform_changed
                || diff.vrr_changed)
            {
                continue;
            }

            if diff.mode_changed {
                let result = {
                    let renderer = &mut self.renderer;
                    let entry = &mut self.drm_outputs[index];
                    entry
                        .drm_output
                        .use_mode::<GlesRenderer, SolidColorRenderElement>(
                            target.mode,
                            renderer,
                            &DrmOutputRenderElements::default(),
                        )
                };
                if let Err(err) = result {
                    let name = self.drm_outputs[index].output.name();
                    eventline::error!(
                        "output {name:?}: failed to apply configured mode, keeping previous state: {err}"
                    );
                    continue;
                }
            }

            let (name, output, connector, current_mode, configured_vrr) = {
                let entry = &mut self.drm_outputs[index];
                entry.output.change_current_state(
                    diff.mode_changed.then(|| drm_output_mode(&target.mode)),
                    diff.transform_changed.then_some(target.transform),
                    None,
                    diff.offset_changed.then(|| target.offset.into()),
                );
                entry.current_mode = target.mode;
                entry.configured_vrr = target.vrr;
                (
                    entry.output.name(),
                    entry.output.clone(),
                    entry.connector.clone(),
                    entry.current_mode,
                    entry.configured_vrr,
                )
            };

            let info = connector_output_info(
                name.clone(),
                &connector,
                Some(current_mode),
                {
                    let location = output.current_location();
                    (location.x, location.y)
                },
                configured_vrr,
            );
            if let Some(existing) = self
                .ipc_output_info
                .iter_mut()
                .find(|existing| existing.name == name)
            {
                *existing = info;
            }

            changes.push(AppliedOutputChange {
                output,
                mode_changed: diff.mode_changed,
                size_changed: diff.size_changed,
                layout_changed: diff.size_changed || diff.offset_changed || diff.transform_changed,
            });
        }

        changes
    }

    /// Reacquire DRM master and resync KMS state after a VT switch back.
    /// Kept separate from `Renderable` (like `WinitBackend::request_redraw()`)
    /// since there's no shared shape with winit worth forcing into one trait
    /// method - the session-event closure only ever needs `&mut TtyBackend`,
    /// never the whole compositor state (the flaw old halley's
    /// `apply_tty_reload(..., st: &mut Halley, ...)` had).
    pub fn resume(&mut self) -> Result<(), Box<dyn Error>> {
        self.drm_output_manager.lock().activate(false)?;
        // Any frame that was in flight before the VT switch away is gone -
        // without this, a stale `pending` would permanently block that
        // output from rendering again (its VBlank is never coming).
        for entry in &mut self.drm_outputs {
            entry.pending = false;
        }
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

    pub fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    /// Acknowledge a page-flip completion for one output, called from the
    /// `DrmEvent::VBlank(crtc)` handler - the DRM-path equivalent of
    /// `WinitBackend::request_redraw()`. Takes a `crtc::Handle` since with
    /// multiple outputs "which one flipped" is no longer implicit. Must be
    /// followed by a fresh `render()` call to queue that output's next frame.
    pub fn frame_submitted(
        &mut self,
        crtc: crtc::Handle,
    ) -> Result<Option<FrameSubmission>, Box<dyn Error>> {
        if let Some(entry) = self.drm_outputs.iter_mut().find(|e| e.crtc == crtc) {
            let submitted = entry.drm_output.frame_submitted()?;
            entry.pending = false;
            return Ok(submitted);
        }
        Ok(None)
    }
}

impl crate::ipc::OutputInfoSource for TtyBackend {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo> {
        self.ipc_output_info.clone()
    }
}

impl Renderable for TtyBackend {
    fn render(
        &mut self,
        output: &Output,
        request: RenderRequest<'_>,
    ) -> Result<RenderOutcome, Box<dyn Error>> {
        let primary_output = self.primary_output.clone();
        let entry = self
            .drm_outputs
            .iter_mut()
            .find(|entry| &entry.output == output)
            .ok_or_else(|| format!("unknown tty output {:?}", output.name()))?;
        // DRM rejects a second commit while the previous page flip is still
        // pending. The next VBlank clears this flag and schedules another
        // render, so skipping here does not lose scene changes.
        if entry.pending {
            return Ok(RenderOutcome::new(RenderStatus::Skipped, None));
        }
        let output_geometry = request
            .space
            .output_geometry(&entry.output)
            .ok_or_else(|| format!("tty output {:?} is not mapped", entry.output.name()))?;
        let clear = request.clear;
        let target_presentation_time = request.target_presentation_time;
        let elements = super::scene::build(
            &mut self.renderer,
            &entry.output,
            &primary_output,
            output_geometry,
            request,
        )?;
        let result = entry.drm_output.render_frame::<_, SceneElement>(
            &mut self.renderer,
            &elements,
            clear,
            super::tty_dmabuf::frame_flags(),
        )?;

        let element_states = result.states.clone();
        if result.needs_sync()
            && let PrimaryPlaneElement::Swapchain(element) = &result.primary_element
            && let Err(err) = element.sync.wait()
        {
            eventline::warn!(
                "output {:?}: failed waiting for rendered frame completion: {err}",
                entry.output.name()
            );
        }

        if result.is_empty {
            return Ok(RenderOutcome::new(
                RenderStatus::Skipped,
                Some(element_states),
            ));
        }

        entry.drm_output.queue_frame(FrameSubmission {
            target_presentation_time,
        })?;
        entry.pending = true;
        Ok(RenderOutcome::new(
            RenderStatus::Submitted,
            Some(element_states),
        ))
    }
}
