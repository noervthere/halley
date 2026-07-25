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
use smithay::output::{Mode as OutputMode, Output, OutputModeSource, PhysicalProperties, Subpixel};
use smithay::reexports::drm::control::{Mode, ModeTypeFlags, connector, crtc};
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
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
    output: Output,
    drm_output: TtyDrmOutput,
    pending: bool,
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
    outputs: Vec<TtyOutput>,
}

/// One fully-initialized output's live handle plus the VRR mode it was
/// configured with. `Output` itself has no VRR concept, and real VRR
/// toggling isn't wired yet (see `TtyBackend::new`'s doc comment on why) -
/// this is tracked separately purely so IPC can report what was requested.
pub struct TtyOutput {
    pub output: Output,
    pub vrr: halley_config::Vrr,
}

/// `{interface}-{interface_id}`, e.g. "DP-1" - the standard connector-name
/// convention (matches sway/niri/wlroots), used to match a connector against
/// a configured `output:` block by its `name` field.
fn connector_name(connector: &connector::Info) -> String {
    format!("{}-{}", connector.interface().as_str(), connector.interface_id())
}

/// The real, precise refresh rate of a mode - `Mode::vrefresh()` only
/// returns the kernel's rounded integer Hz (e.g. `60`), not a value with
/// enough precision to exactly match a configured rate like `179.998`. This
/// is the same clock/htotal/vtotal calculation niri and wlroots use.
/// Doesn't special-case interlaced modes (a rare edge case, not worth the
/// complexity here).
fn exact_refresh_hz(mode: &Mode) -> f64 {
    let (_, _, htotal) = mode.hsync();
    let (_, _, vtotal) = mode.vsync();
    let htotal = (htotal.max(1)) as f64;
    let vtotal = (vtotal.max(1)) as f64;
    (mode.clock() as f64 * 1000.0) / (htotal * vtotal)
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

    let matched = connector.modes().iter().find(|mode| {
        let (w, h) = mode.size();
        w as i32 == cfg.width
            && h as i32 == cfg.height
            && cfg.rate.is_none_or(|hz| (exact_refresh_hz(mode) - hz).abs() < 0.001)
    });

    match matched {
        Some(mode) => *mode,
        None => {
            let available = connector
                .modes()
                .iter()
                .map(|mode| {
                    let (w, h) = mode.size();
                    format!("{w}x{h}@{:.3}", exact_refresh_hz(mode))
                })
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
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

impl TtyBackend {
    /// Opens the seat, the primary GPU, and the first connected
    /// connector/CRTC/mode found on it. Notifiers are returned rather than
    /// owned by `TtyBackend` - whatever drives the event loop inserts them,
    /// exactly like `session::winit` owns `winit_source` today rather than
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
        // Mode selection happens later, per-connector, once config is
        // loaded (see `select_mode`) - not here, so it can take a
        // configured `output:` block into account.
        let mut scanner: DrmScanner = DrmScanner::new();
        let scan = scanner.scan_connectors(&drm)?;
        let mut connected: Vec<(connector::Info, crtc::Handle)> = Vec::new();
        for event in scan {
            if let DrmScanEvent::Connected { connector, crtc } = event
                && let Some(crtc) = crtc
                && !connector.modes().is_empty()
            {
                connected.push((connector, crtc));
            }
        }
        if connected.is_empty() {
            return Err("no connected connector/CRTC/mode found".into());
        }

        let outputs_config = halley_config::load_outputs();

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
        for (connector, crtc) in connected {
            let name = connector_name(&connector);
            let configured = outputs_config.iter().find(|cfg| cfg.name == name);
            let mode = select_mode(&connector, configured);
            let (width, height) = mode.size();
            let size = Size::from((width as i32, height as i32));
            // DRM scanout stays `Transform::Normal` regardless of a
            // configured `transform` - real hardware-plane rotation is a
            // separate, much larger undertaking (GPU/plane-dependent); only
            // the wl_output-facing transform below reflects config, matching
            // old halley's own actual behavior (confirmed by reading its
            // `backend/tty/drm.rs` - it has this exact same split, not a
            // regression introduced here).
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
                    &[connector.handle()],
                    output_mode_source,
                    None,
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                );

            match result {
                Ok(drm_output) => {
                    let offset = configured.map_or((0, 0), |cfg| (cfg.offset_x, cfg.offset_y));
                    let transform = configured.map_or(Transform::Normal, |cfg| transform_from_degrees(cfg.transform));
                    let vrr = configured.map(|cfg| cfg.vrr).unwrap_or_default();
                    if vrr == halley_config::Vrr::On {
                        eprintln!(
                            "output {name:?}: vrr \"on\" is configured but not wired to real hardware VRR yet \
                             (needs lower-level DRM compositor access this backend doesn't have) - ignored for now"
                        );
                    }

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
                    let output_mode = OutputMode {
                        size,
                        refresh: (exact_refresh_hz(&mode) * 1000.0).round() as i32,
                    };
                    output.change_current_state(Some(output_mode), Some(transform), None, Some(offset.into()));
                    output.set_preferred(output_mode);

                    primary_output.get_or_insert_with(|| output.clone());
                    outputs.push(TtyOutput {
                        output: output.clone(),
                        vrr,
                    });
                    drm_outputs.push(DrmOutputEntry {
                        crtc,
                        output,
                        drm_output,
                        pending: false,
                    });
                }
                Err(err) => eprintln!("failed to initialize output {name:?}: {err}"),
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
        self.outputs.iter().map(|entry| &entry.output)
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
        self.outputs
            .iter()
            .map(|entry| {
                let location = entry.output.current_location();
                halley_ipc::OutputInfo {
                    name: entry.output.name(),
                    current_mode: crate::ipc::mode_info(entry.output.current_mode()),
                    offset_x: location.x,
                    offset_y: location.y,
                    vrr: crate::ipc::vrr_str(entry.vrr).to_string(),
                }
            })
            .collect()
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
            let output_camera_center = camera_center_for_output(
                view.center,
                output_geometry,
            );
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
                    let dst = super::camera_rect(
                        native_geo,
                        output_camera_center,
                        output_size,
                        zoom_scale,
                    );
                    TtyRenderElement::Rescaled(super::rescale::RescaledElement::new(
                        surface_element,
                        dst,
                    ))
                }));
                if animated_bbox.size.w == 0 || animated_bbox.size.h == 0 {
                    continue;
                }
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
                let border_width = ((decorations.border_width_px as f64
                    * zoom_scale as f64
                    * opening_progress)
                    .round() as i32)
                    .max(1);
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

fn camera_center_for_output(
    local_camera_center: Point<f32, Physical>,
    output_geometry: Rectangle<i32, Logical>,
) -> Point<f32, Physical> {
    let local_output_center = Point::<f32, Physical>::from((
        output_geometry.size.w as f32 / 2.0,
        output_geometry.size.h as f32 / 2.0,
    ));
    let pan = local_camera_center - local_output_center;
    Point::from((
        output_geometry.loc.x as f32 + output_geometry.size.w as f32 / 2.0 + pan.x,
        output_geometry.loc.y as f32 + output_geometry.size.h as f32 / 2.0 + pan.y,
    ))
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
    fn secondary_camera_rebases_its_local_pan_into_global_space() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());

        assert_eq!(
            camera_center_for_output(
                Point::from((960.0, 600.0)),
                secondary,
            ),
            Point::from((3520.0, 600.0))
        );
        assert_eq!(
            camera_center_for_output(
                Point::from((1060.0, 550.0)),
                secondary,
            ),
            Point::from((3620.0, 550.0))
        );
    }

    #[test]
    fn secondary_window_geometry_becomes_output_local() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());
        let camera_center = camera_center_for_output(
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
