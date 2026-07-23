use std::error::Error;
use std::path::Path;
use std::time::Duration;

use calloop::EventLoop;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::exporter::gbm::NodeFilter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev;
use smithay::output::OutputModeSource;
use smithay::reexports::drm::control::Mode;
use smithay::reexports::drm::control::connector;
use smithay::reexports::drm::control::crtc;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Scale, Size, Transform};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

type ProbeDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

type ProbeDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// Everything steps 6-11 build: open the DRM device, scan for a connected
/// connector/CRTC/mode, set up GBM+EGL+GlesRenderer, construct a
/// `DrmOutputManager`, and initialize a `DrmOutput` for the first connected
/// connector/CRTC/mode found. Written with `?` rather than nested matches
/// purely for readability - this is still the throwaway probe binary,
/// consolidated into the real `TtyBackend` in step 12.
fn probe_drm(
    session: &mut LibSeatSession,
    gpu_path: &Path,
) -> Result<(DrmDeviceNotifier, Option<(GlesRenderer, ProbeDrmOutput)>), Box<dyn Error>> {
    let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
    let fd = session.open(gpu_path, flags)?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), false)?;
    println!("opened DRM device on {gpu_path:?}, {} crtcs", drm.crtcs().len());

    let mut scanner: DrmScanner = DrmScanner::new();
    let scan = scanner.scan_connectors(&drm)?;
    let mut first_connected: Option<(connector::Handle, crtc::Handle, Mode)> = None;
    for event in scan {
        if let DrmScanEvent::Connected { connector, crtc } = event {
            let mode = connector.modes().first().copied();
            println!(
                "connector {:?} ({:?}), crtc: {:?}, first mode: {:?}",
                connector.interface(),
                connector.state(),
                crtc,
                mode
            );
            if let (Some(crtc), Some(mode)) = (crtc, mode) {
                first_connected.get_or_insert((connector.handle(), crtc, mode));
            }
        }
    }

    let gbm = GbmDevice::new(drm_fd)?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    println!("gbm device + allocator constructed");

    let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_context = EGLContext::new(&egl_display)?;
    let mut renderer = unsafe { GlesRenderer::new(egl_context)? };
    let renderer_formats: Vec<Format> = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect();
    println!(
        "gles renderer constructed, {} dmabuf formats",
        renderer_formats.len()
    );

    let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::All);
    let mut mgr: ProbeDrmOutputManager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        [Fourcc::Argb8888],
        renderer_formats,
    );
    println!("DrmOutputManager constructed");

    let Some((connector, crtc, mode)) = first_connected else {
        println!("no connected connector/CRTC/mode found - skipping initialize_output");
        return Ok((drm_notifier, None));
    };

    let (width, height) = mode.size();
    let output_mode_source = OutputModeSource::Static {
        size: Size::from((width as i32, height as i32)),
        scale: Scale::from(1.0),
        transform: Transform::Normal,
    };

    // Uncertain nested: this is the first call that may need DRM master via
    // an atomic TEST_ONLY commit. A clean typed Err here (rather than a
    // panic) is an acceptable, expected outcome nested - only a real free VT
    // confirms Ok.
    let result = mgr.lock().initialize_output::<GlesRenderer, SolidColorRenderElement>(
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
            println!("initialize_output succeeded");
            Ok((drm_notifier, Some((renderer, drm_output))))
        }
        Err(err) => {
            println!("initialize_output failed (expected nested under a host compositor): {err}");
            Ok((drm_notifier, None))
        }
    }
}

fn main() {
    // Session creation needs exclusive control of the seat via logind/seatd -
    // expected to fail cleanly (not panic) while a host compositor (niri)
    // already holds that control. A free VT is required for a real pass.
    let session_result = LibSeatSession::new();

    // GPU discovery only needs a seat *name* (pure udev enumeration, no
    // session fd) - fall back to the env var a live session would otherwise
    // report, so this step isn't blocked by session creation failing nested.
    let seat_name = match &session_result {
        Ok((session, _)) => session.seat(),
        Err(err) => {
            println!("session creation failed (expected if a host compositor holds the seat): {err}");
            std::env::var("XDG_SEAT").unwrap_or_else(|_| "seat0".to_string())
        }
    };

    let gpus = match udev::all_gpus(&seat_name) {
        Ok(gpus) => {
            println!("gpus on seat {seat_name}: {gpus:?}");
            gpus
        }
        Err(err) => {
            println!("gpu discovery failed: {err}");
            Vec::new()
        }
    };

    let Ok((mut session, notifier)) = session_result else {
        println!("skipping DRM device open: no live session");
        return;
    };

    let mut event_loop: EventLoop<()> = EventLoop::try_new().expect("failed to create event loop");
    event_loop
        .handle()
        .insert_source(notifier, |event, _, _| match event {
            SessionEvent::PauseSession => println!("session event: pause"),
            SessionEvent::ActivateSession => println!("session event: activate"),
        })
        .expect("failed to insert session notifier");

    let Some(gpu_path) = gpus.first() else {
        println!("skipping DRM device open: no GPU found");
        return;
    };

    match probe_drm(&mut session, gpu_path) {
        Ok((drm_notifier, output)) => {
            event_loop
                .handle()
                .insert_source(drm_notifier, |event, _, _| match event {
                    DrmEvent::VBlank(crtc) => println!("drm event: vblank on {crtc:?}"),
                    DrmEvent::Error(err) => println!("drm event: error {err:?}"),
                })
                .expect("failed to insert drm notifier");

            if output.is_some() {
                println!("drm output ready (renderer + DrmOutput constructed)");
            }
        }
        Err(err) => println!("probe_drm failed: {err}"),
    }

    println!("dispatching for 2 seconds (no session/drm events expected nested)...");
    event_loop
        .dispatch(Some(Duration::from_secs(2)), &mut ())
        .expect("event loop dispatch failed");
    println!("done");
}
