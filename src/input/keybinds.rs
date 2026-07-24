use halley_config::{Action, Keybinds, ModifierKey, Modifiers};
use smithay::input::keyboard::{Keysym, xkb};

/// Which backend a bind table is being resolved for - drives the
/// Alt-vs-Super mod-key convention (design decision 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Winit,
    Tty,
}

/// Load the user's keybinds, falling back to `Keybinds::default()` on any
/// failure (missing `$HOME`, unwritable config dir, parse error) - a config
/// typo shouldn't crash compositor startup.
pub fn load_keybinds() -> Keybinds {
    let Some(path) = halley_config::config_path() else {
        eprintln!("keybinds: no config path resolvable, using defaults");
        return Keybinds::default();
    };

    if let Err(err) = halley_config::bootstrap_default_config_at(&path) {
        eprintln!("keybinds: failed to bootstrap default config: {err}");
    }

    let config = match rune_cfg::RuneConfig::from_file(&path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("keybinds: failed to load {path:?}, using defaults: {err}");
            return Keybinds::default();
        }
    };

    match halley_config::parse_keybinds(&config) {
        Ok(keybinds) => keybinds,
        Err(err) => {
            eprintln!("keybinds: failed to parse {path:?}, using defaults: {err}");
            Keybinds::default()
        }
    }
}

/// niri's confirmed convention (`Backend::mod_key`): tty uses the configured
/// mod key as-is; winit (nested) uses Alt instead, unless the user
/// explicitly configured Alt as their real mod key, in which case nested
/// falls back to Super - avoids stealing the host desktop's Super key
/// during nested/dev testing.
pub fn effective_mod(configured: ModifierKey, backend: BackendKind) -> ModifierKey {
    match backend {
        BackendKind::Winit => {
            if configured == ModifierKey::Alt {
                ModifierKey::Super
            } else {
                ModifierKey::Alt
            }
        }
        BackendKind::Tty => configured,
    }
}

fn modifier_bit(modifiers: Modifiers, key: ModifierKey) -> bool {
    match key {
        ModifierKey::Super => modifiers.super_key,
        ModifierKey::Alt => modifiers.alt,
        ModifierKey::Ctrl => modifiers.ctrl,
        ModifierKey::Shift => modifiers.shift,
    }
}

fn set_modifier_bit(modifiers: &mut Modifiers, key: ModifierKey, value: bool) {
    match key {
        ModifierKey::Super => modifiers.super_key = value,
        ModifierKey::Alt => modifiers.alt = value,
        ModifierKey::Ctrl => modifiers.ctrl = value,
        ModifierKey::Shift => modifiers.shift = value,
    }
}

/// If the `from` mod-key bit is set, clear it and set the `to` bit instead -
/// every other bit (e.g. shift) is untouched. A config chord is always
/// written in terms of the configured mod key; this remaps it to whatever
/// that mod key actually resolves to for the current backend.
pub fn remap_mod_bit(modifiers: Modifiers, from: ModifierKey, to: ModifierKey) -> Modifiers {
    if from == to || !modifier_bit(modifiers, from) {
        return modifiers;
    }
    let mut remapped = modifiers;
    set_modifier_bit(&mut remapped, from, false);
    set_modifier_bit(&mut remapped, to, true);
    remapped
}

/// A keybind fully resolved for matching against live input: modifiers
/// already remapped for this backend, key name already resolved to a
/// `Keysym`. Every `Action` variant is resolved now - real keyboard focus
/// (`WaylandState.focused`) exists, so `CloseFocusedWindow` finally has
/// something to act on (see `wayland::xdg_shell::close_focused`).
#[derive(Clone, Copy, Debug)]
pub struct ResolvedBind {
    pub modifiers: Modifiers,
    pub keysym: Keysym,
    pub action: Action,
}

pub fn resolve_binds(keybinds: &Keybinds, backend: BackendKind) -> Vec<ResolvedBind> {
    let effective = effective_mod(keybinds.modifier, backend);

    keybinds
        .binds
        .iter()
        .filter_map(|bind| {
            let keysym = xkb::keysym_from_name(&bind.key, xkb::KEYSYM_NO_FLAGS);
            if keysym == Keysym::NoSymbol {
                eprintln!("keybinds: unknown key name {:?}, skipping bind", bind.key);
                return None;
            }
            Some(ResolvedBind {
                modifiers: remap_mod_bit(bind.modifiers, keybinds.modifier, effective),
                keysym,
                action: bind.action,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winit_defaults_super_config_to_alt() {
        assert_eq!(effective_mod(ModifierKey::Super, BackendKind::Winit), ModifierKey::Alt);
    }

    #[test]
    fn winit_falls_back_to_super_when_configured_alt() {
        assert_eq!(effective_mod(ModifierKey::Alt, BackendKind::Winit), ModifierKey::Super);
    }

    #[test]
    fn tty_uses_configured_mod_unchanged() {
        assert_eq!(effective_mod(ModifierKey::Super, BackendKind::Tty), ModifierKey::Super);
        assert_eq!(effective_mod(ModifierKey::Ctrl, BackendKind::Tty), ModifierKey::Ctrl);
    }

    #[test]
    fn remap_moves_the_bit_and_leaves_others_alone() {
        let modifiers = Modifiers {
            shift: true,
            super_key: true,
            ..Modifiers::default()
        };
        let remapped = remap_mod_bit(modifiers, ModifierKey::Super, ModifierKey::Alt);
        assert!(remapped.alt);
        assert!(!remapped.super_key);
        assert!(remapped.shift);
    }

    #[test]
    fn remap_is_a_no_op_when_bit_not_set() {
        let modifiers = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let remapped = remap_mod_bit(modifiers, ModifierKey::Super, ModifierKey::Alt);
        assert_eq!(remapped, modifiers);
    }

    #[test]
    fn default_config_quit_resolves_to_alt_shift_e_under_winit() {
        let keybinds = Keybinds::default();
        let resolved = resolve_binds(&keybinds, BackendKind::Winit);

        assert_eq!(resolved.len(), 6);
        let quit = resolved.iter().find(|bind| bind.action == Action::Quit).unwrap();
        assert!(quit.modifiers.alt);
        assert!(!quit.modifiers.super_key);
        assert!(quit.modifiers.shift);
        assert_eq!(quit.keysym, xkb::keysym_from_name("e", xkb::KEYSYM_NO_FLAGS));
    }

    #[test]
    fn default_config_quit_resolves_to_super_shift_e_under_tty() {
        let keybinds = Keybinds::default();
        let resolved = resolve_binds(&keybinds, BackendKind::Tty);

        assert_eq!(resolved.len(), 6);
        let quit = resolved.iter().find(|bind| bind.action == Action::Quit).unwrap();
        assert!(quit.modifiers.super_key);
        assert!(!quit.modifiers.alt);
        assert!(quit.modifiers.shift);
    }

    #[test]
    fn default_config_open_terminal_resolves_to_mod_t() {
        let keybinds = Keybinds::default();
        let resolved = resolve_binds(&keybinds, BackendKind::Tty);

        let open_terminal = resolved
            .iter()
            .find(|bind| bind.action == Action::OpenTerminal)
            .unwrap();
        assert!(open_terminal.modifiers.super_key);
        assert!(!open_terminal.modifiers.shift);
        assert_eq!(open_terminal.keysym, xkb::keysym_from_name("t", xkb::KEYSYM_NO_FLAGS));
    }

    #[test]
    fn every_configured_action_is_resolved() {
        let keybinds = Keybinds::default();
        let resolved = resolve_binds(&keybinds, BackendKind::Tty);
        assert!(resolved.iter().any(|bind| bind.action == Action::Quit));
        assert!(resolved.iter().any(|bind| bind.action == Action::CloseFocusedWindow));
        assert!(resolved.iter().any(|bind| bind.action == Action::OpenTerminal));
        assert!(resolved.iter().any(|bind| bind.action == Action::ZoomIn));
        assert!(resolved.iter().any(|bind| bind.action == Action::ZoomOut));
        assert!(resolved.iter().any(|bind| bind.action == Action::ZoomReset));
    }
}
