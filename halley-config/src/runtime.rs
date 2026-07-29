use std::fmt;
use std::path::Path;

use rune_cfg::RuneConfig;

use crate::{
    Animations, Cursor, Decorations, Input, InputParseError, Keybinds, OutputConfig,
    OutputParseError, Screenshot, Zoom, parse_animations, parse_cursor, parse_decorations,
    parse_input, parse_keybinds, parse_outputs_checked, parse_screenshot, parse_zoom,
};

/// One validated snapshot of every setting the running compositor currently
/// understands. Loading the file once avoids independently parsing the same
/// bytes for each subsystem and gives live reload a single atomic unit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeConfig {
    pub keybinds: Keybinds,
    pub decorations: Decorations,
    pub zoom: Zoom,
    pub screenshot: Screenshot,
    pub cursor: Cursor,
    pub input: Input,
    pub animations: Animations,
    pub outputs: Vec<OutputConfig>,
}

#[derive(Debug)]
pub enum RuntimeConfigError {
    Rune(rune_cfg::RuneError),
    Keybind(crate::ParseError),
    Input(InputParseError),
    Output(OutputParseError),
}

impl fmt::Display for RuntimeConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rune(err) => write!(f, "{err}"),
            Self::Keybind(err) => write!(f, "{err}"),
            Self::Input(err) => write!(f, "{err}"),
            Self::Output(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RuntimeConfigError {}

impl From<rune_cfg::RuneError> for RuntimeConfigError {
    fn from(value: rune_cfg::RuneError) -> Self {
        Self::Rune(value)
    }
}

impl From<crate::ParseError> for RuntimeConfigError {
    fn from(value: crate::ParseError) -> Self {
        Self::Keybind(value)
    }
}

impl From<OutputParseError> for RuntimeConfigError {
    fn from(value: OutputParseError) -> Self {
        Self::Output(value)
    }
}

impl From<InputParseError> for RuntimeConfigError {
    fn from(value: InputParseError) -> Self {
        Self::Input(value)
    }
}

pub fn parse_runtime_config(config: &RuneConfig) -> Result<RuntimeConfig, RuntimeConfigError> {
    Ok(RuntimeConfig {
        keybinds: parse_keybinds(config)?,
        decorations: parse_decorations(config),
        zoom: parse_zoom(config),
        screenshot: parse_screenshot(config),
        cursor: parse_cursor(config),
        input: parse_input(config)?,
        animations: parse_animations(config),
        outputs: parse_outputs_checked(config)?,
    })
}

pub fn load_runtime_config_at(path: &Path) -> Result<RuntimeConfig, RuntimeConfigError> {
    let config = RuneConfig::from_file(path)?;
    parse_runtime_config(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_supported_sections_as_one_snapshot() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
  width 2560
  height 1440
end

zoom:
  enabled false
end

screenshot:
  directory "/tmp/screenshots"
end

cursor:
  theme "Breeze"
  size 32
  hide-when-typing true
  hide-after-ms 750
end

input:
  repeat-rate 45
  focus-mode "hover"
end

decorations:
  border:
    size 7
  end
end

animations:
  enabled false
end

keybinds:
  mod "super"
  "$var.mod+t" "open-terminal"
end
"##,
        )
        .expect("valid Rune config");

        let runtime = parse_runtime_config(&config).unwrap();
        assert_eq!(runtime.outputs.len(), 1);
        assert!(!runtime.zoom.enabled);
        assert_eq!(runtime.screenshot.directory, "/tmp/screenshots");
        assert_eq!(runtime.cursor.theme, "Breeze");
        assert_eq!(runtime.cursor.size, 32);
        assert!(runtime.cursor.hide_when_typing);
        assert_eq!(runtime.cursor.hide_after_ms, Some(750));
        assert_eq!(runtime.input.repeat_rate, 45);
        assert_eq!(runtime.input.focus_mode, crate::FocusMode::Hover);
        assert_eq!(runtime.decorations.border_width_px, 7);
        assert!(!runtime.animations.enabled);
        assert_eq!(runtime.keybinds.binds.len(), 1);
    }

    #[test]
    fn rejects_an_incomplete_output_block() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
  width 2560
end

keybinds:
  mod "super"
end
"##,
        )
        .expect("syntactically valid Rune config");

        assert!(matches!(
            parse_runtime_config(&config),
            Err(RuntimeConfigError::Output(_))
        ));
    }

    #[test]
    fn valid_keybind_section_is_authoritative_without_embedded_defaults() {
        let config = RuneConfig::from_str(
            r#"
keybinds:
  mod "super"
  "$var.mod+x" "open-terminal"
end
"#,
        )
        .expect("valid Rune config");

        let runtime = parse_runtime_config(&config).unwrap();
        assert_eq!(runtime.keybinds.binds.len(), 1);
        assert_eq!(runtime.keybinds.binds[0].key, "x");
        assert_eq!(
            runtime.keybinds.binds[0].action,
            crate::Action::OpenTerminal
        );
    }

    #[test]
    fn empty_config_is_invalid_while_runtime_default_is_complete() {
        let empty = RuneConfig::from_str("").expect("empty Rune source is syntactically valid");

        assert!(parse_runtime_config(&empty).is_err());
        assert!(!RuntimeConfig::default().keybinds.binds.is_empty());
    }
}
