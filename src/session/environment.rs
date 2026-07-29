use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

const SESSION_VARIABLES: [&str; 6] = [
    "WAYLAND_DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_TYPE",
    "DESKTOP_SESSION",
    "PATH",
];

/// Effective environment overrides for every process launched by Halley.
///
/// Reloads intentionally merge rather than replace: old Halley applied
/// variables to its process environment, so removing a key from a live
/// config did not unset it until the compositor restarted.
#[derive(Clone, Debug, Default)]
pub(super) struct LaunchEnvironment {
    values: BTreeMap<String, String>,
}

impl LaunchEnvironment {
    pub fn new(values: &BTreeMap<String, String>) -> Self {
        Self {
            values: values.clone(),
        }
    }

    pub fn reload(&mut self, values: &BTreeMap<String, String>) {
        self.values.extend(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }

    pub fn apply_to(&self, command: &mut Command) {
        command.envs(&self.values);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.values
            .iter()
            .map(|(key, value)| (OsStr::new(key), OsStr::new(value)))
    }

    pub fn path(&self) -> Option<OsString> {
        self.values
            .get("PATH")
            .map(OsString::from)
            .or_else(|| std::env::var_os("PATH"))
    }
}

/// Establishes the environment that selects the real session backend.
///
/// This runs before any worker threads exist. Removing inherited display
/// variables is what prevents a display-manager environment from making a
/// full login accidentally choose the nested backend.
pub fn prepare_session() {
    unsafe {
        std::env::set_var("XDG_CURRENT_DESKTOP", "Halley");
        std::env::set_var("XDG_SESSION_DESKTOP", "Halley");
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        std::env::set_var("DESKTOP_SESSION", "Halley");
        std::env::remove_var("DISPLAY");
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("WAYLAND_SOCKET");
    }
}

/// Publishes the compositor socket only after it exists, then refreshes the
/// portal frontend so D-Bus activation sees the complete Halley environment.
pub fn activate_session(wayland_display: &OsStr, cursor: &halley_config::Cursor) {
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", wayland_display);
    }

    let mut dbus = Command::new("dbus-update-activation-environment");
    if systemd_is_booted() {
        dbus.arg("--systemd");
    }
    dbus.args(SESSION_VARIABLES);
    run("dbus activation environment", &mut dbus);

    if systemd_is_booted() {
        let mut import = Command::new("systemctl");
        import
            .arg("--user")
            .arg("import-environment")
            .args(SESSION_VARIABLES);
        run("systemd user environment", &mut import);

        // A portal backend may have exhausted its start limit while the
        // compositor socket was unavailable. Clear that stale failure after
        // publishing the complete environment so the frontend can activate
        // it immediately.
        let mut reset_failed = Command::new("systemctl");
        reset_failed
            .arg("--user")
            .arg("reset-failed")
            .arg("xdg-desktop-portal-gtk.service")
            .arg("xdg-desktop-portal-halley.service");
        run("portal failure reset", &mut reset_failed);

        let mut restart = Command::new("systemctl");
        restart
            .arg("--user")
            .arg("restart")
            .arg("--no-block")
            .arg("xdg-desktop-portal.service");
        run("portal refresh", &mut restart);
    }
    publish_cursor(cursor);
}

/// Updates activation environments for applications started after a cursor
/// config reload. Existing processes retain their launch environment.
pub fn publish_cursor(cursor: &halley_config::Cursor) {
    let assignments = cursor_assignments(cursor);
    let mut dbus = Command::new("dbus-update-activation-environment");
    if systemd_is_booted() {
        dbus.arg("--systemd");
    }
    dbus.args(&assignments);
    run("cursor D-Bus environment", &mut dbus);

    if systemd_is_booted() {
        let mut systemd = Command::new("systemctl");
        systemd
            .arg("--user")
            .arg("set-environment")
            .args(&assignments);
        run("cursor systemd environment", &mut systemd);
    }
}

pub fn activate_xwayland(display: &OsStr) {
    let mut assignment = OsString::from("DISPLAY=");
    assignment.push(display);

    let mut dbus = Command::new("dbus-update-activation-environment");
    if systemd_is_booted() {
        dbus.arg("--systemd");
    }
    dbus.arg(&assignment);
    run("XWayland D-Bus environment", &mut dbus);

    if systemd_is_booted() {
        let mut systemd = Command::new("systemctl");
        systemd
            .arg("--user")
            .arg("set-environment")
            .arg(&assignment);
        run("XWayland systemd environment", &mut systemd);
    }
}

fn systemd_is_booted() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

fn run(label: &str, command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match command.status() {
        Ok(status) if status.success() => eventline::debug!("{label}: complete"),
        Ok(status) => eventline::warn!("{label}: exited with {status}"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eventline::debug!("{label}: helper unavailable")
        }
        Err(err) => eventline::warn!("{label}: {err}"),
    }
}

fn cursor_assignments(cursor: &halley_config::Cursor) -> [OsString; 2] {
    [
        OsString::from(format!("XCURSOR_THEME={}", cursor.theme)),
        OsString::from(format!("XCURSOR_SIZE={}", cursor.size)),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::process::Command;

    use super::{LaunchEnvironment, SESSION_VARIABLES, cursor_assignments};

    #[test]
    fn activation_exports_portal_and_wayland_identity() {
        for required in ["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"] {
            assert!(SESSION_VARIABLES.contains(&required));
        }
    }

    #[test]
    fn cursor_assignments_use_the_validated_runtime_config() {
        let cursor = halley_config::Cursor {
            theme: "Breeze".to_string(),
            size: 32,
            ..halley_config::Cursor::default()
        };
        assert_eq!(
            cursor_assignments(&cursor),
            [
                std::ffi::OsString::from("XCURSOR_THEME=Breeze"),
                std::ffi::OsString::from("XCURSOR_SIZE=32"),
            ]
        );
    }

    #[test]
    fn launch_environment_keeps_live_reload_values_until_restart() {
        let mut environment =
            LaunchEnvironment::new(&BTreeMap::from([("FIRST".to_string(), "one".to_string())]));
        environment.reload(&BTreeMap::from([("SECOND".to_string(), "two".to_string())]));

        let mut command = Command::new("true");
        environment.apply_to(&mut command);
        let env = command.get_envs().collect::<BTreeMap<_, _>>();
        assert_eq!(env.get(OsStr::new("FIRST")), Some(&Some(OsStr::new("one"))));
        assert_eq!(
            env.get(OsStr::new("SECOND")),
            Some(&Some(OsStr::new("two")))
        );
        assert_eq!(
            environment.iter().collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                (OsStr::new("FIRST"), OsStr::new("one")),
                (OsStr::new("SECOND"), OsStr::new("two")),
            ])
        );
    }

    #[test]
    fn configured_path_overrides_the_ambient_path() {
        let environment = LaunchEnvironment::new(&BTreeMap::from([(
            "PATH".to_string(),
            "/configured/bin".to_string(),
        )]));

        assert_eq!(
            environment.path().as_deref(),
            Some(OsStr::new("/configured/bin"))
        );
    }
}
