use calloop::EventLoop;
use smithay::backend::drm::DrmEvent;
use smithay::backend::session::Event as SessionEvent;

// src/bin/*.rs binaries are separate crates from main.rs and can't import
// its modules directly - reuse the same source tree via #[path] rather than
// touching main.rs (out of scope for this plan) to share it.
#[path = "../backend/mod.rs"]
mod backend;

use backend::CLEAR_COLOR;
use backend::Renderable;
use backend::tty::TtyBackend;

fn main() {
    let (mut backend, session_notifier, drm_notifier) = match TtyBackend::new() {
        Ok(parts) => parts,
        // Expected nested under a host compositor (niri already holds
        // exclusive session control) - confirmed since step 3. Real success
        // needs a free VT.
        Err(err) => {
            println!("TtyBackend::new() failed: {err}");
            return;
        }
    };
    println!("TtyBackend constructed successfully");

    match backend.render(CLEAR_COLOR) {
        Ok(()) => println!("first render() succeeded"),
        Err(err) => println!("first render() failed (same caveat as initialize_output): {err}"),
    }

    let mut event_loop: EventLoop<TtyBackend> =
        EventLoop::try_new().expect("failed to create event loop");

    event_loop
        .handle()
        .insert_source(session_notifier, |event, _, backend| match event {
            SessionEvent::PauseSession => {
                println!("session event: pause");
                backend.pause();
            }
            SessionEvent::ActivateSession => {
                println!("session event: activate");
                match backend.resume() {
                    Ok(()) => {
                        if let Err(err) = backend.render(CLEAR_COLOR) {
                            println!("post-resume render failed: {err}");
                        }
                    }
                    Err(err) => println!("resume failed: {err}"),
                }
            }
        })
        .expect("failed to insert session notifier");

    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, backend| match event {
            DrmEvent::VBlank(crtc) => {
                if let Err(err) = backend.frame_submitted(crtc) {
                    println!("frame_submitted failed for {crtc:?}: {err}");
                    return;
                }
                if let Err(err) = backend.render(CLEAR_COLOR) {
                    println!("render failed: {err}");
                }
            }
            DrmEvent::Error(err) => println!("drm event: error {err:?}"),
        })
        .expect("failed to insert drm notifier");

    println!("dispatching until Ctrl-C - switch to this VT to see a solid color fill the screen");
    loop {
        event_loop
            .dispatch(None, &mut backend)
            .expect("event loop dispatch failed");
    }
}
