use std::collections::HashSet;
use std::fmt;

use rune_cfg::RuneConfig;
use rune_cfg::ast::{ObjectItem, Value};

const DEFAULT_REPEAT_RATE: i32 = 30;
const DEFAULT_REPEAT_DELAY: i32 = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusMode {
    #[default]
    Click,
    Hover,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardConfig {
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub model: String,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            layout: "us".to_string(),
            variant: String::new(),
            options: String::new(),
            model: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelProfile {
    Adaptive,
    Flat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollMethod {
    None,
    TwoFinger,
    Edge,
    OnButtonDown,
}

/// Settings which may be inherited from `input.mouse` or overridden for one
/// exact libinput device name. `None` means to use that device's own default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MouseSettings {
    pub enabled: Option<bool>,
    pub natural_scroll: Option<bool>,
    pub accel_speed: Option<f64>,
    pub accel_profile: Option<AccelProfile>,
    pub scroll_method: Option<ScrollMethod>,
    pub scroll_button: Option<u32>,
    pub left_handed: Option<bool>,
    pub middle_emulation: Option<bool>,
}

impl MouseSettings {
    pub fn overlay(&self, override_settings: &Self) -> Self {
        Self {
            enabled: override_settings.enabled.or(self.enabled),
            natural_scroll: override_settings.natural_scroll.or(self.natural_scroll),
            accel_speed: override_settings.accel_speed.or(self.accel_speed),
            accel_profile: override_settings.accel_profile.or(self.accel_profile),
            scroll_method: override_settings.scroll_method.or(self.scroll_method),
            scroll_button: override_settings.scroll_button.or(self.scroll_button),
            left_handed: override_settings.left_handed.or(self.left_handed),
            middle_emulation: override_settings.middle_emulation.or(self.middle_emulation),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceOverride {
    pub name: String,
    pub settings: MouseSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub repeat_rate: i32,
    pub repeat_delay: i32,
    pub focus_mode: FocusMode,
    pub raise_on_click: bool,
    pub keyboard: KeyboardConfig,
    pub mouse: MouseSettings,
    pub devices: Vec<DeviceOverride>,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            repeat_rate: DEFAULT_REPEAT_RATE,
            repeat_delay: DEFAULT_REPEAT_DELAY,
            focus_mode: FocusMode::Click,
            raise_on_click: true,
            keyboard: KeyboardConfig::default(),
            mouse: MouseSettings::default(),
            devices: Vec::new(),
        }
    }
}

impl Input {
    pub fn settings_for_device(&self, name: &str) -> MouseSettings {
        self.devices
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .map_or_else(
                || self.mouse.clone(),
                |entry| self.mouse.overlay(&entry.settings),
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputParseError(String);

impl fmt::Display for InputParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InputParseError {}

pub fn parse_input(config: &RuneConfig) -> Result<Input, InputParseError> {
    let Value::Object(root) = config
        .get_value("")
        .map_err(|err| InputParseError(format!("input config: {err}")))?
    else {
        return Err(InputParseError(
            "input config root must be an object".to_string(),
        ));
    };

    let Some(input_value) = field(&root, "input") else {
        return Ok(Input::default());
    };
    let input_fields = object(input_value, "input")?;
    let defaults = Input::default();

    let repeat_rate =
        optional_integer(input_fields, "repeat-rate", "input")?.unwrap_or(defaults.repeat_rate);
    if !(0..=1_000).contains(&repeat_rate) {
        return Err(InputParseError(
            "input.repeat-rate must be between 0 and 1000".to_string(),
        ));
    }
    let repeat_delay =
        optional_integer(input_fields, "repeat-delay", "input")?.unwrap_or(defaults.repeat_delay);
    if !(0..=10_000).contains(&repeat_delay) {
        return Err(InputParseError(
            "input.repeat-delay must be between 0 and 10000".to_string(),
        ));
    }

    let focus_mode = match optional_string(input_fields, "focus-mode", "input")?.as_deref() {
        None | Some("click") => FocusMode::Click,
        Some("hover") => FocusMode::Hover,
        Some(value) => {
            return Err(InputParseError(format!(
                "input.focus-mode must be \"click\" or \"hover\", got {value:?}"
            )));
        }
    };
    let raise_on_click = optional_bool(input_fields, "raise-on-click", "input")?.unwrap_or(true);

    let keyboard = match field(input_fields, "keyboard") {
        Some(value) => parse_keyboard(object(value, "input.keyboard")?)?,
        None => defaults.keyboard,
    };
    let mouse = match field(input_fields, "mouse") {
        Some(value) => parse_mouse_settings(object(value, "input.mouse")?, "input.mouse")?,
        None => MouseSettings::default(),
    };
    let devices = match field(input_fields, "devices") {
        Some(value) => parse_devices(object(value, "input.devices")?)?,
        None => Vec::new(),
    };

    Ok(Input {
        repeat_rate,
        repeat_delay,
        focus_mode,
        raise_on_click,
        keyboard,
        mouse,
        devices,
    })
}

fn parse_keyboard(fields: &[ObjectItem]) -> Result<KeyboardConfig, InputParseError> {
    let defaults = KeyboardConfig::default();
    Ok(KeyboardConfig {
        layout: optional_string(fields, "layout", "input.keyboard")?.unwrap_or(defaults.layout),
        variant: optional_string(fields, "variant", "input.keyboard")?.unwrap_or(defaults.variant),
        options: optional_string(fields, "options", "input.keyboard")?.unwrap_or(defaults.options),
        model: optional_string(fields, "model", "input.keyboard")?.unwrap_or(defaults.model),
    })
}

fn parse_devices(fields: &[ObjectItem]) -> Result<Vec<DeviceOverride>, InputParseError> {
    let mut seen = HashSet::new();
    let mut devices = Vec::new();
    for item in fields {
        let ObjectItem::Assign(name, value) = item else {
            continue;
        };
        let normalized = name.trim().to_lowercase();
        if normalized.is_empty() {
            return Err(InputParseError(
                "input.devices names must not be empty".to_string(),
            ));
        }
        if !seen.insert(normalized) {
            return Err(InputParseError(format!(
                "duplicate input.devices entry for {name:?}"
            )));
        }
        let path = format!("input.devices.{name}");
        devices.push(DeviceOverride {
            name: name.trim().to_string(),
            settings: parse_mouse_settings(object(value, &path)?, &path)?,
        });
    }
    Ok(devices)
}

fn parse_mouse_settings(
    fields: &[ObjectItem],
    path: &str,
) -> Result<MouseSettings, InputParseError> {
    let accel_speed = optional_number(fields, "accel-speed", path)?;
    if accel_speed.is_some_and(|speed| !(-1.0..=1.0).contains(&speed)) {
        return Err(InputParseError(format!(
            "{path}.accel-speed must be between -1.0 and 1.0"
        )));
    }

    let accel_profile = match optional_string(fields, "accel-profile", path)?.as_deref() {
        None => None,
        Some("adaptive") => Some(AccelProfile::Adaptive),
        Some("flat") => Some(AccelProfile::Flat),
        Some(value) => {
            return Err(InputParseError(format!(
                "{path}.accel-profile must be \"adaptive\" or \"flat\", got {value:?}"
            )));
        }
    };
    let scroll_method = match optional_string(fields, "scroll-method", path)?.as_deref() {
        None => None,
        Some("none") => Some(ScrollMethod::None),
        Some("two-finger") => Some(ScrollMethod::TwoFinger),
        Some("edge") => Some(ScrollMethod::Edge),
        Some("on-button-down") => Some(ScrollMethod::OnButtonDown),
        Some(value) => {
            return Err(InputParseError(format!(
                "{path}.scroll-method must be \"none\", \"two-finger\", \"edge\", or \
                 \"on-button-down\", got {value:?}"
            )));
        }
    };

    Ok(MouseSettings {
        enabled: optional_bool(fields, "enabled", path)?,
        natural_scroll: optional_bool(fields, "natural-scroll", path)?,
        accel_speed,
        accel_profile,
        scroll_method,
        scroll_button: optional_u32(fields, "scroll-button", path)?,
        left_handed: optional_bool(fields, "left-handed", path)?,
        middle_emulation: optional_bool(fields, "middle-emulation", path)?,
    })
}

fn field<'a>(fields: &'a [ObjectItem], key: &str) -> Option<&'a Value> {
    fields.iter().find_map(|item| match item {
        ObjectItem::Assign(candidate, value) if candidate == key => Some(value),
        _ => None,
    })
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a [ObjectItem], InputParseError> {
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(InputParseError(format!("{path} must be a block"))),
    }
}

fn optional_string(
    fields: &[ObjectItem],
    key: &str,
    path: &str,
) -> Result<Option<String>, InputParseError> {
    match field(fields, key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(InputParseError(format!("{path}.{key} must be a string"))),
    }
}

fn optional_bool(
    fields: &[ObjectItem],
    key: &str,
    path: &str,
) -> Result<Option<bool>, InputParseError> {
    match field(fields, key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(InputParseError(format!("{path}.{key} must be a boolean"))),
    }
}

fn optional_number(
    fields: &[ObjectItem],
    key: &str,
    path: &str,
) -> Result<Option<f64>, InputParseError> {
    match field(fields, key) {
        None => Ok(None),
        Some(Value::Number(value)) if value.is_finite() => Ok(Some(*value)),
        Some(_) => Err(InputParseError(format!(
            "{path}.{key} must be a finite number"
        ))),
    }
}

fn optional_integer(
    fields: &[ObjectItem],
    key: &str,
    path: &str,
) -> Result<Option<i32>, InputParseError> {
    optional_number(fields, key, path)?.map_or(Ok(None), |value| {
        if value.fract() == 0.0 && value >= i32::MIN as f64 && value <= i32::MAX as f64 {
            Ok(Some(value as i32))
        } else {
            Err(InputParseError(format!("{path}.{key} must be an integer")))
        }
    })
}

fn optional_u32(
    fields: &[ObjectItem],
    key: &str,
    path: &str,
) -> Result<Option<u32>, InputParseError> {
    optional_number(fields, key, path)?.map_or(Ok(None), |value| {
        if value.fract() == 0.0 && value >= 0.0 && value <= u32::MAX as f64 {
            Ok(Some(value as u32))
        } else {
            Err(InputParseError(format!(
                "{path}.{key} must be a non-negative integer"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Result<Input, InputParseError> {
        let config = RuneConfig::from_str(source).expect("syntactically valid Rune");
        parse_input(&config)
    }

    #[test]
    fn parses_keyboard_focus_and_mouse_settings() {
        let input = parse(
            r#"
input:
  repeat-rate 45
  repeat-delay 350
  focus-mode "hover"
  raise-on-click false
  keyboard:
    layout "ca"
    variant "multix"
    options "caps:escape"
    model "pc105"
  end
  mouse:
    enabled true
    natural-scroll true
    accel-speed 0.25
    accel-profile "flat"
    scroll-method "on-button-down"
    scroll-button 274
    left-handed true
    middle-emulation true
  end
end
"#,
        )
        .unwrap();

        assert_eq!(input.repeat_rate, 45);
        assert_eq!(input.repeat_delay, 350);
        assert_eq!(input.focus_mode, FocusMode::Hover);
        assert!(!input.raise_on_click);
        assert_eq!(input.keyboard.layout, "ca");
        assert_eq!(input.keyboard.variant, "multix");
        assert_eq!(input.mouse.accel_speed, Some(0.25));
        assert_eq!(input.mouse.accel_profile, Some(AccelProfile::Flat));
        assert_eq!(input.mouse.scroll_method, Some(ScrollMethod::OnButtonDown));
        assert_eq!(input.mouse.scroll_button, Some(274));
    }

    #[test]
    fn exact_case_insensitive_override_layers_over_mouse() {
        let input = parse(
            r#"
input:
  mouse:
    accel-speed 0.4
    natural-scroll false
  end
  devices:
    "Logitech MX Master 3":
      natural-scroll true
    end
  end
end
"#,
        )
        .unwrap();

        let matching = input.settings_for_device("logitech mx master 3");
        assert_eq!(matching.accel_speed, Some(0.4));
        assert_eq!(matching.natural_scroll, Some(true));
        assert_eq!(
            input.settings_for_device("Logitech MX Master 3S"),
            input.mouse
        );
    }

    #[test]
    fn rejects_duplicate_normalized_device_names() {
        let err = parse(
            r#"
input:
  devices:
    "Mouse":
      enabled true
    end
    "mouse":
      enabled false
    end
  end
end
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_invalid_ranges_types_and_enums() {
        for (source, expected) in [
            ("input:\n  repeat-rate 1001\nend\n", "repeat-rate"),
            ("input:\n  repeat-delay 10001\nend\n", "repeat-delay"),
            ("input:\n  focus-mode \"sloppy\"\nend\n", "focus-mode"),
            (
                "input:\n  mouse:\n    accel-speed 1.1\n  end\nend\n",
                "accel-speed",
            ),
            (
                "input:\n  mouse:\n    accel-profile \"magic\"\n  end\nend\n",
                "accel-profile",
            ),
            (
                "input:\n  mouse:\n    natural-scroll \"yes\"\n  end\nend\n",
                "natural-scroll",
            ),
        ] {
            assert!(
                parse(source)
                    .expect_err("invalid input should fail")
                    .to_string()
                    .contains(expected),
                "{source:?} did not mention {expected:?}"
            );
        }
    }

    #[test]
    fn omitted_mouse_values_preserve_device_defaults() {
        let input = parse("input:\nend\n").unwrap();
        assert_eq!(input.mouse, MouseSettings::default());
        assert!(input.devices.is_empty());
    }
}
