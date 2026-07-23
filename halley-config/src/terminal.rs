use crate::keybinds::DefaultTerminal;

/// Terminal candidates for `DefaultTerminal::Auto`, in priority order.
///
/// Ordered by popularity, with one deliberate nudge: `alacritty` leads
/// (most popular terminal emulator generally, and - since this is a Rust
/// project - also written in Rust; that's a tie-breaker, not the reason on
/// its own), then `kitty`, then the rest. This is a real change from old
/// halley's hardcoded list (which led with `ghostty`) - not a port.
pub const TERMINAL_PRIORITY: &[&str] = &[
    "alacritty",
    "kitty",
    "ghostty",
    "wezterm",
    "foot",
    "footclient",
    "rio",
    "contour",
];

/// Resolve `setting` to an actual terminal command: the explicit command if
/// set, otherwise the first candidate from `TERMINAL_PRIORITY` for which
/// `is_available` returns true.
///
/// Takes the availability check as an injectable closure so the
/// priority-order logic is unit-testable without touching the real
/// filesystem/PATH - see `resolve_default_terminal_from_path` for the real
/// version.
pub fn resolve_default_terminal(
    setting: &DefaultTerminal,
    is_available: impl Fn(&str) -> bool,
) -> Option<String> {
    match setting {
        DefaultTerminal::Explicit(command) => Some(command.clone()),
        DefaultTerminal::Auto => TERMINAL_PRIORITY
            .iter()
            .find(|name| is_available(name))
            .map(|name| name.to_string()),
    }
}

/// Real-PATH-scanning version of `resolve_default_terminal`.
pub fn resolve_default_terminal_from_path(setting: &DefaultTerminal) -> Option<String> {
    resolve_default_terminal(setting, command_exists_in_path)
}

fn command_exists_in_path(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_setting_bypasses_detection_entirely() {
        let setting = DefaultTerminal::Explicit("my-custom-term".to_string());
        // is_available always false - explicit should still win.
        let resolved = resolve_default_terminal(&setting, |_| false);
        assert_eq!(resolved.as_deref(), Some("my-custom-term"));
    }

    #[test]
    fn auto_picks_first_available_in_priority_order() {
        let setting = DefaultTerminal::Auto;
        let available = ["kitty", "wezterm"];
        let resolved = resolve_default_terminal(&setting, |name| available.contains(&name));
        // alacritty leads the priority list but isn't "available" here.
        assert_eq!(resolved.as_deref(), Some("kitty"));
    }

    #[test]
    fn auto_prefers_alacritty_when_available() {
        let setting = DefaultTerminal::Auto;
        let available = ["alacritty", "kitty", "ghostty"];
        let resolved = resolve_default_terminal(&setting, |name| available.contains(&name));
        assert_eq!(resolved.as_deref(), Some("alacritty"));
    }

    #[test]
    fn auto_falls_through_to_later_candidates() {
        let setting = DefaultTerminal::Auto;
        let available = ["contour"];
        let resolved = resolve_default_terminal(&setting, |name| available.contains(&name));
        assert_eq!(resolved.as_deref(), Some("contour"));
    }

    #[test]
    fn auto_returns_none_when_nothing_available() {
        let setting = DefaultTerminal::Auto;
        let resolved = resolve_default_terminal(&setting, |_| false);
        assert_eq!(resolved, None);
    }

    #[test]
    fn priority_list_leads_with_alacritty_then_kitty() {
        assert_eq!(TERMINAL_PRIORITY[0], "alacritty");
        assert_eq!(TERMINAL_PRIORITY[1], "kitty");
    }
}
