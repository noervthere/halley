use std::time::Duration;

use calloop::EventLoop;
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev;

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

    if let Ok((session, notifier)) = session_result {
        println!("seat: {}", session.seat());
        println!("active: {}", session.is_active());

        let mut event_loop: EventLoop<()> = EventLoop::try_new().expect("failed to create event loop");
        event_loop
            .handle()
            .insert_source(notifier, |event, _, _| match event {
                SessionEvent::PauseSession => println!("session event: pause"),
                SessionEvent::ActivateSession => println!("session event: activate"),
            })
            .expect("failed to insert session notifier");

        println!("dispatching for 2 seconds (no session events expected nested)...");
        event_loop
            .dispatch(Some(Duration::from_secs(2)), &mut ())
            .expect("event loop dispatch failed");
    }

    match udev::all_gpus(&seat_name) {
        Ok(gpus) => {
            println!("gpus on seat {seat_name}: {gpus:?}");
        }
        Err(err) => {
            println!("gpu discovery failed: {err}");
        }
    }
}
