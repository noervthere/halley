use std::fmt;

use rune_cfg::RuneConfig;

pub const DEFAULT_SUCCESS_DURATION_MS: u64 = 4_000;
pub const DEFAULT_ERROR_DURATION_MS: u64 = 9_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayColorMode {
    Auto,
    Light,
    Dark,
    Fixed { r: f32, g: f32, b: f32, a: f32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayShape {
    Square,
    Rounded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayBorderSource {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Notifications {
    pub position: NotificationPosition,
    pub success_duration_ms: u64,
    pub error_duration_ms: u64,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            position: NotificationPosition::TopCenter,
            success_duration_ms: DEFAULT_SUCCESS_DURATION_MS,
            error_duration_ms: DEFAULT_ERROR_DURATION_MS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlays {
    pub background_color: OverlayColorMode,
    pub text_color: OverlayColorMode,
    pub error_color: OverlayColorMode,
    pub shape: OverlayShape,
    pub borders: bool,
    pub border_source: OverlayBorderSource,
    pub blur: bool,
    pub notifications: Notifications,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            background_color: OverlayColorMode::Auto,
            text_color: OverlayColorMode::Auto,
            error_color: OverlayColorMode::Fixed {
                r: 0xfb as f32 / 255.0,
                g: 0x49 as f32 / 255.0,
                b: 0x34 as f32 / 255.0,
                a: 1.0,
            },
            shape: OverlayShape::Square,
            borders: true,
            border_source: OverlayBorderSource::Primary,
            blur: true,
            notifications: Notifications::default(),
        }
    }
}

#[derive(Debug)]
pub enum OverlayParseError {
    Rune(rune_cfg::RuneError),
    InvalidValue { path: &'static str, value: String },
    InvalidDuration { path: &'static str, value: u64 },
}

impl fmt::Display for OverlayParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rune(error) => write!(f, "{error}"),
            Self::InvalidValue { path, value } => {
                write!(f, "invalid value {value:?} for {path}")
            }
            Self::InvalidDuration { path, value } => {
                write!(f, "{path} must be greater than zero, got {value}")
            }
        }
    }
}

impl std::error::Error for OverlayParseError {}

impl From<rune_cfg::RuneError> for OverlayParseError {
    fn from(value: rune_cfg::RuneError) -> Self {
        Self::Rune(value)
    }
}

pub fn parse_overlays_checked(config: &RuneConfig) -> Result<Overlays, OverlayParseError> {
    let defaults = Overlays::default();
    let background_color = parse_color(
        optional_string(
            config,
            &[
                "overlays.background-colour",
                "overlays.background-color",
                "overlay.background-colour",
                "overlay.background-color",
            ],
        )?,
        "overlays.background-colour",
        defaults.background_color,
    )?;
    let text_color = parse_color(
        optional_string(
            config,
            &[
                "overlays.text-colour",
                "overlays.text-color",
                "overlay.text-colour",
                "overlay.text-color",
            ],
        )?,
        "overlays.text-colour",
        defaults.text_color,
    )?;
    let error_color = parse_color(
        optional_string(
            config,
            &[
                "overlays.error-colour",
                "overlays.error-color",
                "overlay.error-colour",
                "overlay.error-color",
            ],
        )?,
        "overlays.error-colour",
        defaults.error_color,
    )?;
    let shape = match optional_string(config, &["overlays.shape", "overlay.shape"])?
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None => defaults.shape,
        Some("square") => OverlayShape::Square,
        Some("rounded") => OverlayShape::Rounded,
        Some(value) => {
            return Err(OverlayParseError::InvalidValue {
                path: "overlays.shape",
                value: value.to_string(),
            });
        }
    };
    let border_source =
        match optional_string(config, &["overlays.border-source", "overlay.border-source"])?
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None => defaults.border_source,
            Some("primary") => OverlayBorderSource::Primary,
            Some("secondary") => OverlayBorderSource::Secondary,
            Some(value) => {
                return Err(OverlayParseError::InvalidValue {
                    path: "overlays.border-source",
                    value: value.to_string(),
                });
            }
        };
    let position = match optional_string(
        config,
        &[
            "overlays.notifications.position",
            "overlay.notifications.position",
        ],
    )?
    .as_deref()
    .map(str::to_ascii_lowercase)
    .as_deref()
    {
        None => defaults.notifications.position,
        Some("top-left") => NotificationPosition::TopLeft,
        Some("top-center") => NotificationPosition::TopCenter,
        Some("top-right") => NotificationPosition::TopRight,
        Some("bottom-left") => NotificationPosition::BottomLeft,
        Some("bottom-center") => NotificationPosition::BottomCenter,
        Some("bottom-right") => NotificationPosition::BottomRight,
        Some(value) => {
            return Err(OverlayParseError::InvalidValue {
                path: "overlays.notifications.position",
                value: value.to_string(),
            });
        }
    };
    let success_duration_ms = optional_u64(
        config,
        &[
            "overlays.notifications.success-duration-ms",
            "overlay.notifications.success-duration-ms",
        ],
    )?
    .unwrap_or(defaults.notifications.success_duration_ms);
    let error_duration_ms = optional_u64(
        config,
        &[
            "overlays.notifications.error-duration-ms",
            "overlay.notifications.error-duration-ms",
        ],
    )?
    .unwrap_or(defaults.notifications.error_duration_ms);
    validate_duration(
        "overlays.notifications.success-duration-ms",
        success_duration_ms,
    )?;
    validate_duration(
        "overlays.notifications.error-duration-ms",
        error_duration_ms,
    )?;

    Ok(Overlays {
        background_color,
        text_color,
        error_color,
        shape,
        borders: optional_bool(config, &["overlays.borders", "overlay.borders"])?
            .unwrap_or(defaults.borders),
        border_source,
        blur: optional_bool(config, &["overlays.blur", "overlay.blur"])?.unwrap_or(defaults.blur),
        notifications: Notifications {
            position,
            success_duration_ms,
            error_duration_ms,
        },
    })
}

fn optional_string(
    config: &RuneConfig,
    paths: &[&str],
) -> Result<Option<String>, rune_cfg::RuneError> {
    for path in paths {
        if let Some(value) = config.get_optional::<String>(path)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn optional_bool(config: &RuneConfig, paths: &[&str]) -> Result<Option<bool>, rune_cfg::RuneError> {
    for path in paths {
        if let Some(value) = config.get_optional::<bool>(path)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn optional_u64(config: &RuneConfig, paths: &[&str]) -> Result<Option<u64>, rune_cfg::RuneError> {
    for path in paths {
        if let Some(value) = config.get_optional::<u64>(path)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn parse_color(
    raw: Option<String>,
    path: &'static str,
    default: OverlayColorMode,
) -> Result<OverlayColorMode, OverlayParseError> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(OverlayColorMode::Auto),
        "light" => Ok(OverlayColorMode::Light),
        "dark" => Ok(OverlayColorMode::Dark),
        value => parse_hex_color(value)
            .ok_or_else(|| OverlayParseError::InvalidValue { path, value: raw }),
    }
}

fn parse_hex_color(value: &str) -> Option<OverlayColorMode> {
    let hex = value.strip_prefix('#')?;
    let (r, g, b, a) = match hex.len() {
        3 => (
            expand_nibble(&hex[0..1])?,
            expand_nibble(&hex[1..2])?,
            expand_nibble(&hex[2..3])?,
            255,
        ),
        4 => (
            expand_nibble(&hex[0..1])?,
            expand_nibble(&hex[1..2])?,
            expand_nibble(&hex[2..3])?,
            expand_nibble(&hex[3..4])?,
        ),
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
            255,
        ),
        8 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
            u8::from_str_radix(&hex[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some(OverlayColorMode::Fixed {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    })
}

fn expand_nibble(value: &str) -> Option<u8> {
    u8::from_str_radix(value, 16).ok().map(|value| value * 17)
}

fn validate_duration(path: &'static str, value: u64) -> Result<(), OverlayParseError> {
    if value == 0 {
        Err(OverlayParseError::InvalidDuration { path, value })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_restore_old_halley_square_overlay_style() {
        let overlays = parse_overlays_checked(&RuneConfig::from_str("").unwrap()).unwrap();
        assert_eq!(overlays.shape, OverlayShape::Square);
        assert!(overlays.borders);
        assert_eq!(overlays.border_source, OverlayBorderSource::Primary);
        assert_eq!(
            overlays.notifications.position,
            NotificationPosition::TopCenter
        );
        assert_eq!(
            overlays.notifications.success_duration_ms,
            DEFAULT_SUCCESS_DURATION_MS
        );
    }

    #[test]
    fn parses_shared_style_and_notification_settings() {
        let config = RuneConfig::from_str(
            r##"
overlays:
  background-colour "#1238"
  text-color "dark"
  error-colour "#fb4934"
  shape "rounded"
  borders false
  border-source "secondary"
  notifications:
    position "bottom-right"
    success-duration-ms 1500
    error-duration-ms 12000
  end
end
"##,
        )
        .unwrap();
        let overlays = parse_overlays_checked(&config).unwrap();

        assert_eq!(overlays.shape, OverlayShape::Rounded);
        assert!(!overlays.borders);
        assert_eq!(overlays.border_source, OverlayBorderSource::Secondary);
        assert_eq!(
            overlays.notifications.position,
            NotificationPosition::BottomRight
        );
        assert_eq!(overlays.notifications.success_duration_ms, 1_500);
        assert_eq!(overlays.notifications.error_duration_ms, 12_000);
        assert!(matches!(
            overlays.background_color,
            OverlayColorMode::Fixed { .. }
        ));
    }

    #[test]
    fn rejects_invalid_style_and_zero_duration() {
        let shape = RuneConfig::from_str("overlays:\n  shape \"squircle\"\nend\n").unwrap();
        assert!(parse_overlays_checked(&shape).is_err());

        let duration = RuneConfig::from_str(
            "overlays:\n  notifications:\n    error-duration-ms 0\n  end\nend\n",
        )
        .unwrap();
        assert!(parse_overlays_checked(&duration).is_err());
    }
}
