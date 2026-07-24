pub mod bootstrap;
pub mod chord;
pub mod decorations;
pub mod keybinds;
pub mod output;
pub mod parse;
pub mod terminal;
pub mod zoom;

pub use bootstrap::{DEFAULT_CONFIG, bootstrap_default_config, bootstrap_default_config_at, config_path};
pub use chord::parse_chord;
pub use decorations::{BorderColor, Decorations, load_decorations, parse_decorations};
pub use keybinds::{Action, DefaultTerminal, Keybind, Keybinds, ModifierKey, Modifiers};
pub use output::{OutputConfig, Vrr, load_outputs, parse_outputs};
pub use parse::{ParseError, parse_keybinds};
pub use terminal::{TERMINAL_PRIORITY, resolve_default_terminal, resolve_default_terminal_from_path};
pub use zoom::{Zoom, load_zoom, parse_zoom};
