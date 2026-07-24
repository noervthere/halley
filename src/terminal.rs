use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Spawns `command` detached from the compositor, with `WAYLAND_DISPLAY` set
/// so it connects to us rather than whatever the ambient environment
/// happens to have - matches sway/Hyprland/niri's own convention (niri's
/// `spawning.rs`: "Double-fork to avoid having to waitpid the child").
///
/// A plain `Command::spawn()` would make the compositor the parent of
/// whatever it launches for that process's entire lifetime: when it
/// eventually exits, it becomes a zombie until *something* reaps it -
/// nothing in this codebase ever does, so they'd just accumulate for as
/// long as the compositor runs. Double-forking sidesteps that without
/// needing a SIGCHLD handler: the direct child forks again and exits
/// immediately (reaped synchronously below, near-instantly, since it does
/// nothing but that one syscall), while the real target process is
/// reparented to init, which reaps it whenever it exits - we never need to
/// know or care.
pub fn spawn_detached(command: &str, wayland_display: &OsStr) {
    let mut process = Command::new(command);
    process
        .env("WAYLAND_DISPLAY", wayland_display)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Safety: only async-signal-safe calls between fork and exec - a raw
    // fork() plus an immediate _exit() (never std::process::exit, which
    // isn't safe to run again after a raw fork - it may re-run Rust's
    // normal shutdown machinery a second time).
    unsafe {
        process.pre_exec(|| match libc::fork() {
            -1 => Err(io::Error::last_os_error()),
            0 => Ok(()),
            _ => libc::_exit(0),
        });
    }

    match process.spawn() {
        Ok(mut child) => {
            // The immediate child (whose PID this `Child` refers to) exits
            // right after its own fork() above - this reaps *that*, not the
            // real target process, which is already independent by now.
            if let Err(err) = child.wait() {
                eprintln!("terminal: failed to reap intermediate process: {err}");
            }
        }
        Err(err) => eprintln!("terminal: failed to spawn {command:?}: {err}"),
    }
}
