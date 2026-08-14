use std::ffi::{OsStr, OsString};
use std::process::Child;
use std::time::{Duration, Instant};

use super::environment::LaunchEnvironment;

const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

enum OnceState {
    Unarmed,
    Pending(Vec<String>),
    Finished,
}

/// Owns the complete lifecycle of commands started from `autostart:`.
///
/// Keybind launches remain intentionally detached. Autostart processes are
/// session services, so their process groups are retained, reaped, and
/// stopped when Halley exits.
pub(super) struct Autostart {
    enabled: bool,
    once: OnceState,
    wayland_display: Option<OsString>,
    children: Vec<Child>,
}

impl Autostart {
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            once: OnceState::Unarmed,
            wayland_display: None,
            children: Vec::new(),
        }
    }

    #[cfg_attr(not(feature = "winit"), allow(dead_code))]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            once: OnceState::Finished,
            wayland_display: None,
            children: Vec::new(),
        }
    }

    pub fn arm_once(&mut self, wayland_display: &OsStr, commands: Vec<String>) {
        if !self.enabled || !matches!(self.once, OnceState::Unarmed) {
            return;
        }
        self.wayland_display = Some(wayland_display.to_os_string());
        self.once = OnceState::Pending(commands);
    }

    pub fn run_once(
        &mut self,
        x11_display: Option<&OsStr>,
        cursor_size: u8,
        environment: &LaunchEnvironment,
    ) {
        let OnceState::Pending(commands) = std::mem::replace(&mut self.once, OnceState::Finished)
        else {
            return;
        };
        self.run_commands(&commands, x11_display, cursor_size, environment);
    }

    pub fn run_reload(
        &mut self,
        commands: &[String],
        x11_display: Option<&OsStr>,
        cursor_size: u8,
        environment: &LaunchEnvironment,
    ) {
        if self.enabled {
            self.run_commands(commands, x11_display, cursor_size, environment);
        }
    }

    pub fn reap_finished(&mut self) {
        self.children.retain_mut(|child| match child.try_wait() {
            Ok(Some(status)) => {
                eventline::debug!("autostart: reaped pid={} status={status}", child.id());
                false
            }
            Ok(None) => true,
            Err(err) => {
                eventline::warn!("autostart: failed to inspect pid={}: {err}", child.id());
                false
            }
        });
    }

    fn run_commands(
        &mut self,
        commands: &[String],
        x11_display: Option<&OsStr>,
        cursor_size: u8,
        environment: &LaunchEnvironment,
    ) {
        self.reap_finished();
        let Some(wayland_display) = self.wayland_display.as_deref() else {
            return;
        };
        for command in commands {
            let command = command.trim();
            if command.is_empty() {
                continue;
            }
            let mut process = super::spawn::managed_process(
                command,
                wayland_display,
                x11_display,
                cursor_size,
                environment,
            );
            match process.spawn() {
                Ok(child) => {
                    eventline::debug!(
                        "autostart: launched {command:?} (pid={}, WAYLAND_DISPLAY={wayland_display:?}, DISPLAY={x11_display:?})",
                        child.id()
                    );
                    self.children.push(child);
                }
                Err(err) => eventline::warn!("autostart: failed to launch {command:?}: {err}"),
            }
        }
    }
}

impl Drop for Autostart {
    fn drop(&mut self) {
        self.reap_finished();
        for child in &mut self.children {
            terminate_process_group(child);
        }
    }
}

fn terminate_process_group(child: &mut Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // Negative pid targets the process group created by managed_process.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    let mut child_reaped = false;
    loop {
        if !child_reaped {
            match child.try_wait() {
                Ok(Some(_)) => child_reaped = true,
                Ok(None) => {}
                Err(_) => return,
            }
        }
        if !process_group_exists(pid) {
            if !child_reaped {
                let _ = child.wait();
            }
            return;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            if !child_reaped {
                let _ = child.wait();
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_group_exists(pid: i32) -> bool {
    if unsafe { libc::kill(-pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> (LaunchEnvironment, OsString) {
        (LaunchEnvironment::default(), OsString::from("wayland-9"))
    }

    #[test]
    fn once_can_only_be_armed_and_run_once() {
        let (environment, display) = context();
        let mut autostart = Autostart::enabled();
        autostart.arm_once(&display, Vec::new());
        autostart.arm_once(&display, vec!["sleep 30".to_string()]);
        autostart.run_once(None, 24, &environment);
        autostart.run_once(None, 24, &environment);

        assert!(matches!(autostart.once, OnceState::Finished));
        assert!(autostart.children.is_empty());
    }

    #[test]
    fn nested_policy_suppresses_once_and_reload_commands() {
        let (environment, display) = context();
        let mut autostart = Autostart::disabled();
        autostart.arm_once(&display, vec!["sleep 30".to_string()]);
        autostart.run_once(None, 24, &environment);
        autostart.run_reload(&["sleep 30".to_string()], None, 24, &environment);

        assert!(autostart.children.is_empty());
    }

    #[test]
    fn completed_command_is_reaped_without_blocking_later_commands() {
        let (environment, display) = context();
        let mut autostart = Autostart::enabled();
        autostart.arm_once(&display, vec!["exit 7".to_string(), "sleep 30".to_string()]);
        autostart.run_once(None, 24, &environment);
        std::thread::sleep(Duration::from_millis(30));
        autostart.reap_finished();

        assert_eq!(autostart.children.len(), 1);
    }

    #[test]
    fn reload_does_not_consume_once_and_drop_stops_the_process_group() {
        let (environment, display) = context();
        let mut autostart = Autostart::enabled();
        autostart.arm_once(&display, Vec::new());
        autostart.run_reload(&["sleep 30".to_string()], None, 24, &environment);
        let pid = i32::try_from(autostart.children[0].id()).unwrap();

        assert!(matches!(autostart.once, OnceState::Pending(_)));
        drop(autostart);
        assert!(!process_group_exists(pid));
    }
}
