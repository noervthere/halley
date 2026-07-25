pub mod grab;
pub mod keybinds;
pub mod pointer;
pub mod zoom;

use std::collections::HashSet;

use halley_config::{Action, ModifierKey, Modifiers};
use smithay::backend::input::Keycode;
use smithay::input::keyboard::{Keysym, ModifiersState};

use keybinds::{BackendKind, ResolvedBind, ResolvedTrigger, WheelDirection};

pub fn modifiers_match(state: &ModifiersState, expected: Modifiers) -> bool {
    state.ctrl == expected.ctrl
        && state.alt == expected.alt
        && state.shift == expected.shift
        && state.logo == expected.super_key
}

fn keyboard_modifiers_match(
    state: &ModifiersState,
    expected: Modifiers,
    trigger: ResolvedTrigger,
) -> bool {
    let mut without_trigger = *state;
    if let ResolvedTrigger::Keysym(keysym) = trigger {
        if matches!(keysym, Keysym::Shift_L | Keysym::Shift_R) {
            without_trigger.shift = false;
        } else if matches!(keysym, Keysym::Control_L | Keysym::Control_R) {
            without_trigger.ctrl = false;
        } else if matches!(keysym, Keysym::Alt_L | Keysym::Alt_R) {
            without_trigger.alt = false;
        } else if matches!(keysym, Keysym::Super_L | Keysym::Super_R) {
            without_trigger.logo = false;
        }
    }
    modifiers_match(&without_trigger, expected)
}

/// Whether the given modifier key is currently held, per a live
/// `ModifiersState` query - used by `input::grab`'s pointer-button dispatch,
/// which (unlike keyboard binds) has no filter closure to read modifiers
/// from and instead queries `KeyboardHandle::modifier_state()` directly at
/// button-press time.
pub fn mod_key_held(state: &ModifiersState, key: ModifierKey) -> bool {
    match key {
        ModifierKey::Super => state.logo,
        ModifierKey::Alt => state.alt,
        ModifierKey::Ctrl => state.ctrl,
        ModifierKey::Shift => state.shift,
    }
}

/// Looks up a pressed keysym/raw keycode plus modifiers against the resolved
/// bind table. This remains pure and backend-independent so both sessions
/// can share it from their real `KeyboardHandle::input()` filter closures.
pub fn match_keyboard_bind(
    binds: &[ResolvedBind],
    mods: &ModifiersState,
    keysym: Option<Keysym>,
    keycode: Keycode,
) -> Option<Action> {
    let bind = binds.iter().find(|bind| {
        let trigger_matches = match bind.trigger {
            ResolvedTrigger::Keysym(expected) => Some(expected) == keysym,
            ResolvedTrigger::Keycode(expected) => expected == keycode,
            ResolvedTrigger::PointerButton(_) | ResolvedTrigger::Wheel(_) => false,
        };
        trigger_matches && keyboard_modifiers_match(mods, bind.modifiers, bind.trigger)
    })?;
    // Low-frequency (only fires on an actual match, not every keystroke) and
    // genuinely useful - confirms which action a chord resolved to without
    // needing to reason about it from config alone.
    eprintln!(
        "keybinds: {:?} + {mods:?} -> {:?}",
        bind.trigger, bind.action
    );
    Some(bind.action.clone())
}

pub fn match_pointer_bind(
    binds: &[ResolvedBind],
    mods: &ModifiersState,
    button: u32,
) -> Option<Action> {
    let bind = binds.iter().find(|bind| {
        matches!(
            bind.trigger,
            ResolvedTrigger::PointerButton(trigger) if trigger.matches(button)
        ) && modifiers_match(mods, bind.modifiers)
    })?;
    eprintln!(
        "keybinds: {:?} + {mods:?} -> {:?}",
        bind.trigger, bind.action
    );
    Some(bind.action.clone())
}

pub fn match_wheel_bind(
    binds: &[ResolvedBind],
    mods: &ModifiersState,
    direction: WheelDirection,
) -> Option<Action> {
    let bind = binds.iter().find(|bind| {
        bind.trigger == ResolvedTrigger::Wheel(direction)
            && modifiers_match(mods, bind.modifiers)
    })?;
    Some(bind.action.clone())
}

pub fn no_modifiers_held(state: &ModifiersState) -> bool {
    !state.ctrl && !state.alt && !state.shift && !state.logo
}

#[derive(Default)]
pub struct SuppressedButtons {
    buttons: HashSet<u32>,
}

impl SuppressedButtons {
    pub fn suppress(&mut self, button: u32) {
        self.buttons.insert(button);
    }

    pub fn release_is_suppressed(&mut self, button: u32) -> bool {
        self.buttons.remove(&button)
    }
}

/// The resolved bind table plus the configured terminal command - nothing
/// else. Used to own a fake `Seat`/`KeyboardHandle` purely to match
/// keybinds, back when there was no real Wayland client to focus or forward
/// to; now that real clients exist, matching happens directly on the real
/// `Seat<App>`/`Seat<TtyApp>` each app already owns, so this is just data.
pub struct Keyboard {
    pub binds: Vec<ResolvedBind>,
    /// The configured mod key, already remapped for this backend (matches
    /// `binds`' own chords) - `input::grab`'s pointer-button dispatch needs
    /// this too, since "mod+click" checks the same mod key keyboard binds
    /// use, just via a live `modifier_state()` query instead of a filter
    /// closure (pointer events don't carry modifier state directly).
    pub effective_mod: ModifierKey,
    /// Resolved once at startup from the user's config - `None` if nothing
    /// was configured and nothing from `TERMINAL_PRIORITY` was found on
    /// `PATH`. `Action::OpenTerminal`'s dispatch (driving code) needs this,
    /// but resolving *what* to launch is keybind-config concern, not
    /// something the caller should redo itself.
    terminal_command: Option<String>,
}

impl Keyboard {
    pub fn new(backend: BackendKind) -> Self {
        let keybinds = keybinds::load_keybinds();
        let binds = keybinds::resolve_binds(&keybinds, backend);
        let effective_mod = keybinds::effective_mod(keybinds.modifier, backend);
        let terminal_command = halley_config::resolve_default_terminal_from_path(&keybinds.default_terminal);

        Self {
            binds,
            effective_mod,
            terminal_command,
        }
    }

    /// The command `Action::OpenTerminal` should launch - `None` if nothing
    /// was configured and nothing from the priority list was found on
    /// `PATH`.
    pub fn terminal_command(&self) -> Option<&str> {
        self.terminal_command.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keybinds::{PointerButtonTrigger, ResolvedTrigger};

    fn bind(trigger: ResolvedTrigger, modifiers: Modifiers) -> ResolvedBind {
        ResolvedBind {
            modifiers,
            trigger,
            action: Action::Quit,
        }
    }

    #[test]
    fn pointer_bind_requires_exact_modifiers() {
        let binds = [bind(
            ResolvedTrigger::PointerButton(PointerButtonTrigger::Left),
            Modifiers {
                super_key: true,
                ..Modifiers::default()
            },
        )];
        assert_eq!(
            match_pointer_bind(&binds, &ModifiersState::default(), 0x110),
            None
        );
        let mods = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };
        assert_eq!(match_pointer_bind(&binds, &mods, 0x110), Some(Action::Quit));
    }

    #[test]
    fn raw_pointer_button_matches_only_its_code() {
        let binds = [bind(
            ResolvedTrigger::PointerButton(PointerButtonTrigger::Code(279)),
            Modifiers::default(),
        )];
        assert_eq!(
            match_pointer_bind(&binds, &ModifiersState::default(), 278),
            None
        );
        assert_eq!(
            match_pointer_bind(&binds, &ModifiersState::default(), 279),
            Some(Action::Quit)
        );
    }

    #[test]
    fn wheel_directions_do_not_cross_match() {
        let binds = [bind(
            ResolvedTrigger::Wheel(WheelDirection::Up),
            Modifiers::default(),
        )];
        assert_eq!(
            match_wheel_bind(
                &binds,
                &ModifiersState::default(),
                WheelDirection::Down
            ),
            None
        );
        assert_eq!(
            match_wheel_bind(&binds, &ModifiersState::default(), WheelDirection::Up),
            Some(Action::Quit)
        );
    }

    #[test]
    fn raw_keycodes_match_even_when_xkb_has_no_symbol() {
        let binds = [bind(
            ResolvedTrigger::Keycode(Keycode::new(255)),
            Modifiers::default(),
        )];
        assert_eq!(
            match_keyboard_bind(
                &binds,
                &ModifiersState::default(),
                None,
                Keycode::new(255)
            ),
            Some(Action::Quit)
        );
    }

    #[test]
    fn modifier_keys_can_be_bare_triggers() {
        let binds = [
            bind(
                ResolvedTrigger::Keysym(Keysym::Shift_L),
                Modifiers::default(),
            ),
            bind(
                ResolvedTrigger::Keysym(Keysym::Super_R),
                Modifiers::default(),
            ),
        ];
        let shift = ModifiersState {
            shift: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            match_keyboard_bind(
                &binds,
                &shift,
                Some(Keysym::Shift_L),
                Keycode::new(50)
            ),
            Some(Action::Quit)
        );
        let logo = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            match_keyboard_bind(
                &binds,
                &logo,
                Some(Keysym::Super_R),
                Keycode::new(134)
            ),
            Some(Action::Quit)
        );
    }

    #[test]
    fn suppressed_button_release_is_consumed_exactly_once() {
        let mut suppressed = SuppressedButtons::default();
        suppressed.suppress(0x110);
        assert!(!suppressed.release_is_suppressed(0x111));
        assert!(suppressed.release_is_suppressed(0x110));
        assert!(!suppressed.release_is_suppressed(0x110));
    }
}
