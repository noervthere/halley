pub mod keybinds;
pub mod pointer;

use halley_config::{Action, Modifiers};
use smithay::input::keyboard::{Keysym, ModifiersState};

use keybinds::{BackendKind, ResolvedBind};

pub fn modifiers_match(state: &ModifiersState, expected: Modifiers) -> bool {
    state.ctrl == expected.ctrl
        && state.alt == expected.alt
        && state.shift == expected.shift
        && state.logo == expected.super_key
}

/// Looks up a pressed keysym+modifiers against the resolved bind table -
/// pure and backend-independent, so both `session::tty` and `session::winit`
/// can share it from inside their own real `KeyboardHandle::input()` filter
/// closures (which need the concrete `App`/`TtyApp` type, so the closure
/// itself can't live here - only this lookup can).
pub fn match_bind(binds: &[ResolvedBind], mods: &ModifiersState, keysym: Keysym) -> Option<Action> {
    let bind = binds
        .iter()
        .find(|bind| bind.keysym == keysym && modifiers_match(mods, bind.modifiers))?;
    // Low-frequency (only fires on an actual match, not every keystroke) and
    // genuinely useful - confirms which action a chord resolved to without
    // needing to reason about it from config alone.
    eprintln!("keybinds: {keysym:?} + {mods:?} -> {:?}", bind.action);
    Some(bind.action)
}

/// The resolved bind table plus the configured terminal command - nothing
/// else. Used to own a fake `Seat`/`KeyboardHandle` purely to match
/// keybinds, back when there was no real Wayland client to focus or forward
/// to; now that real clients exist, matching happens directly on the real
/// `Seat<App>`/`Seat<TtyApp>` each app already owns (see `match_bind` and
/// each session module's input-handling closure), so this is just data.
pub struct Keyboard {
    pub binds: Vec<ResolvedBind>,
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
        let terminal_command = halley_config::resolve_default_terminal_from_path(&keybinds.default_terminal);

        Self {
            binds,
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
