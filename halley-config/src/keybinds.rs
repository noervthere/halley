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
    OpenTerminal,
    ZoomIn,
    ZoomOut,
    ZoomReset,
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
    /// Matches `examples/halley.rune` exactly - this is the fallback used
    /// when no user config exists yet (see `bootstrap.rs`), not an
    /// independently-maintained second copy of the same three binds.
    fn default() -> Self {
        Self {
            modifier: ModifierKey::Super,
            default_terminal: DefaultTerminal::Auto,
            binds: vec![
                Keybind {
                    modifiers: Modifiers {
                        shift: true,
                        super_key: true,
                        ..Modifiers::default()
                    },
                    key: "e".to_string(),
                    action: Action::Quit,
                },
                Keybind {
                    modifiers: Modifiers {
                        super_key: true,
                        ..Modifiers::default()
                    },
                    key: "c".to_string(),
                    action: Action::CloseFocusedWindow,
                },
                Keybind {
                    modifiers: Modifiers {
                        super_key: true,
                        ..Modifiers::default()
                    },
                    key: "t".to_string(),
                    action: Action::OpenTerminal,
                },
                Keybind {
                    modifiers: Modifiers {
                        super_key: true,
                        ..Modifiers::default()
                    },
                    key: "minus".to_string(),
                    action: Action::ZoomOut,
                },
                // Bound to the unshifted "=" key (same physical key "+"
                // lives on), not literal shift+plus - matches "minus"'s own
                // no-shift ergonomics and the common Ctrl+=/Ctrl+- app
                // convention for zoom. A chord requiring shift would need
                // `shift: true` here too, since the produced keysym would be
                // "plus" only while shift is actually held.
                Keybind {
                    modifiers: Modifiers {
                        super_key: true,
                        ..Modifiers::default()
                    },
                    key: "equal".to_string(),
                    action: Action::ZoomIn,
                },
                Keybind {
                    modifiers: Modifiers {
                        super_key: true,
                        ..Modifiers::default()
                    },
                    key: "0".to_string(),
                    action: Action::ZoomReset,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_the_six_starting_binds() {
        let kb = Keybinds::default();
        assert_eq!(kb.modifier, ModifierKey::Super);
        assert_eq!(kb.default_terminal, DefaultTerminal::Auto);
        assert_eq!(kb.binds.len(), 6);

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
        assert_eq!(close.key, "c");

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

        let zoom_in = kb.binds.iter().find(|b| b.action == Action::ZoomIn).unwrap();
        assert!(zoom_in.modifiers.super_key);
        assert_eq!(zoom_in.key, "equal");

        let zoom_reset = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomReset)
            .unwrap();
        assert!(zoom_reset.modifiers.super_key);
        assert_eq!(zoom_reset.key, "0");
    }
}
