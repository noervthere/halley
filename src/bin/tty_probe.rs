use std::time::Duration;

use calloop::EventLoop;
use smithay::backend::session::Event as SessionEvent;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;

fn main() {
    // Session creation needs exclusive control of the seat via logind/seatd -
    // expected to fail cleanly (not panic) while a host compositor (niri)
    // already holds that control. A free VT is required for a real pass.
    let (session, notifier) = match LibSeatSession::new() {
        Ok(pair) => pair,
        Err(err) => {
            println!("session creation failed (expected if a host compositor holds the seat): {err}");
            return;
        }
    };

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

    // No real VT switch will happen nested (niri owns the session), so no
    // events are expected here - this only proves the notifier's calloop
    // wiring compiles and dispatches without error.
    println!("dispatching for 2 seconds (no session events expected nested)...");
    event_loop
        .dispatch(Some(Duration::from_secs(2)), &mut ())
        .expect("event loop dispatch failed");
    println!("done");
}
