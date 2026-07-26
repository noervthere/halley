mod animation;
mod backend;
mod camera;
mod config;
mod cursor;
mod frame_clock;
mod input;
mod ipc;
mod logging;
mod spawn;
mod session;
mod wayland;

fn main() {
    logging::init();
    let force_winit = std::env::args().any(|arg| arg == "--winit");

    if force_winit || detect_nested_session() {
        session::winit::run();
    } else {
        session::tty::run();
    }
    logging::flush();
}

/// Whether we're already running inside another Wayland/X compositor - if
/// so, taking over real hardware (the tty/DRM session) would be wrong even
/// without `--winit`. Mirrors niri's own auto-detection (`Niri::init`,
/// reference-only): a `WAYLAND_DISPLAY` or `DISPLAY` already set in the
/// environment means a host compositor/X server is available to nest under.
fn detect_nested_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}
