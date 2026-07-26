use std::collections::HashSet;
use std::fmt;

use rune_cfg::RuneConfig;
use rune_cfg::ast::{ObjectItem, Value};

/// Variable refresh rate mode for one output. `Auto` parses and stores like
/// old halley's own `OnDemand` - honestly unimplemented for now (no
/// per-content signal exists yet to drive it), so it behaves like `Off`
/// until there's something real to base that decision on. Only `On` calls
/// the real DRM VRR toggle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Vrr {
    #[default]
    Off,
    On,
    Auto,
}

/// One physical monitor's configuration - hardware/mode/position only,
/// deliberately narrow (see `output.rs`'s module doc). No `enabled` field:
/// comment the whole block out in the config file to disable a monitor,
/// matching old halley's own simplest working pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputConfig {
    /// Connector name, e.g. "DP-1" - matched against real connector names
    /// at startup.
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub offset_x: i32,
    pub offset_y: i32,
    /// `None` means "use the highest advertised refresh at this exact
    /// resolution" - when `Some`, the backend requires an exact
    /// integer-millihertz match (see `backend/tty.rs`), not a closest/fuzzy
    /// match.
    pub rate: Option<f64>,
    /// Raw degrees - 0/90/180/270. Any other value falls back to 0, same
    /// as old halley's own clamp (its legacy 1/2/3 shorthand isn't ported -
    /// no reason to keep an alternate spelling this project never used).
    pub transform: u16,
    pub vrr: Vrr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputParseError(String);

impl fmt::Display for OutputParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OutputParseError {}

/// Parses every top-level `output:` block in the config into one
/// `OutputConfig` each, in file order.
///
/// This is the one config module in this crate that can't use the tidy
/// `config.get_or("path", default)`-style dotted-path helpers every other
/// module (`decorations.rs`, `zoom.rs`, `keybinds.rs`) uses: those helpers
/// resolve a path to the *first* matching key, but a monitor setup means
/// writing multiple sibling `output:` blocks with the same key name (not one
/// wrapper section with per-connector children, which is what old halley did
/// and what this format deliberately avoids - see the plan). rune-cfg's
/// parser never deduplicates repeated top-level keys (confirmed by reading
/// `rune_cfg::config::mod::RuneConfig::resolved_root`, which folds
/// `Document.items: Vec<(String, Value)>` - an ordered list, not a map -
/// into the resolved tree), so reading the whole document as a raw
/// `Value::Object` once and filtering for every `("output", _)` entry
/// directly is the only way to see all of them.
pub fn parse_outputs(config: &RuneConfig) -> Vec<OutputConfig> {
    let Ok(Value::Object(root_items)) = config.get_value("") else {
        return Vec::new();
    };

    root_items
        .iter()
        .filter_map(|item| match item {
            ObjectItem::Assign(key, value) if key == "output" => Some(value),
            _ => None,
        })
        .filter_map(|value| {
            let Value::Object(fields) = value else {
                return None;
            };
            parse_one_output(fields)
        })
        .collect()
}

/// Strict output parsing for atomic live reload. The tolerant public parser
/// above remains useful for its existing load-or-default callers, but a
/// half-written output block must not make a live compositor temporarily
/// drop that connector's last valid configuration.
pub fn parse_outputs_checked(config: &RuneConfig) -> Result<Vec<OutputConfig>, OutputParseError> {
    let Value::Object(root_items) = config
        .get_value("")
        .map_err(|err| OutputParseError(format!("output config: {err}")))?
    else {
        return Err(OutputParseError(
            "output config root must be an object".to_string(),
        ));
    };

    let mut outputs = Vec::new();
    let mut names = HashSet::new();
    for value in root_items.iter().filter_map(|item| match item {
        ObjectItem::Assign(key, value) if key == "output" => Some(value),
        _ => None,
    }) {
        let Value::Object(fields) = value else {
            return Err(OutputParseError(
                "output block must be an object".to_string(),
            ));
        };
        validate_output_fields(fields)?;
        let output = parse_one_output(fields)
            .expect("validated output fields must produce an output config");
        if !names.insert(output.name.clone()) {
            return Err(OutputParseError(format!(
                "duplicate output block for {:?}",
                output.name
            )));
        }
        outputs.push(output);
    }

    Ok(outputs)
}

fn validate_output_fields(fields: &[ObjectItem]) -> Result<(), OutputParseError> {
    let name = field(fields, &["name"])
        .and_then(as_str)
        .ok_or_else(|| OutputParseError("output block requires a string name".to_string()))?;
    let width = field(fields, &["width"])
        .and_then(as_i32)
        .ok_or_else(|| OutputParseError(format!("output {name:?} requires a numeric width")))?;
    let height = field(fields, &["height"])
        .and_then(as_i32)
        .ok_or_else(|| OutputParseError(format!("output {name:?} requires a numeric height")))?;
    if width <= 0 || height <= 0 {
        return Err(OutputParseError(format!(
            "output {name:?}: width/height must be positive"
        )));
    }

    for keys in [&["offset-x", "offset_x"][..], &["offset-y", "offset_y"][..]] {
        if field(fields, keys).is_some_and(|value| as_i32(value).is_none()) {
            return Err(OutputParseError(format!(
                "output {name:?}: {} must be numeric",
                keys[0]
            )));
        }
    }

    if let Some(value) = field(fields, &["rate", "refresh-rate", "refresh_rate"]) {
        let Some(rate) = as_f64(value) else {
            return Err(OutputParseError(format!(
                "output {name:?}: rate must be numeric"
            )));
        };
        if !rate.is_finite() || rate <= 0.0 {
            return Err(OutputParseError(format!(
                "output {name:?}: rate must be positive and finite"
            )));
        }
    }

    if let Some(value) = field(fields, &["transform", "rotation"]) {
        let Some(transform) = as_u16(value) else {
            return Err(OutputParseError(format!(
                "output {name:?}: transform must be numeric"
            )));
        };
        if !matches!(transform, 0 | 90 | 180 | 270) {
            return Err(OutputParseError(format!(
                "output {name:?}: transform must be 0, 90, 180, or 270"
            )));
        }
    }

    if let Some(value) = field(fields, &["vrr"]) {
        let Some(vrr) = as_str(value) else {
            return Err(OutputParseError(format!(
                "output {name:?}: vrr must be a string"
            )));
        };
        if !matches!(
            vrr.to_ascii_lowercase().as_str(),
            "on" | "true" | "off" | "false" | "auto" | "on-demand" | "on_demand" | "ondemand"
        ) {
            return Err(OutputParseError(format!(
                "output {name:?}: unknown vrr mode {vrr:?}"
            )));
        }
    }

    Ok(())
}

fn parse_one_output(fields: &[ObjectItem]) -> Option<OutputConfig> {
    let name = as_str(field(fields, &["name"])?)?;
    let width = as_i32(field(fields, &["width"])?)?;
    let height = as_i32(field(fields, &["height"])?)?;
    if width <= 0 || height <= 0 {
        eprintln!("output {name:?}: width/height must be positive, skipping");
        return None;
    }

    let offset_x = field(fields, &["offset-x", "offset_x"])
        .and_then(as_i32)
        .unwrap_or(0);
    let offset_y = field(fields, &["offset-y", "offset_y"])
        .and_then(as_i32)
        .unwrap_or(0);
    let rate = field(fields, &["rate", "refresh-rate", "refresh_rate"]).and_then(as_f64);
    let transform = field(fields, &["transform", "rotation"])
        .and_then(as_u16)
        .map(|degrees| match degrees {
            0 | 90 | 180 | 270 => degrees,
            other => {
                eprintln!("output {name:?}: invalid transform {other}, falling back to 0");
                0
            }
        })
        .unwrap_or(0);
    let vrr = field(fields, &["vrr"])
        .and_then(as_str)
        .map(|raw| parse_vrr(&raw))
        .unwrap_or_default();

    Some(OutputConfig {
        name,
        width,
        height,
        offset_x,
        offset_y,
        rate,
        transform,
        vrr,
    })
}

fn parse_vrr(raw: &str) -> Vrr {
    match raw.to_ascii_lowercase().as_str() {
        "on" | "true" => Vrr::On,
        "auto" | "on-demand" | "on_demand" | "ondemand" => Vrr::Auto,
        _ => Vrr::Off,
    }
}

fn field<'a>(fields: &'a [ObjectItem], keys: &[&str]) -> Option<&'a Value> {
    fields.iter().find_map(|item| match item {
        ObjectItem::Assign(key, value) if keys.contains(&key.as_str()) => Some(value),
        _ => None,
    })
}

fn as_str(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

fn as_i32(value: &Value) -> Option<i32> {
    as_f64(value).map(|n| n as i32)
}

fn as_u16(value: &Value) -> Option<u16> {
    as_f64(value).map(|n| n as u16)
}

/// Loads the configured outputs, falling back to an empty list (auto-detect
/// every connected connector's default mode, handled entirely in the
/// backend) on any failure - a config typo shouldn't crash compositor
/// startup, matching `load_decorations`/`load_zoom`/`load_keybinds`.
pub fn load_outputs() -> Vec<OutputConfig> {
    let Some(path) = crate::config_path() else {
        eprintln!("output: no config path resolvable, using auto-detected outputs");
        return Vec::new();
    };

    if let Err(err) = crate::bootstrap_default_config_at(&path) {
        eprintln!("output: failed to bootstrap default config: {err}");
    }

    match RuneConfig::from_file(&path) {
        Ok(config) => parse_outputs(&config),
        Err(err) => {
            eprintln!("output: failed to load {path:?}, using auto-detected outputs: {err}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_output_block() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
  width 2560
  height 1440
  offset-x 0
  offset-y 0
  rate 179.998
  transform 0
  vrr "auto"
end
"##,
        )
        .expect("valid rune-cfg source");

        let outputs = parse_outputs(&config);
        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0],
            OutputConfig {
                name: "DP-1".to_string(),
                width: 2560,
                height: 1440,
                offset_x: 0,
                offset_y: 0,
                rate: Some(179.998),
                transform: 0,
                vrr: Vrr::Auto,
            }
        );
    }

    #[test]
    fn parses_multiple_repeated_output_blocks_in_order() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
  width 2560
  height 1440
  offset-x 0
  offset-y 0
end

output:
  name "HDMI-A-1"
  width 1920
  height 1080
  offset-x 2560
  offset-y 0
end
"##,
        )
        .expect("valid rune-cfg source");

        let outputs = parse_outputs(&config);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].name, "DP-1");
        assert_eq!(outputs[1].name, "HDMI-A-1");
        assert_eq!(outputs[1].offset_x, 2560);
    }

    #[test]
    fn missing_width_skips_entry_but_keeps_the_rest() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
end

output:
  name "HDMI-A-1"
  width 1920
  height 1080
end
"##,
        )
        .expect("valid rune-cfg source");

        let outputs = parse_outputs(&config);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "HDMI-A-1");
    }

    #[test]
    fn no_output_sections_returns_empty() {
        let config = RuneConfig::from_str("keybinds:\n  mod \"super\"\nend\n")
            .expect("valid rune-cfg source");
        assert_eq!(parse_outputs(&config), Vec::new());
    }

    #[test]
    fn vrr_defaults_to_off_when_absent() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
  width 2560
  height 1440
end
"##,
        )
        .expect("valid rune-cfg source");

        assert_eq!(parse_outputs(&config)[0].vrr, Vrr::Off);
    }

    #[test]
    fn vrr_accepts_on_and_auto_spellings() {
        for (raw, expected) in [
            ("on", Vrr::On),
            ("true", Vrr::On),
            ("off", Vrr::Off),
            ("auto", Vrr::Auto),
            ("on-demand", Vrr::Auto),
        ] {
            assert_eq!(parse_vrr(raw), expected, "vrr {raw:?}");
        }
    }

    #[test]
    fn invalid_transform_falls_back_to_zero() {
        let config = RuneConfig::from_str(
            r##"
output:
  name "DP-1"
  width 2560
  height 1440
  transform 45
end
"##,
        )
        .expect("valid rune-cfg source");

        assert_eq!(parse_outputs(&config)[0].transform, 0);
    }

    #[test]
    fn accepts_valid_transform_values() {
        for degrees in [0, 90, 180, 270] {
            let config = RuneConfig::from_str(&format!(
                "output:\n  name \"DP-1\"\n  width 2560\n  height 1440\n  transform {degrees}\nend\n"
            ))
            .expect("valid rune-cfg source");
            assert_eq!(parse_outputs(&config)[0].transform, degrees);
        }
    }
}
