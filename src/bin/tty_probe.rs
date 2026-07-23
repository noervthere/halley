use std::time::Duration;

use calloop::EventLoop;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent};
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::DeviceFd;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

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

    let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
    match session.open(gpu_path, flags) {
        Ok(fd) => {
            let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));
            match DrmDevice::new(drm_fd.clone(), false) {
                Ok((drm, drm_notifier)) => {
                    println!("opened DRM device on {gpu_path:?}, {} crtcs", drm.crtcs().len());

                    event_loop
                        .handle()
                        .insert_source(drm_notifier, |event, _, _| match event {
                            DrmEvent::VBlank(crtc) => println!("drm event: vblank on {crtc:?}"),
                            DrmEvent::Error(err) => println!("drm event: error {err:?}"),
                        })
                        .expect("failed to insert drm notifier");

                    let mut scanner: DrmScanner = DrmScanner::new();
                    match scanner.scan_connectors(&drm) {
                        Ok(scan) => {
                            for event in scan {
                                if let DrmScanEvent::Connected { connector, crtc } = event {
                                    let mode = connector.modes().first();
                                    println!(
                                        "connector {:?} ({:?}), crtc: {:?}, first mode: {:?}",
                                        connector.interface(),
                                        connector.state(),
                                        crtc,
                                        mode
                                    );
                                }
                            }
                        }
                        Err(err) => println!("scan_connectors failed: {err}"),
                    }

                    match GbmDevice::new(drm_fd.clone()) {
                        Ok(gbm) => {
                            let _allocator = GbmAllocator::new(
                                gbm,
                                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                            );
                            println!("gbm device + allocator constructed");
                        }
                        Err(err) => println!("GbmDevice::new failed: {err}"),
                    }
                }
                Err(err) => println!("DrmDevice::new failed: {err}"),
            }
        }
        Err(err) => println!("session.open({gpu_path:?}) failed: {err}"),
    }

    println!("dispatching for 2 seconds (no session/drm events expected nested)...");
    event_loop
        .dispatch(Some(Duration::from_secs(2)), &mut ())
        .expect("event loop dispatch failed");
    println!("done");
}
