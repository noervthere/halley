use std::fmt;

use rune_cfg::RuneConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientBlurMode {
    Off,
    Auto,
    Always,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlurMethod {
    DualKawase,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blur {
    pub enabled: bool,
    pub overlays: bool,
    pub windows: ClientBlurMode,
    pub layer_shell: ClientBlurMode,
    pub method: BlurMethod,
    pub radius: f32,
    pub passes: u32,
    pub saturation: f32,
    pub noise: f32,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            enabled: false,
            overlays: true,
            windows: ClientBlurMode::Auto,
            layer_shell: ClientBlurMode::Off,
            method: BlurMethod::DualKawase,
            radius: 24.0,
            passes: 3,
            saturation: 1.10,
            noise: 0.012,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Effects {
    pub blur: Blur,
}

#[derive(Debug)]
pub enum EffectsParseError {
    Rune(rune_cfg::RuneError),
    InvalidValue { path: &'static str, value: String },
}

impl fmt::Display for EffectsParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rune(error) => write!(f, "{error}"),
            Self::InvalidValue { path, value } => {
                write!(f, "invalid value {value:?} for {path}")
            }
        }
    }
}

impl std::error::Error for EffectsParseError {}

impl From<rune_cfg::RuneError> for EffectsParseError {
    fn from(value: rune_cfg::RuneError) -> Self {
        Self::Rune(value)
    }
}

pub fn parse_effects(config: &RuneConfig) -> Result<Effects, EffectsParseError> {
    let defaults = Blur::default();
    let radius = finite_clamp(
        config.get_or("effects.blur.radius", defaults.radius),
        0.0,
        128.0,
        defaults.radius,
    );
    let saturation = finite_clamp(
        config.get_or("effects.blur.saturation", defaults.saturation),
        0.0,
        4.0,
        defaults.saturation,
    );
    let noise = finite_clamp(
        config.get_or("effects.blur.noise", defaults.noise),
        0.0,
        0.25,
        defaults.noise,
    );
    Ok(Effects {
        blur: Blur {
            enabled: config.get_or("effects.blur.enabled", defaults.enabled),
            overlays: config.get_or("effects.blur.overlays", defaults.overlays),
            windows: parse_mode(config, "effects.blur.windows", defaults.windows)?,
            layer_shell: parse_mode(config, "effects.blur.layer-shell", defaults.layer_shell)?,
            method: parse_method(config, defaults.method)?,
            radius,
            passes: config
                .get_or("effects.blur.passes", defaults.passes)
                .clamp(1, 5),
            saturation,
            noise,
        },
    })
}

fn parse_mode(
    config: &RuneConfig,
    path: &'static str,
    default: ClientBlurMode,
) -> Result<ClientBlurMode, EffectsParseError> {
    let Some(value) = config.get_optional::<String>(path)? else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(ClientBlurMode::Off),
        "auto" => Ok(ClientBlurMode::Auto),
        "always" => Ok(ClientBlurMode::Always),
        _ => Err(EffectsParseError::InvalidValue { path, value }),
    }
}

fn parse_method(config: &RuneConfig, default: BlurMethod) -> Result<BlurMethod, EffectsParseError> {
    let Some(value) = config.get_optional::<String>("effects.blur.method")? else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "dual-kawase" | "dual_kawase" | "kawase" => Ok(BlurMethod::DualKawase),
        _ => Err(EffectsParseError::InvalidValue {
            path: "effects.blur.method",
            value,
        }),
    }
}

fn finite_clamp(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_old_halley() {
        let blur = Blur::default();
        assert!(!blur.enabled);
        assert!(blur.overlays);
        assert_eq!(blur.windows, ClientBlurMode::Auto);
        assert_eq!(blur.layer_shell, ClientBlurMode::Off);
        assert_eq!(blur.method, BlurMethod::DualKawase);
        assert_eq!(blur.radius, 24.0);
        assert_eq!(blur.passes, 3);
        assert_eq!(blur.saturation, 1.10);
        assert_eq!(blur.noise, 0.012);
    }

    #[test]
    fn parses_and_bounds_blur_knobs() {
        let config = RuneConfig::from_str(
            r#"
effects:
  blur:
    enabled true
    overlays false
    windows "always"
    layer-shell "auto"
    method "dual-kawase"
    radius 30.0
    passes 9
    saturation 1.2
    noise 0.02
  end
end
"#,
        )
        .unwrap();
        let blur = parse_effects(&config).unwrap().blur;
        assert!(blur.enabled);
        assert!(!blur.overlays);
        assert_eq!(blur.windows, ClientBlurMode::Always);
        assert_eq!(blur.layer_shell, ClientBlurMode::Auto);
        assert_eq!(blur.radius, 30.0);
        assert_eq!(blur.passes, 5);
        assert_eq!(blur.saturation, 1.2);
        assert_eq!(blur.noise, 0.02);
    }

    #[test]
    fn rejects_unknown_policy_and_method() {
        for source in [
            "effects:\n  blur:\n    windows \"sometimes\"\n  end\nend\n",
            "effects:\n  blur:\n    method \"gaussian\"\n  end\nend\n",
        ] {
            let config = RuneConfig::from_str(source).unwrap();
            assert!(matches!(
                parse_effects(&config),
                Err(EffectsParseError::InvalidValue { .. })
            ));
        }
    }
}
