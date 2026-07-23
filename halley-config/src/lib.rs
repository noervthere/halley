pub mod chord;
pub mod keybinds;
pub mod parse;

pub use chord::parse_chord;
pub use keybinds::{Action, DefaultTerminal, Keybind, Keybinds, ModifierKey, Modifiers};
pub use parse::{ParseError, parse_keybinds};
