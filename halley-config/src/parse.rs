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
    InvalidChord(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Rune(e) => write!(f, "config error: {e}"),
            ParseError::UnknownModifier(m) => write!(f, "unknown modifier key: {m:?}"),
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

fn parse_direction(value: &str) -> Option<crate::Direction> {
    match value {
        "left" => Some(crate::Direction::Left),
        "right" => Some(crate::Direction::Right),
        "up" => Some(crate::Direction::Up),
        "down" => Some(crate::Direction::Down),
        _ => None,
    }
}

fn parse_action(s: &str) -> Action {
    let words = s.split_whitespace().collect::<Vec<_>>();
    if let ["cluster", "slot", slot] = words.as_slice()
        && let Ok(slot) = slot.parse::<u8>()
        && (1..=10).contains(&slot)
    {
        return Action::ClusterSlot(slot);
    }
    if let Some(slot) = s
        .strip_prefix("cluster-slot-")
        .or_else(|| s.strip_prefix("cluster_slot_"))
        .and_then(|slot| slot.parse::<u8>().ok())
        .filter(|slot| (1..=10).contains(slot))
    {
        return Action::ClusterSlot(slot);
    }
    if let ["cluster", "focus", direction]
    | ["cluster-focus", direction]
    | ["tile", "focus", direction]
    | ["tile-focus", direction] = words.as_slice()
        && let Some(direction) = parse_direction(direction)
    {
        return Action::ClusterTileFocus(direction);
    }
    if let Some(direction) = s
        .strip_prefix("cluster-focus-")
        .or_else(|| s.strip_prefix("cluster_focus_"))
        .or_else(|| s.strip_prefix("cluster-tile-focus-"))
        .or_else(|| s.strip_prefix("cluster_tile_focus_"))
        .and_then(parse_direction)
    {
        return Action::ClusterTileFocus(direction);
    }
    if let ["tile", "swap", direction] | ["tile-swap", direction] = words.as_slice()
        && let Some(direction) = parse_direction(direction)
    {
        return Action::ClusterTileSwap(direction);
    }
    if let ["monitor", "focus", direction] | ["monitor-focus", direction] = words.as_slice()
        && let Some(direction) = parse_direction(direction)
    {
        return Action::MonitorFocus(direction);
    }
    if let Some(direction) = s
        .strip_prefix("monitor-focus-")
        .or_else(|| s.strip_prefix("monitor_focus_"))
        .and_then(parse_direction)
    {
        return Action::MonitorFocus(direction);
    }
    if let Some(direction) = s
        .strip_prefix("cluster-tile-swap-")
        .or_else(|| s.strip_prefix("cluster_tile_swap_"))
        .and_then(parse_direction)
    {
        return Action::ClusterTileSwap(direction);
    }
    match s {
        "quit" => Action::Quit,
        "close-focused" | "close_focused" | "close-window" | "close_window" => {
            Action::CloseFocusedWindow
        }
        "toggle-fullscreen" | "toggle_fullscreen" | "fullscreen" => Action::ToggleFullscreen,
        "maximize-focused" | "maximize_focused" | "toggle-maximize" | "toggle_maximize" => {
            Action::ToggleFieldMaximize
        }
        "toggle-state" | "toggle_state" => Action::ToggleState,
        "apogee" | "overview" => Action::Apogee,
        "bearings-show" | "bearings_show" => Action::BearingsShow,
        "bearings-toggle" | "bearings_toggle" => Action::BearingsToggle,
        "cycle-focus" | "cycle_focus" => Action::FocusCycle(crate::FocusCycleDirection::Forward),
        "cycle-focus-backward" | "cycle_focus_backward" => {
            Action::FocusCycle(crate::FocusCycleDirection::Backward)
        }
        "cluster-mode" | "cluster_mode" => Action::ClusterMode,
        "cluster-layout cycle" | "cluster-layout-cycle" | "cluster_layout_cycle" => {
            Action::ClusterLayoutCycle
        }
        "open-terminal" | "open_terminal" => Action::OpenTerminal,
        "zoom-in" | "zoom_in" => Action::ZoomIn,
        "zoom-out" | "zoom_out" => Action::ZoomOut,
        "zoom-reset" | "zoom_reset" => Action::ZoomReset,
        "screenshot" => Action::Screenshot,
        command => Action::Spawn(command.to_string()),
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
        let action = parse_action(action_str);
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
    fn accepts_fullscreen_action_aliases() {
        for action in ["toggle-fullscreen", "toggle_fullscreen", "fullscreen"] {
            let kb = parse(&format!(
                "keybinds:\n  mod \"super\"\n  \"$var.mod+f\" \"{action}\"\nend\n"
            ));
            assert!(kb.binds.iter().any(|bind| {
                bind.key == "f"
                    && bind.modifiers.super_key
                    && bind.action == Action::ToggleFullscreen
            }));
        }
    }

    #[test]
    fn accepts_field_maximize_action_aliases() {
        for action in [
            "maximize-focused",
            "maximize_focused",
            "toggle-maximize",
            "toggle_maximize",
        ] {
            let kb = parse(&format!(
                "keybinds:\n  mod \"super\"\n  \"$var.mod+m\" \"{action}\"\nend\n"
            ));
            assert!(kb.binds.iter().any(|bind| {
                bind.key == "m"
                    && bind.modifiers.super_key
                    && bind.action == Action::ToggleFieldMaximize
            }));
        }
    }

    #[test]
    fn accepts_zoom_in_zoom_out_and_zoom_reset_actions() {
        let kb = parse(
            r#"
keybinds:
  mod "super"
  "$var.mod+equal" "zoom-in"
  "$var.mod+minus" "zoom-out"
  "$var.mod+click-middle" "zoom_reset"
end
"#,
        );
        let zoom_in = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomIn)
            .unwrap();
        assert_eq!(zoom_in.key, "equal");
        let zoom_out = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomOut)
            .unwrap();
        assert_eq!(zoom_out.key, "minus");
        let zoom_reset = kb
            .binds
            .iter()
            .find(|b| b.action == Action::ZoomReset)
            .unwrap();
        assert_eq!(zoom_reset.key, "click-middle");
        assert!(zoom_reset.modifiers.super_key);
    }

    #[test]
    fn parses_cluster_actions() {
        let kb = parse(
            r#"
keybinds:
  mod "super"
  "$var.mod+shift+c" "cluster-mode"
  "$var.mod+l" "cluster-layout cycle"
  "$var.mod+1" "cluster slot 1"
  "$var.mod+left" "cluster-focus-left"
  "$var.mod+ctrl+right" "tile swap right"
  "$var.mod+shift+up" "monitor-focus up"
end
"#,
        );

        for expected in [
            Action::ClusterMode,
            Action::ClusterLayoutCycle,
            Action::ClusterSlot(1),
            Action::ClusterTileFocus(crate::ClusterDirection::Left),
            Action::ClusterTileSwap(crate::ClusterDirection::Right),
            Action::MonitorFocus(crate::Direction::Up),
        ] {
            assert!(kb.binds.iter().any(|bind| bind.action == expected));
        }
    }

    #[test]
    fn accepts_screenshot_action() {
        let kb = parse(
            r#"
keybinds:
  mod "super"
  "Print" "screenshot"
end
"#,
        );
        assert!(kb.binds.iter().any(|bind| {
            bind.key == "Print"
                && bind.modifiers == crate::Modifiers::default()
                && bind.action == Action::Screenshot
        }));
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
        assert_eq!(
            kb.default_terminal,
            DefaultTerminal::Explicit("foot".to_string())
        );
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
    fn non_builtin_action_is_a_command_line() {
        let kb = parse(
            r#"
keybinds:
  mod "super"
  "$var.mod+d" "fuzzel"
  "$var.mod+shift+s" "grim -g \"$(slurp)\" ~/shot.png"
end
"#,
        );

        assert!(
            kb.binds.iter().any(|bind| {
                bind.key == "d" && bind.action == Action::Spawn("fuzzel".to_string())
            })
        );
        assert!(kb.binds.iter().any(|bind| {
            bind.key == "s"
                && bind.action == Action::Spawn("grim -g \"$(slurp)\" ~/shot.png".to_string())
        }));
    }

    #[test]
    fn missing_keybinds_section_errors() {
        let config = RuneConfig::from_str("other: \n foo \"bar\"\nend\n").unwrap();
        assert!(parse_keybinds(&config).is_err());
    }
}
