use std::time::Duration;

use calloop::EventLoop;
use smithay::backend::drm::DrmEvent;
use smithay::backend::renderer::Color32F;
use smithay::backend::session::Event as SessionEvent;

// src/bin/*.rs binaries are separate crates from main.rs and can't import
// its modules directly - reuse the same source tree via #[path] rather than
// touching main.rs (out of scope for this plan) to share it.
#[path = "../backend/mod.rs"]
mod backend;

use backend::Renderable;
use backend::tty::TtyBackend;

// Matches main.rs's CLEAR_COLOR - same call shape used for WinitBackend,
// proving the Renderable trait abstraction works identically across backends.
const CLEAR_COLOR: Color32F = Color32F::new(0.04, 0.05, 0.06, 1.0);

fn main() {
    match TtyBackend::new() {
        Ok((mut backend, session_notifier, drm_notifier)) => {
            println!("TtyBackend constructed successfully");

            match backend.render(CLEAR_COLOR) {
                Ok(()) => println!("render() succeeded"),
                Err(err) => println!("render() failed (same caveat as initialize_output): {err}"),
            }

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
