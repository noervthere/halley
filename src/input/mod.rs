pub mod grab;
pub mod keybinds;
pub mod pointer;
pub mod zoom;

use halley_config::{Action, ModifierKey, Modifiers};
use smithay::backend::input::Keycode;
use smithay::input::keyboard::{Keysym, ModifiersState};

use keybinds::{BackendKind, ResolvedBind, ResolvedTrigger};

pub fn modifiers_match(state: &ModifiersState, expected: Modifiers) -> bool {
    state.ctrl == expected.ctrl
        && state.alt == expected.alt
        && state.shift == expected.shift
        && state.logo == expected.super_key
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
    keysym: Keysym,
    keycode: Keycode,
) -> Option<Action> {
    let bind = binds.iter().find(|bind| {
        let trigger_matches = match bind.trigger {
            ResolvedTrigger::Keysym(expected) => expected == keysym,
            ResolvedTrigger::Keycode(expected) => expected == keycode,
            ResolvedTrigger::PointerButton(_) | ResolvedTrigger::Wheel(_) => false,
        };
        trigger_matches && modifiers_match(mods, bind.modifiers)
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
