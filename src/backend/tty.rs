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
use smithay::backend::renderer::element::{AsRenderElements, Element, Kind, render_elements};
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
use smithay::utils::{DeviceFd, Physical, Point, Scale, Size, Transform};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::Renderable;
use crate::cursor::CursorImage;

type TtyDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

type TtyDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

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
    drm_output: TtyDrmOutput,
    pending: bool,
}

render_elements! {
    /// Combines mapped-window and cursor elements into one homogeneous list -
    /// `DrmOutput::render_frame` takes a single element type per call, unlike
    /// the winit backend's `Frame`, which can be drawn into with several
    /// separate calls. Concrete over `GlesRenderer` (not generic over `R`) -
    /// this codebase only ever has one renderer, and `RescaledElement`
    /// itself is only implemented for `GlesRenderer`, so the old `<R>`
    /// generic bought nothing.
    TtyRenderElement<=GlesRenderer>;
    Rescaled=super::rescale::RescaledElement,
    Border=SolidColorRenderElement,
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
    /// The first successfully initialized output's size - used only to
    /// clamp the single shared pointer position (see `Pointer`'s doc
    /// comment on the multi-output simplification). Not per-output layout;
    /// real per-output pointer coordinate spaces are future multi-monitor
    /// work.
    output_size: Size<i32, Physical>,
    /// The CRTC of the first successfully initialized output - the only one
    /// the cursor is drawn on. There's no per-output coordinate layout yet
    /// (see `output_size`), so a single shared cursor position can't be
    /// meaningfully placed on every monitor at once; showing it on all of
    /// them made it look duplicated rather than tracking one pointer.
    primary_crtc: crtc::Handle,
    /// The `wl_output` for the primary output - client windows are only
    /// ever drawn there this round (see `render()`), matching the same
    /// "first output" simplification `output_size`/`primary_crtc` already
    /// establish rather than inventing a new one.
    primary_output: Output,
    /// Every successfully initialized output, in connector-scan order - each
    /// one now gets its own real name, configured mode/position/transform
    /// (see `TtyBackend::new`'s per-connector loop). Purely informational
    /// until multi-output window placement exists (`halleyctl outputs` and
    /// future per-output work read this); rendering still only ever targets
    /// `primary_output`.
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
        let mut output_size = None;
        let mut primary_crtc = None;
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
                    output_size.get_or_insert(size);
                    primary_crtc.get_or_insert(crtc);

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
                    outputs.push(TtyOutput { output, vrr });
                    drm_outputs.push(DrmOutputEntry {
                        crtc,
                        drm_output,
                        pending: false,
                    });
                }
                Err(err) => eprintln!("failed to initialize output {name:?}: {err}"),
            }
        }

        let Some(output_size) = output_size else {
            return Err("no output could be initialized".into());
        };
        let primary_crtc = primary_crtc.expect("set alongside output_size above");
        let primary_output = primary_output.expect("set alongside output_size above");

        let backend = TtyBackend {
            session,
            renderer,
            drm_output_manager,
            drm_outputs,
            output_size,
            primary_crtc,
            primary_output,
            outputs,
        };

        Ok((backend, session_notifier, drm_notifier))
    }

    /// The size used to clamp the shared pointer position - see the
    /// `output_size` field's doc comment for the multi-output caveat.
    pub fn output_size(&self) -> Size<i32, Physical> {
        self.output_size
    }

    /// The `wl_output` driving code registers as a global and maps into a
    /// `Space` - see `primary_output`'s doc comment for the single-output
    /// caveat this shares with `output_size`/`primary_crtc`.
    pub fn primary_output(&self) -> &Output {
        &self.primary_output
    }

    /// The CRTC driving code's redraw scheduling should key off of - only
    /// the primary output ever has genuinely dynamic content (windows, the
    /// cursor), so it's the only one whose VBlank cadence needs to drive a
    /// redraw-request state machine at all.
    pub fn primary_crtc(&self) -> crtc::Handle {
        self.primary_crtc
    }

    /// Whether the primary output currently has a frame queued and awaiting
    /// its VBlank - lets driving code's redraw scheduling (see
    /// `session::tty`'s `RedrawState`) tell whether the last `render()` call
    /// actually submitted anything to DRM, or produced no damage.
    pub fn primary_frame_in_flight(&self) -> bool {
        self.drm_outputs
            .iter()
            .find(|entry| entry.crtc == self.primary_crtc)
            .is_some_and(|entry| entry.pending)
    }

    /// The primary output's refresh interval, used to time the estimated-
    /// VBlank fallback timer (see `session::tty`) - falls back to 60Hz if
    /// the mode's refresh rate is somehow unset.
    pub fn refresh_interval(&self) -> Duration {
        let refresh_mhz = self
            .primary_output
            .current_mode()
            .map(|mode| mode.refresh)
            .filter(|&refresh| refresh > 0)
            .unwrap_or(60_000);
        Duration::from_secs_f64(1000.0 / refresh_mhz as f64)
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
    pub fn frame_submitted(&mut self, crtc: crtc::Handle) -> Result<(), Box<dyn Error>> {
        if let Some(entry) = self.drm_outputs.iter_mut().find(|e| e.crtc == crtc) {
            entry.drm_output.frame_submitted()?;
            entry.pending = false;
        }
        Ok(())
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
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
        space: &Space<Window>,
        focused: Option<&WlSurface>,
        decorations: &halley_config::Decorations,
        camera_center: Point<f32, Physical>,
        zoom_scale: f32,
    ) -> Result<(), Box<dyn Error>> {
        // A single bad output shouldn't hide a working one - failures are
        // logged per-output, and only surfaced to the caller if literally
        // every output failed.
        let mut ok_count = 0;
        let mut last_err: Option<Box<dyn Error>> = None;
        let output_size = self.output_size();

        for entry in &mut self.drm_outputs {
            // A page flip is already queued for this output and hasn't
            // landed yet - DRM won't accept a second commit before that
            // happens. Nothing is lost: `frame_submitted()` clears `pending`
            // on the next VBlank, and the driving code always re-renders
            // right after, picking up whatever changed in the meantime.
            if entry.pending {
                continue;
            }
            let crtc = entry.crtc;
            let drm_output = &mut entry.drm_output;

            // Only the primary output draws windows and the cursor - there's
            // no per-output coordinate/layout system yet (see
            // `primary_crtc`'s doc comment), so a single shared pointer
            // position and one `Space` can't be meaningfully split across
            // monitors. Secondary outputs keep rendering clear-color only.
            let mut elements: Vec<TtyRenderElement> = Vec::new();
            if crtc == self.primary_crtc {
                // Built directly per mapped window rather than via
                // `space_render_elements` - nesting `SpaceRenderElements`
                // inside this file's own combined element enum runs into an
                // internal bound mismatch in the render_elements! macro
                // (SpaceRenderElements's own generated impl wants
                // `ImportMemWl`/`ImportDmaWl`, which its own declared
                // `where` clause doesn't actually list); going straight to
                // `Window`'s `AsRenderElements` avoids nesting the macro
                // output of one render_elements! invocation inside another.
                //
                // `.rev()` is load-bearing: `Space::elements()` iterates
                // z-order *back to front* (bottom-most first), but a render
                // element list is front-to-back by convention -
                // `draw_render_elements` reverses whatever it's given before
                // drawing, so feeding it bottom-to-front order inverts
                // z-order on screen and raising a window would push it
                // visually to the back. Smithay's own
                // `Space::render_elements_for_region` calls `.rev()` here for
                // exactly this reason.
                for window in space.elements().rev() {
                    let Some(geometry) = space.element_geometry(window) else {
                        continue;
                    };
                    let scaled_bbox =
                        super::camera_rect(geometry.to_physical(1), camera_center, output_size, zoom_scale);

                    // Rendered at native scale/position first, then each
                    // returned element is individually wrapped to draw into
                    // a `camera_rect`-scaled destination - see `RescaledElement`'s
                    // doc comment for why passing `zoom_scale` directly here
                    // doesn't work (it did move the anchor point, which is
                    // what looked like "drift": a full-size texture anchored
                    // at a shrinking point looks like it's sliding toward a
                    // corner instead of shrinking in place).
                    let surface_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = window
                        .render_elements(&mut self.renderer, geometry.to_physical(1).loc, Scale::from(1.0), 1.0);
                    elements.extend(surface_elements.into_iter().map(|surface_element| {
                        let native_geo = surface_element.geometry(Scale::from(1.0));
                        let dst = super::camera_rect(native_geo, camera_center, output_size, zoom_scale);
                        TtyRenderElement::Rescaled(super::rescale::RescaledElement::new(surface_element, dst))
                    }));

                    let is_focused = window
                        .toplevel()
                        .is_some_and(|t| Some(t.wl_surface()) == focused);
                    let color = super::window_border_color(decorations, is_focused);
                    let border_width = ((decorations.border_width_px as f32 * zoom_scale).round() as i32).max(1);
                    elements.extend(
                        super::border_strips(scaled_bbox, border_width, color)
                            .into_iter()
                            .map(TtyRenderElement::Border),
                    );
                }

                // Built before render_frame() borrows the renderer again -
                // from_buffer() only needs it transiently to import the texture.
                match MemoryRenderBufferRenderElement::from_buffer(
                    &mut self.renderer,
                    Point::<f64, Physical>::from(cursor_position),
                    &cursor.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    // Inserted at the *front*, not pushed: this list is
                    // front-to-back, so index 0 is the topmost element. The
                    // cursor has to composite over every window, and unlike
                    // the winit backend there's no second draw call to put it
                    // in - `DrmOutput::render_frame` takes one list.
                    Ok(element) => elements.insert(0, TtyRenderElement::Cursor(element)),
                    Err(err) => {
                        eprintln!("failed to build cursor element for {crtc:?}: {err}");
                        last_err = Some(Box::new(err));
                        continue;
                    }
                }
            }

            match drm_output.render_frame::<_, TtyRenderElement>(
                &mut self.renderer,
                &elements,
                clear,
                FrameFlags::empty(),
            ) {
                Ok(result) => {
                    ok_count += 1;
                    if !result.is_empty {
                        match drm_output.queue_frame(()) {
                            Ok(()) => entry.pending = true,
                            Err(err) => eprintln!("queue_frame failed for {crtc:?}: {err}"),
                        }
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
}
