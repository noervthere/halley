use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Spawns a user-provided command line detached from the compositor, with
/// `WAYLAND_DISPLAY` set to Halley's socket rather than the ambient host.
///
/// The shell is intentional: loose keybinds may contain arguments, quoting,
/// pipelines, or substitutions. The config is user-authored and has the
/// same authority as starting a command from their own shell.
pub fn spawn_detached(command_line: &str, wayland_display: &OsStr) {
    let mut process = detached_process(command_line, wayland_display);

    match process.spawn() {
        Ok(mut child) => {
            // The immediate child exits after its own fork. The command's
            // shell is already reparented and will be reaped independently.
            match child.wait() {
                Ok(_) => eventline::debug!(
                    "spawn: launched {command_line:?} (WAYLAND_DISPLAY={wayland_display:?})"
                ),
                Err(err) => {
                    eventline::warn!("spawn: failed to reap intermediate process: {err}")
                }
            }
        }
        Err(err) => eventline::error!("spawn: failed to launch {command_line:?}: {err}"),
    }
}

fn detached_process(command_line: &str, wayland_display: &OsStr) -> Command {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command_line)
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

    process
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_use_a_shell_and_halley_display() {
        let process = detached_process(
            "grim -g \"$(slurp)\" ~/shot.png",
            OsStr::new("wayland-9"),
        );
        assert_eq!(process.get_program(), "sh");
        assert_eq!(
            process.get_args().collect::<Vec<_>>(),
            ["-c", "grim -g \"$(slurp)\" ~/shot.png"]
        );
        assert!(process.get_envs().any(|(name, value)| {
            name == "WAYLAND_DISPLAY" && value == Some(OsStr::new("wayland-9"))
        }));
    }
}
