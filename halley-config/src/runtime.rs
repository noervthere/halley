use std::fmt;
use std::path::Path;

use rune_cfg::RuneConfig;

use crate::{
    Animations, Decorations, Keybinds, OutputConfig, OutputParseError, Screenshot, Zoom,
    parse_animations, parse_decorations, parse_keybinds, parse_outputs_checked, parse_screenshot,
    parse_zoom,
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
    pub animations: Animations,
    pub outputs: Vec<OutputConfig>,
}

#[derive(Debug)]
pub enum RuntimeConfigError {
    Rune(rune_cfg::RuneError),
    Keybind(crate::ParseError),
    Output(OutputParseError),
}

impl fmt::Display for RuntimeConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rune(err) => write!(f, "{err}"),
            Self::Keybind(err) => write!(f, "{err}"),
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

pub fn parse_runtime_config(config: &RuneConfig) -> Result<RuntimeConfig, RuntimeConfigError> {
    Ok(RuntimeConfig {
        keybinds: parse_keybinds(config)?,
        decorations: parse_decorations(config),
        zoom: parse_zoom(config),
        screenshot: parse_screenshot(config),
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
}
