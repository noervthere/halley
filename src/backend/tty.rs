use std::error::Error;
use std::time::Duration;

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Element, Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Color32F;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::udev;
use smithay::desktop::{Space, Window};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::drm::control::{Mode, ModeTypeFlags, connector, crtc};
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Scale, Transform};
use smithay::wayland::shell::wlr_layer::Layer;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::{FrameSubmission, RenderStatus, Renderable};
use crate::cursor::CursorImage;

type TtyDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, FrameSubmission, DrmDeviceFd>;

type TtyDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, FrameSubmission, DrmDeviceFd>;

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
    pending: bool,
}

pub struct AppliedOutputChange {
    pub output: Output,
    pub mode_changed: bool,
    pub size_changed: bool,
    pub layout_changed: bool,
}

#[derive(Clone, Copy)]
struct OutputTarget {
    mode: Mode,
    offset: (i32, i32),
    transform: Transform,
    vrr: halley_config::Vrr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OutputDiff {
    mode_changed: bool,
    size_changed: bool,
    offset_changed: bool,
    transform_changed: bool,
    vrr_changed: bool,
}

fn output_diff(
    current_mode: OutputMode,
    current_offset: (i32, i32),
    current_transform: Transform,
    current_vrr: halley_config::Vrr,
    target_mode: OutputMode,
    target_offset: (i32, i32),
    target_transform: Transform,
    target_vrr: halley_config::Vrr,
) -> OutputDiff {
    OutputDiff {
        mode_changed: current_mode != target_mode,
        size_changed: current_transform.transform_size(current_mode.size)
            != target_transform.transform_size(target_mode.size),
        offset_changed: current_offset != target_offset,
        transform_changed: current_transform != target_transform,
        vrr_changed: current_vrr != target_vrr,
    }
}

render_elements! {
    /// Combines layer, window, border, and cursor elements into one list -
    /// `DrmOutput::render_frame` takes a single element type per call, unlike
    /// the winit backend's `Frame`, which can be drawn into with several
    /// separate calls. Concrete over `GlesRenderer` (not generic over `R`) -
    /// this codebase only ever has one renderer, and `RescaledElement`
    /// itself is only implemented for `GlesRenderer`, so the old `<R>`
    /// generic bought nothing.
    TtyRenderElement<=GlesRenderer>;
    Rescaled=super::rescale::RescaledElement,
    Cropped=CropRenderElement<super::rescale::RescaledElement>,
    Border=SolidColorRenderElement,
    Layer=WaylandSurfaceRenderElement<GlesRenderer>,
    Cursor=MemoryRenderBufferRenderElement<GlesRenderer>,
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
}

/// `{interface}-{interface_id}`, e.g. "DP-1" - the standard connector-name
/// convention (matches sway/niri/wlroots), used to match a connector against
/// a configured `output:` block by its `name` field.
fn connector_name(connector: &connector::Info) -> String {
    format!("{}-{}", connector.interface().as_str(), connector.interface_id())
}

/// The single DRM-to-output-mode conversion used by selection, activation,
/// and IPC. Smithay performs the same rounded millihertz calculation as
/// niri, including interlace/doublescan handling.
fn drm_output_mode(mode: &Mode) -> OutputMode {
    OutputMode::from(*mode)
}

fn configured_refresh_millihz(refresh_hz: f64) -> i32 {
    (refresh_hz * 1000.0).round() as i32
}

/// Returns the connector-order index of an exact configured mode. When the
/// refresh is omitted, niri's policy is to use the highest refresh available
/// at that exact resolution.
fn matching_mode_index(
    modes: impl IntoIterator<Item = (usize, i32, i32, i32)>,
    width: i32,
    height: i32,
    refresh_hz: Option<f64>,
) -> Option<usize> {
    let refresh_millihz = refresh_hz.map(configured_refresh_millihz);
    modes
        .into_iter()
        .filter(|(_, mode_width, mode_height, mode_refresh)| {
            *mode_width == width
                && *mode_height == height
                && refresh_millihz.is_none_or(|refresh| *mode_refresh == refresh)
        })
        .max_by_key(|(_, _, _, refresh)| *refresh)
        .map(|(index, _, _, _)| index)
}

fn connector_output_info(
    name: String,
    connector: &connector::Info,
    current_mode: Option<Mode>,
    offset: (i32, i32),
    vrr: halley_config::Vrr,
) -> halley_ipc::OutputInfo {
    let modes = connector
        .modes()
        .iter()
        .map(|mode| {
            crate::ipc::mode_info(
                drm_output_mode(mode),
                mode.mode_type().contains(ModeTypeFlags::PREFERRED),
            )
        })
        .collect::<Vec<_>>();
    let current_mode =
        current_mode.and_then(|current| connector.modes().iter().position(|mode| *mode == current));

    halley_ipc::OutputInfo {
        name,
        modes,
        current_mode,
        offset_x: offset.0,
        offset_y: offset.1,
        vrr: crate::ipc::vrr_str(vrr).to_string(),
    }
}

/// This connector's preferred mode, or its first mode if none is flagged
/// preferred - used both as the fallback when no `output:` block configures
/// this connector, and when one does but no mode matches it exactly.
fn default_mode(connector: &connector::Info) -> Mode {
    connector
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| connector.modes().first())
        .copied()
        .expect("connector has at least one mode - checked when building `connected`")
}

/// Picks which mode to actually use for this connector: if a matching
/// `output:` block exists, its `width`/`height` (and `rate`, if set) must
/// match one of the connector's real modes *exactly* - no closest/fuzzy
/// rate matching, unlike old halley's `<2.0`Hz tolerance window. A
/// configured-but-unmatched output falls back to `default_mode` with a
/// clear error listing what's actually available, rather than silently
/// accepting a different rate or refusing to start.
fn select_mode(connector: &connector::Info, configured: Option<&halley_config::OutputConfig>) -> Mode {
    let name = connector_name(connector);
    let Some(cfg) = configured else {
        return default_mode(connector);
    };

    let matched = matching_mode_index(
        connector.modes().iter().enumerate().map(|(index, mode)| {
            let output_mode = drm_output_mode(mode);
            (index, output_mode.size.w, output_mode.size.h, output_mode.refresh)
        }),
        cfg.width,
        cfg.height,
        cfg.rate,
    )
    .and_then(|index| connector.modes().get(index));

    match matched {
        Some(mode) => *mode,
        None => {
            let available = connector
                .modes()
                .iter()
                .map(|mode| {
                    let output_mode = drm_output_mode(mode);
                    format!(
                        "{}x{}@{:.3}",
                        output_mode.size.w,
                        output_mode.size.h,
                        output_mode.refresh as f64 / 1000.0,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            eventline::warn!(
                "output {name:?}: configured {}x{}{} not found on this connector; available modes: {available}",
                cfg.width,
                cfg.height,
                cfg.rate.map(|hz| format!(" @ {hz}Hz")).unwrap_or_default(),
            );
            default_mode(connector)
        }
    }
}

/// Only rotation is representable (matches old halley's own `transform`
/// mapping) - any other value already falls back to `0` during config
/// parsing (`halley_config::output::parse_one_output`), so this only ever
/// sees 0/90/180/270.
fn transform_from_degrees(degrees: u16) -> Transform {
    match degrees {
        90 => Transform::_90,
        180 => Transform::_180,
        270 => Transform::_270,
        _ => Transform::Normal,
    }
}

fn output_target(
    connector: &connector::Info,
    configured: Option<&halley_config::OutputConfig>,
) -> OutputTarget {
    OutputTarget {
        mode: select_mode(connector, configured),
        offset: configured.map_or((0, 0), |cfg| (cfg.offset_x, cfg.offset_y)),
        transform: configured.map_or(Transform::Normal, |cfg| {
            transform_from_degrees(cfg.transform)
        }),
        vrr: configured.map(|cfg| cfg.vrr).unwrap_or_default(),
    }
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
                        pending: false,
                    });
                }
                Err(err) => {
                    eventline::error!("failed to initialize output {name:?}: {err}");
                    ipc_output_info.push(connector_output_info(name, &connector, None, offset, vrr));
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
                let current = drm_output_mode(&entry.current_mode);
                let requested = drm_output_mode(&target.mode);
                (
                    target,
                    output_diff(
                        current,
                        {
                            let location = entry.output.current_location();
                            (location.x, location.y)
                        },
                        entry.output.current_transform(),
                        entry.configured_vrr,
                        requested,
                        target.offset,
                        target.transform,
                        target.vrr,
                    ),
                )
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
                layout_changed: diff.size_changed
                    || diff.offset_changed
                    || diff.transform_changed,
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
        target_presentation_time: Duration,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
        space: &Space<Window>,
        focused: Option<&WlSurface>,
        decorations: &halley_config::Decorations,
        cameras: &crate::camera::OutputCameras,
        window_open_animations: &crate::animation::WindowOpenAnimations,
    ) -> Result<RenderStatus, Box<dyn Error>> {
        let primary_output = self.primary_output.clone();
        let entry = self
            .drm_outputs
            .iter_mut()
            .find(|entry| &entry.output == output)
            .ok_or_else(|| format!("unknown tty output {:?}", output.name()))?;
            // A page flip is already queued for this output and hasn't
            // landed yet - DRM won't accept a second commit before that
            // happens. Nothing is lost: `frame_submitted()` clears `pending`
            // on the next VBlank, and the driving code always re-renders
            // right after, picking up whatever changed in the meantime.
            if entry.pending {
                return Ok(RenderStatus::Skipped);
            }
            let output_geometry = space
                .output_geometry(&entry.output)
                .ok_or_else(|| format!("tty output {:?} is not mapped", entry.output.name()))?;
            let output_size = output_geometry.size.to_physical(1);
            let view = cameras
                .view(&entry.output.name())
                .expect("tty output camera initialized at startup");
            let output_camera_center = crate::camera::global_center(view.center, output_geometry);
            let zoom_scale = view.scale;
            let cursor_position =
                cursor_position_for_output(output_geometry, cursor_position);
            let drm_output = &mut entry.drm_output;

            // The cursor and screen-fixed layers use output-local
            // coordinates. Windows are separately camera-transformed and
            // filtered by Halley's single owning output.
            let mut elements: Vec<TtyRenderElement> =
                super::layer_surface_elements(&mut self.renderer, &entry.output, Layer::Overlay)
                    .into_iter()
                    .map(TtyRenderElement::Layer)
                    .collect();
            elements.extend(
                super::layer_surface_elements(&mut self.renderer, &entry.output, Layer::Top)
                    .into_iter()
                    .map(TtyRenderElement::Layer),
            );
            // Built directly per mapped window rather than via
            // `space_render_elements` - nesting `SpaceRenderElements`
            // inside this file's own combined element enum runs into an
            // internal bound mismatch in the render_elements! macro
            // (SpaceRenderElements's own generated impl wants
            // `ImportMemWl`/`ImportDmaWl`, which its own declared
            // `where` clause doesn't actually list); going straight to
            // `Window`'s `AsRenderElements` avoids nesting the macro output
            // of one render_elements! invocation inside another.
            //
            // `.rev()` is load-bearing: `Space::elements()` iterates z-order
            // back to front, but render element lists are front-to-back.
            for window in space.elements().rev() {
                if !crate::wayland::window_is_on_output(
                    window,
                    &entry.output,
                    &primary_output,
                ) {
                    continue;
                }
                let Some(geometry) = space.element_geometry(window) else {
                    continue;
                };
                let Some(location) = space.element_location(window) else {
                    continue;
                };
                let scaled_bbox = super::camera_rect(
                    geometry.to_physical(1),
                    output_camera_center,
                    output_size,
                    zoom_scale,
                );
                let opening_progress = window
                    .toplevel()
                    .and_then(|toplevel| {
                        window_open_animations
                            .progress(toplevel.wl_surface(), target_presentation_time)
                    })
                    .unwrap_or(1.0);
                let animated_bbox = crate::animation::window_open_rect(
                    scaled_bbox,
                    scaled_bbox,
                    opening_progress,
                );
                if animated_bbox.size.w == 0 || animated_bbox.size.h == 0 {
                    continue;
                }

                // `Space` locations refer to window geometry, while Smithay
                // renders from the underlying surface origin. GTK, Qt and
                // Firefox commonly use a non-zero geometry offset for CSD.
                let surface_location =
                    super::window_surface_location(location, window.geometry());
                let (popup_elements, surface_elements) = super::window_surface_elements(
                    &mut self.renderer,
                    window,
                    surface_location,
                );
                elements.extend(popup_elements.into_iter().map(|surface_element| {
                    let native_geo = surface_element.geometry(Scale::from(1.0));
                    let final_dst = super::camera_rect(
                        native_geo,
                        output_camera_center,
                        output_size,
                        zoom_scale,
                    );
                    // Scaled about the *window's* center like every other
                    // surface in the tree, so a popup that is already up when
                    // its toplevel maps rides the open animation instead of
                    // hanging full-size next to a window that is still a
                    // sliver. Not cropped to `animated_bbox` - popups
                    // legitimately extend past their parent's geometry.
                    let dst = crate::animation::window_open_rect(
                        final_dst,
                        scaled_bbox,
                        opening_progress,
                    );
                    TtyRenderElement::Rescaled(super::rescale::RescaledElement::new(
                        surface_element,
                        dst,
                    ))
                }));
                elements.extend(surface_elements.into_iter().filter_map(|surface_element| {
                    let native_geo = surface_element.geometry(Scale::from(1.0));
                    let final_dst = super::camera_rect(
                        native_geo,
                        output_camera_center,
                        output_size,
                        zoom_scale,
                    );
                    let dst = crate::animation::window_open_rect(
                        final_dst,
                        scaled_bbox,
                        opening_progress,
                    );
                    let element =
                        super::rescale::RescaledElement::new(surface_element, dst);
                    CropRenderElement::from_element(element, 1.0, animated_bbox)
                        .map(TtyRenderElement::Cropped)
                }));

                let is_focused = window
                    .toplevel()
                    .is_some_and(|t| Some(t.wl_surface()) == focused);
                let color = super::window_border_color(decorations, is_focused);
                // Deliberately *not* scaled by `opening_progress` - see the
                // matching comment in `winit.rs`.
                let border_width =
                    ((decorations.border_width_px as f64 * zoom_scale as f64).round() as i32).max(1);
                elements.extend(
                    super::border_strips(animated_bbox, border_width, color)
                        .into_iter()
                        .map(TtyRenderElement::Border),
                );
            }

            elements.extend(
                super::layer_surface_elements(&mut self.renderer, &entry.output, Layer::Bottom)
                    .into_iter()
                    .map(TtyRenderElement::Layer),
            );
            elements.extend(
                super::layer_surface_elements(
                    &mut self.renderer,
                    &entry.output,
                    Layer::Background,
                )
                .into_iter()
                .map(TtyRenderElement::Layer),
            );

            if let Some(cursor_position) = cursor_position {
                // Built before render_frame() borrows the renderer again -
                // from_buffer() only needs it transiently to import the texture.
                let element = MemoryRenderBufferRenderElement::from_buffer(
                    &mut self.renderer,
                    cursor_position,
                    &cursor.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )?;
                // Inserted at the *front*, not pushed: this list is
                // front-to-back, so index 0 is the topmost element. The
                // cursor has to composite over the entire scene, and unlike
                // the winit backend there's no second draw call to put it
                // in - `DrmOutput::render_frame` takes one list.
                elements.insert(0, TtyRenderElement::Cursor(element));
            }

            let result = drm_output.render_frame::<_, TtyRenderElement>(
                &mut self.renderer,
                &elements,
                clear,
                FrameFlags::empty(),
            )?;

            if result.is_empty {
                return Ok(RenderStatus::Skipped);
            }

            drm_output.queue_frame(FrameSubmission {
                target_presentation_time,
            })?;
            entry.pending = true;
            Ok(RenderStatus::Submitted)
    }
}

fn cursor_position_for_output(
    output_geometry: Rectangle<i32, Logical>,
    cursor_position: (f64, f64),
) -> Option<Point<f64, Physical>> {
    let cursor_position = Point::<f64, Logical>::from(cursor_position);
    output_geometry
        .to_f64()
        .contains(cursor_position)
        .then(|| (cursor_position - output_geometry.loc.to_f64()).to_physical(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_mode(width: i32, height: i32, refresh: i32) -> OutputMode {
        OutputMode {
            size: smithay::utils::Size::from((width, height)),
            refresh,
        }
    }

    #[test]
    fn unchanged_output_has_no_reload_work() {
        assert_eq!(
            output_diff(
                output_mode(1920, 1200, 74_930),
                (2560, 0),
                Transform::Normal,
                halley_config::Vrr::Auto,
                output_mode(1920, 1200, 74_930),
                (2560, 0),
                Transform::Normal,
                halley_config::Vrr::Auto,
            ),
            OutputDiff {
                mode_changed: false,
                size_changed: false,
                offset_changed: false,
                transform_changed: false,
                vrr_changed: false,
            }
        );
    }

    #[test]
    fn refresh_only_change_does_not_reset_layout_or_size() {
        let diff = output_diff(
            output_mode(2560, 1440, 60_000),
            (0, 0),
            Transform::Normal,
            halley_config::Vrr::Off,
            output_mode(2560, 1440, 179_998),
            (0, 0),
            Transform::Normal,
            halley_config::Vrr::Off,
        );

        assert!(diff.mode_changed);
        assert!(!diff.size_changed);
        assert!(!diff.offset_changed);
        assert!(!diff.transform_changed);
    }

    #[test]
    fn quarter_turn_changes_a_rectangular_output_footprint() {
        let diff = output_diff(
            output_mode(1920, 1200, 60_000),
            (0, 0),
            Transform::Normal,
            halley_config::Vrr::Off,
            output_mode(1920, 1200, 60_000),
            (0, 0),
            Transform::_90,
            halley_config::Vrr::Off,
        );

        assert!(!diff.mode_changed);
        assert!(diff.size_changed);
        assert!(diff.transform_changed);
    }

    #[test]
    fn configured_refresh_matches_exact_integer_millihertz_only() {
        let modes = [
            (0, 2560, 1440, 179_997),
            (1, 2560, 1440, 179_998),
            (2, 2560, 1440, 180_000),
        ];

        assert_eq!(
            matching_mode_index(modes, 2560, 1440, Some(179.998)),
            Some(1)
        );
        assert_eq!(
            matching_mode_index(modes, 2560, 1440, Some(179.999)),
            None
        );
    }

    #[test]
    fn configured_refresh_uses_niri_style_millihertz_rounding() {
        let modes = [(0, 2560, 1440, 179_998)];

        assert_eq!(
            matching_mode_index(modes, 2560, 1440, Some(179.9984)),
            Some(0)
        );
        assert_eq!(
            matching_mode_index(modes, 2560, 1440, Some(179.9986)),
            None
        );
    }

    #[test]
    fn omitted_refresh_selects_highest_rate_at_exact_resolution() {
        let modes = [
            (0, 1920, 1080, 60_000),
            (1, 2560, 1440, 143_912),
            (2, 2560, 1440, 179_998),
        ];

        assert_eq!(matching_mode_index(modes, 2560, 1440, None), Some(2));
        assert_eq!(matching_mode_index(modes, 3840, 2160, None), None);
    }

    #[test]
    fn transform_from_degrees_maps_known_values_and_falls_back_to_normal() {
        assert_eq!(transform_from_degrees(0), Transform::Normal);
        assert_eq!(transform_from_degrees(90), Transform::_90);
        assert_eq!(transform_from_degrees(180), Transform::_180);
        assert_eq!(transform_from_degrees(270), Transform::_270);
        // parse_one_output already clamps anything else to 0, but this
        // function stays defensive on its own rather than trusting that.
        assert_eq!(transform_from_degrees(45), Transform::Normal);
    }

    #[test]
    fn cursor_position_is_local_to_the_output_that_contains_it() {
        let primary = Rectangle::new((0, 0).into(), (2560, 1440).into());
        let secondary = Rectangle::new((2560, 0).into(), (1920, 1200).into());

        assert_eq!(
            cursor_position_for_output(primary, (3000.0, 800.0)),
            None
        );
        assert_eq!(
            cursor_position_for_output(secondary, (3000.0, 800.0)),
            Some(Point::from((440.0, 800.0)))
        );
    }

    #[test]
    fn secondary_window_geometry_becomes_output_local() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());
        let camera_center = crate::camera::global_center(
            Point::from((1060.0, 550.0)),
            secondary,
        );
        let world_rect =
            Rectangle::<i32, Physical>::new((3520, 600).into(), (200, 100).into());

        assert_eq!(
            super::super::camera_rect(
                world_rect,
                camera_center,
                secondary.size.to_physical(1),
                0.5,
            ),
            Rectangle::new((910, 625).into(), (100, 50).into())
        );
    }

    #[test]
    fn cursor_position_uses_half_open_output_edges() {
        let secondary = Rectangle::new((2560, 0).into(), (1920, 1200).into());

        assert_eq!(
            cursor_position_for_output(secondary, (2560.0, 0.0)),
            Some(Point::from((0.0, 0.0)))
        );
        assert_eq!(
            cursor_position_for_output(secondary, (4480.0, 1199.0)),
            None
        );
    }
}
