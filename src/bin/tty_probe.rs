use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;

fn main() {
    // Session creation needs exclusive control of the seat via logind/seatd -
    // expected to fail cleanly (not panic) while a host compositor (niri)
    // already holds that control. A free VT is required for a real pass.
    match LibSeatSession::new() {
        Ok((session, _notifier)) => {
            println!("seat: {}", session.seat());
            println!("active: {}", session.is_active());
        }
        Err(err) => {
            println!("session creation failed (expected if a host compositor holds the seat): {err}");
        }
    }
}
