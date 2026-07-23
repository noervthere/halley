pub mod chord;
pub mod keybinds;

pub use chord::parse_chord;
pub use keybinds::{Action, DefaultTerminal, Keybind, Keybinds, ModifierKey, Modifiers};
