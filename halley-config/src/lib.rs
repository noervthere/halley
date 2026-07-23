pub mod bootstrap;
pub mod chord;
pub mod keybinds;
pub mod parse;
pub mod terminal;

pub use bootstrap::{DEFAULT_CONFIG, bootstrap_default_config, bootstrap_default_config_at, config_path};
pub use chord::parse_chord;
pub use keybinds::{Action, DefaultTerminal, Keybind, Keybinds, ModifierKey, Modifiers};
pub use parse::{ParseError, parse_keybinds};
pub use terminal::{TERMINAL_PRIORITY, resolve_default_terminal, resolve_default_terminal_from_path};
