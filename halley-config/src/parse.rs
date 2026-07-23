use std::collections::HashMap;
use std::fmt;

use rune_cfg::RuneConfig;

use crate::chord::parse_chord;
use crate::keybinds::{Action, DefaultTerminal, Keybind, Keybinds, ModifierKey};

const KEY_MOD: &str = "mod";
const KEY_DEFAULT_TERMINAL_KEBAB: &str = "default-terminal";
const KEY_DEFAULT_TERMINAL_SNAKE: &str = "default_terminal";

#[derive(Debug)]
pub enum ParseError {
    Rune(rune_cfg::RuneError),
    UnknownModifier(String),
    UnknownAction { chord: String, action: String },
    InvalidChord(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Rune(e) => write!(f, "config error: {e}"),
            ParseError::UnknownModifier(m) => write!(f, "unknown modifier key: {m:?}"),
            ParseError::UnknownAction { chord, action } => {
                write!(f, "unknown action {action:?} for bind {chord:?}")
            }
            ParseError::InvalidChord(c) => write!(f, "invalid keybind chord: {c:?}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<rune_cfg::RuneError> for ParseError {
    fn from(e: rune_cfg::RuneError) -> Self {
        ParseError::Rune(e)
    }
}

fn parse_modifier_key(s: &str) -> Option<ModifierKey> {
    match s.to_lowercase().as_str() {
        "super" | "logo" | "mod4" => Some(ModifierKey::Super),
        "alt" => Some(ModifierKey::Alt),
        "ctrl" | "control" => Some(ModifierKey::Ctrl),
        "shift" => Some(ModifierKey::Shift),
        _ => None,
    }
}

fn parse_action(s: &str) -> Option<Action> {
    match s {
        "quit" => Some(Action::Quit),
        "close-focused" | "close_focused" | "close-window" | "close_window" => {
            Some(Action::CloseFocusedWindow)
        }
        "open-terminal" | "open_terminal" => Some(Action::OpenTerminal),
        _ => None,
    }
}

/// Parse the `keybinds:` section of a loaded `RuneConfig` into a `Keybinds`
/// value.
///
/// Every value in the section happens to be a plain string (`mod`,
/// `default-terminal`, and every chord->action pair), so this reads the
/// whole section as `HashMap<String, String>` in one call rather than
/// walking the AST by hand.
///
/// Chord keys reference the modifier via a literal `$var.mod` substring
/// (e.g. `"$var.mod+shift+e"`) - rune-cfg's `$var` resolution only applies
/// to *values*, never object *keys* (confirmed by reading its resolver:
/// keys are plain, unprocessed `String`s in the AST), so this substitutes
/// `$var.mod` with the already-parsed modifier string itself before
/// chord-parsing, rather than relying on rune-cfg to do it.
pub fn parse_keybinds(config: &RuneConfig) -> Result<Keybinds, ParseError> {
    let map: HashMap<String, String> = config.get("keybinds")?;

    let modifier_str = map
        .get(KEY_MOD)
        .cloned()
        .unwrap_or_else(|| "super".to_string());
    let modifier = parse_modifier_key(&modifier_str)
        .ok_or_else(|| ParseError::UnknownModifier(modifier_str.clone()))?;

    let default_terminal = match map
        .get(KEY_DEFAULT_TERMINAL_KEBAB)
        .or_else(|| map.get(KEY_DEFAULT_TERMINAL_SNAKE))
    {
        None => DefaultTerminal::Auto,
        Some(s) if s.eq_ignore_ascii_case("auto") => DefaultTerminal::Auto,
        Some(s) => DefaultTerminal::Explicit(s.clone()),
    };

    let mut entries: Vec<(&String, &String)> = map
        .iter()
        .filter(|(k, _)| {
            k.as_str() != KEY_MOD
                && k.as_str() != KEY_DEFAULT_TERMINAL_KEBAB
                && k.as_str() != KEY_DEFAULT_TERMINAL_SNAKE
        })
        .collect();
    // HashMap iteration order isn't stable - sort so parsing the same file
    // always produces binds in the same order.
    entries.sort_by_key(|(k, _)| *k);

    let mut binds = Vec::with_capacity(entries.len());
    for (chord_key, action_str) in entries {
        let resolved_chord = chord_key.replace("$var.mod", &modifier_str);
        let (modifiers, key) = parse_chord(&resolved_chord)
            .ok_or_else(|| ParseError::InvalidChord(chord_key.clone()))?;
        let action = parse_action(action_str).ok_or_else(|| ParseError::UnknownAction {
            chord: chord_key.clone(),
            action: action_str.clone(),
        })?;
        binds.push(Keybind {
            modifiers,
            key,
            action,
        });
    }

    Ok(Keybinds {
        modifier,
        default_terminal,
        binds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Keybinds {
        let config = RuneConfig::from_str(src).expect("valid rune-cfg source");
        parse_keybinds(&config).expect("valid keybinds section")
    }

    #[test]
    fn parses_mod_and_three_binds() {
        let kb = parse(
            r#"
keybinds:
  mod "super"

  "$var.mod+shift+e" "quit"
  "$var.mod+c" "close-focused"
  "$var.mod+t" "open-terminal"

  default-terminal "auto"
end
"#,
        );

        assert_eq!(kb.modifier, ModifierKey::Super);
        assert_eq!(kb.default_terminal, DefaultTerminal::Auto);
        assert_eq!(kb.binds.len(), 3);

        let quit = kb.binds.iter().find(|b| b.action == Action::Quit).unwrap();
        assert_eq!(quit.key, "e");
        assert!(quit.modifiers.super_key);
        assert!(quit.modifiers.shift);
    }

    #[test]
    fn accepts_close_window_alias() {
        let kb = parse(
            r#"
keybinds:
  mod "alt"
  "$var.mod+q" "close-window"
end
"#,
        );
        assert_eq!(kb.modifier, ModifierKey::Alt);
        let close = kb
            .binds
            .iter()
            .find(|b| b.action == Action::CloseFocusedWindow)
            .unwrap();
        assert_eq!(close.key, "q");
        assert!(close.modifiers.alt);
        assert!(!close.modifiers.super_key);
    }

    #[test]
    fn explicit_default_terminal_bypasses_auto() {
        let kb = parse(
            r#"
keybinds:
  mod "super"
  default-terminal "foot"
end
"#,
        );
        assert_eq!(kb.default_terminal, DefaultTerminal::Explicit("foot".to_string()));
    }

    #[test]
    fn missing_default_terminal_defaults_to_auto() {
        let kb = parse(
            r#"
keybinds:
  mod "super"
end
"#,
        );
        assert_eq!(kb.default_terminal, DefaultTerminal::Auto);
        assert_eq!(kb.binds, Vec::<Keybind>::new());
    }

    #[test]
    fn unknown_modifier_name_errors() {
        let config = RuneConfig::from_str(
            r#"
keybinds:
  mod "windows"
end
"#,
        )
        .unwrap();
        assert!(matches!(
            parse_keybinds(&config),
            Err(ParseError::UnknownModifier(_))
        ));
    }

    #[test]
    fn unknown_action_errors() {
        let config = RuneConfig::from_str(
            r#"
keybinds:
  mod "super"
  "$var.mod+x" "launch-nukes"
end
"#,
        )
        .unwrap();
        assert!(matches!(
            parse_keybinds(&config),
            Err(ParseError::UnknownAction { .. })
        ));
    }

    #[test]
    fn missing_keybinds_section_errors() {
        let config = RuneConfig::from_str("other: \n foo \"bar\"\nend\n").unwrap();
        assert!(parse_keybinds(&config).is_err());
    }
}
