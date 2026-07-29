/// The base "mod" key a keybind chord is built on. Just the four keys that
/// matter for a compositor keybind (not evdev/xkb keycodes - those don't
/// exist anywhere in this crate, and won't until `halley-wl` does real
/// input handling).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierKey {
    Super,
    Alt,
    Ctrl,
    Shift,
}

/// A resolved modifier combination for a single keybind (the base `mod` plus
/// any extra modifiers in the chord, e.g. `mod+shift+e`).
///
/// Deliberately flat, unlike old halley's `KeyModifiers` (which split every
/// modifier into generic/left/right bools to match real evdev key-press
/// tracking) - that granularity doesn't apply here yet, since no input
/// handling exists in this crate or `halley-wl` at all. Add it back exactly
/// when real per-side key tracking is built, not before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusCycleDirection {
    Forward,
    Backward,
}

/// What a keybind does. Grows alongside whatever `halley-wl` actually wires
/// up next. `ZoomIn` exists to walk back a `ZoomOut` one step at a time, but
/// it's not a general "magnify past 1.0x" action - the 1.0x ceiling is
/// enforced independently of this enum (`Camera::clamp_view_size`, called
/// with a hardcoded `zoom_max` of `1.0` at every call site in
/// `src/input/zoom.rs`), so `ZoomIn` can never push past it no matter how
/// many times it's pressed. At 1.0x it's simply a no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    CloseFocusedWindow,
    ToggleFullscreen,
    ToggleState,
    Apogee,
    BearingsShow,
    BearingsToggle,
    FocusCycle(FocusCycleDirection),
    OpenTerminal,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Screenshot,
    /// A user-provided command line. This is the fallback for every action
    /// string that is not one of Halley's compositor actions.
    Spawn(String),
}

/// A single parsed keybind: the modifier combination, the key name (as
/// written in config - e.g. `"e"`, `"return"` - not a resolved keycode),
/// and the action it triggers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keybind {
    pub modifiers: Modifiers,
    pub key: String,
    pub action: Action,
}

/// What terminal `Action::OpenTerminal` should launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultTerminal {
    /// Detect an installed terminal from a priority list (see `terminal.rs`).
    Auto,
    /// Use this exact command, no detection.
    Explicit(String),
}

/// The whole (currently keybinds-only) parsed config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keybinds {
    pub modifier: ModifierKey,
    pub default_terminal: DefaultTerminal,
    pub binds: Vec<Keybind>,
}

impl Default for Keybinds {
    /// Parse the same template written by the bootstrap path so the runtime
    /// fallback and fresh-install config cannot drift apart.
    fn default() -> Self {
        let config = rune_cfg::RuneConfig::from_str(crate::bootstrap::DEFAULT_CONFIG)
            .expect("embedded default config must remain valid");
        crate::parse::parse_keybinds(&config).expect("embedded default keybinds must remain valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_the_shipped_keybinds() {
        let kb = Keybinds::default();
        assert_eq!(kb.modifier, ModifierKey::Super);
        assert_eq!(kb.default_terminal, DefaultTerminal::Auto);
        assert_eq!(kb.binds.len(), 17);

        let quit = kb.binds.iter().find(|b| b.action == Action::Quit).unwrap();
        assert!(quit.modifiers.super_key);
        assert!(quit.modifiers.shift);
        assert_eq!(quit.key, "e");

        let close = kb
            .binds
            .iter()
            .find(|b| b.action == Action::CloseFocusedWindow)
            .unwrap();
        assert!(close.modifiers.super_key);
        assert!(!close.modifiers.shift);
        assert_eq!(close.key, "q");

        let fullscreen = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ToggleFullscreen)
            .unwrap();
        assert!(fullscreen.modifiers.super_key);
        assert_eq!(fullscreen.key, "f");

        let toggle_state = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ToggleState)
            .unwrap();
        assert!(toggle_state.modifiers.super_key);
        assert_eq!(toggle_state.key, "n");

        let apogee = kb
            .binds
            .iter()
            .find(|b| b.action == Action::Apogee)
            .unwrap();
        assert!(apogee.modifiers.super_key);
        assert_eq!(apogee.key, "o");

        let bearings_show = kb
            .binds
            .iter()
            .find(|b| b.action == Action::BearingsShow)
            .unwrap();
        assert!(bearings_show.modifiers.super_key);
        assert!(!bearings_show.modifiers.shift);
        assert_eq!(bearings_show.key, "z");

        let bearings_toggle = kb
            .binds
            .iter()
            .find(|b| b.action == Action::BearingsToggle)
            .unwrap();
        assert!(bearings_toggle.modifiers.super_key);
        assert!(bearings_toggle.modifiers.shift);
        assert_eq!(bearings_toggle.key, "z");

        let forward = kb
            .binds
            .iter()
            .find(|b| b.action == Action::FocusCycle(FocusCycleDirection::Forward))
            .unwrap();
        assert!(forward.modifiers.alt);
        assert!(!forward.modifiers.shift);
        assert_eq!(forward.key, "tab");

        let backward = kb
            .binds
            .iter()
            .find(|b| b.action == Action::FocusCycle(FocusCycleDirection::Backward))
            .unwrap();
        assert!(backward.modifiers.alt);
        assert!(backward.modifiers.shift);
        assert_eq!(backward.key, "tab");

        let term = kb
            .binds
            .iter()
            .find(|b| b.action == Action::OpenTerminal)
            .unwrap();
        assert!(term.modifiers.super_key);
        assert_eq!(term.key, "t");

        let zoom_out = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomOut)
            .unwrap();
        assert!(zoom_out.modifiers.super_key);
        assert_eq!(zoom_out.key, "minus");

        let zoom_in = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomIn)
            .unwrap();
        assert!(zoom_in.modifiers.super_key);
        assert_eq!(zoom_in.key, "equal");

        let zoom_reset = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomReset)
            .unwrap();
        assert!(zoom_reset.modifiers.super_key);
        assert_eq!(zoom_reset.key, "0");

        let screenshot = kb
            .binds
            .iter()
            .find(|b| b.action == Action::Screenshot)
            .unwrap();
        assert_eq!(screenshot.modifiers, Modifiers::default());
        assert_eq!(screenshot.key, "Print");
    }
}
