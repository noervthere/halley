use halley_config::{Action, DefaultTerminal, ModifierKey, parse_keybinds};
use rune_cfg::RuneConfig;

const EXAMPLE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/halley.rune");

/// Real end-to-end check: parse the actual shipped example file through
/// real rune-cfg, not just hand-constructed strings like the unit tests
/// elsewhere in this crate use.
#[test]
fn example_config_parses_end_to_end() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let keybinds = parse_keybinds(&config).expect("example keybinds section parses");

    assert_eq!(keybinds.modifier, ModifierKey::Super);
    assert_eq!(keybinds.default_terminal, DefaultTerminal::Auto);
    assert_eq!(keybinds.binds.len(), 3);

    let quit = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::Quit)
        .expect("quit bind present");
    assert_eq!(quit.key, "e");
    assert!(quit.modifiers.super_key);
    assert!(quit.modifiers.shift);

    let close = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::CloseFocusedWindow)
        .expect("close-focused bind present");
    assert_eq!(close.key, "c");
    assert!(close.modifiers.super_key);
    assert!(!close.modifiers.shift);

    let terminal = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::OpenTerminal)
        .expect("open-terminal bind present");
    assert_eq!(terminal.key, "t");
    assert!(terminal.modifiers.super_key);
}
