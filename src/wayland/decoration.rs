use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::wayland::shell::xdg::ToplevelSurface;

/// A toplevel just created a decoration object - default it to server-side.
/// This is the direction halley is headed (her own simple built-in
/// decorations), so it's the preference to advertise before any client asks
/// for something else. Clients that never create a decoration object at all
/// (kitty, alacritty - CSD-only, no negotiation) never reach this path; they
/// draw their own chrome regardless of what we do here.
pub fn new_decoration(toplevel: ToplevelSurface) {
    set_mode(&toplevel, Mode::ServerSide);
}

/// A client explicitly asked for a mode - honor exactly what it asked for
/// rather than overriding it with our own preference. Niri does the same,
/// specifically to avoid a known SDL2 bug where forcing a different mode
/// during window creation leaves the window permanently hidden
/// (https://github.com/libsdl-org/SDL/issues/8173).
pub fn request_mode(toplevel: ToplevelSurface, mode: Mode) {
    set_mode(&toplevel, mode);
}

/// A client unset its preference - fall back to our default.
pub fn unset_mode(toplevel: ToplevelSurface) {
    set_mode(&toplevel, Mode::ServerSide);
}

fn set_mode(toplevel: &ToplevelSurface, mode: Mode) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(mode);
    });
    toplevel.send_configure();
}
