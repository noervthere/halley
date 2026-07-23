use std::time::Duration;

use calloop::EventLoop;
use smithay::backend::drm::DrmEvent;
use smithay::backend::session::Event as SessionEvent;

// src/bin/*.rs binaries are separate crates from main.rs and can't import
// its modules directly - reuse the same source tree via #[path] rather than
// touching main.rs (out of scope for this plan) to share it.
#[path = "../backend/mod.rs"]
mod backend;

use backend::tty::TtyBackend;

fn main() {
    match TtyBackend::new() {
        Ok((_backend, session_notifier, drm_notifier)) => {
            println!("TtyBackend constructed successfully");

            let mut event_loop: EventLoop<()> =
                EventLoop::try_new().expect("failed to create event loop");

            event_loop
                .handle()
                .insert_source(session_notifier, |event, _, _| match event {
                    SessionEvent::PauseSession => println!("session event: pause"),
                    SessionEvent::ActivateSession => println!("session event: activate"),
                })
                .expect("failed to insert session notifier");

            event_loop
                .handle()
                .insert_source(drm_notifier, |event, _, _| match event {
                    DrmEvent::VBlank(crtc) => println!("drm event: vblank on {crtc:?}"),
                    DrmEvent::Error(err) => println!("drm event: error {err:?}"),
                })
                .expect("failed to insert drm notifier");

            println!("dispatching for 2 seconds...");
            event_loop
                .dispatch(Some(Duration::from_secs(2)), &mut ())
                .expect("event loop dispatch failed");
            println!("done");
        }
        // Expected nested under a host compositor (niri already holds
        // exclusive session control) - confirmed since step 3. Real success
        // needs a free VT.
        Err(err) => println!("TtyBackend::new() failed: {err}"),
    }
}
