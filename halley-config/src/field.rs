use std::fmt;

use rune_cfg::RuneConfig;

use crate::Zoom;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloseRestorePan {
    Never,
    #[default]
    IfOffscreen,
    Always,
}

impl CloseRestorePan {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "never" => Some(Self::Never),
            "if-offscreen" => Some(Self::IfOffscreen),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub gap: f32,
    pub close_restore_focus: bool,
    pub close_restore_pan: CloseRestorePan,
    pub zoom: Zoom,
}

impl Default for Field {
    fn default() -> Self {
        Self {
            gap: 20.0,
            close_restore_focus: true,
            close_restore_pan: CloseRestorePan::IfOffscreen,
            zoom: Zoom::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldParseError(String);

impl fmt::Display for FieldParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FieldParseError {}

fn finite_clamp(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn canonical_or_legacy_f32(
    config: &RuneConfig,
    canonical: &str,
    legacy: &str,
    fallback: f32,
) -> f32 {
    config
        .get_optional(canonical)
        .ok()
        .flatten()
        .or_else(|| config.get_optional(legacy).ok().flatten())
        .unwrap_or(fallback)
}

fn canonical_or_legacy_bool(
    config: &RuneConfig,
    canonical: &str,
    legacy: &str,
    fallback: bool,
) -> bool {
    config
        .get_optional(canonical)
        .ok()
        .flatten()
        .or_else(|| config.get_optional(legacy).ok().flatten())
        .unwrap_or(fallback)
}

pub fn parse_field_checked(config: &RuneConfig) -> Result<Field, FieldParseError> {
    for unsupported in ["max", "smooth", "filter", "sharpen"] {
        let path = format!("field.zoom.{unsupported}");
        if config.get_value(&path).is_ok() {
            return Err(FieldParseError(format!(
                "{path} is not supported; Halley field zoom is always smooth and capped at native scale 1.0"
            )));
        }
    }

    let defaults = Field::default();
    let pan = config
        .get_optional::<String>("field.close-restore-pan")
        .map_err(|error| FieldParseError(format!("field.close-restore-pan: {error}")))?
        .map(|value| {
            CloseRestorePan::parse(&value).ok_or_else(|| {
                FieldParseError(format!(
                    "field.close-restore-pan must be \"never\", \"if-offscreen\", or \"always\", got {value:?}"
                ))
            })
        })
        .transpose()?
        .unwrap_or(defaults.close_restore_pan);

    Ok(Field {
        gap: finite_clamp(
            canonical_or_legacy_f32(config, "field.gap", "field.gap-px", defaults.gap),
            0.0,
            256.0,
            defaults.gap,
        ),
        close_restore_focus: config
            .get_or("field.close-restore-focus", defaults.close_restore_focus),
        close_restore_pan: pan,
        zoom: Zoom {
            enabled: canonical_or_legacy_bool(
                config,
                "field.zoom.enabled",
                "zoom.enabled",
                defaults.zoom.enabled,
            ),
            min: finite_clamp(
                canonical_or_legacy_f32(config, "field.zoom.min", "zoom.min", defaults.zoom.min),
                0.05,
                1.0,
                defaults.zoom.min,
            ),
            step: finite_clamp(
                canonical_or_legacy_f32(config, "field.zoom.step", "zoom.step", defaults.zoom.step),
                1.001,
                8.0,
                defaults.zoom.step,
            ),
            smooth_rate: finite_clamp(
                canonical_or_legacy_f32(
                    config,
                    "field.zoom.smooth-rate",
                    "zoom.smooth-rate",
                    defaults.zoom.smooth_rate,
                ),
                0.1,
                120.0,
                defaults.zoom.smooth_rate,
            ),
        },
    })
}

pub fn parse_field(config: &RuneConfig) -> Field {
    parse_field_checked(config).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_field_values_win_per_setting() {
        let config = RuneConfig::from_str(
            r#"
zoom:
  enabled false
  min 0.2
  step 1.5
end
field:
  gap-px 80.0
  gap 24.0
  close-restore-focus false
  close-restore-pan "always"
  zoom:
    enabled true
    step 1.2
    smooth-rate 7.0
  end
end
"#,
        )
        .unwrap();

        assert_eq!(
            parse_field_checked(&config).unwrap(),
            Field {
                gap: 24.0,
                close_restore_focus: false,
                close_restore_pan: CloseRestorePan::Always,
                zoom: Zoom {
                    enabled: true,
                    min: 0.2,
                    step: 1.2,
                    smooth_rate: 7.0,
                },
            }
        );
    }

    #[test]
    fn legacy_zoom_and_gap_remain_compatible() {
        let config = RuneConfig::from_str(
            r#"
zoom:
  min 0.2
end
field:
  gap-px 18.0
end
"#,
        )
        .unwrap();
        let field = parse_field_checked(&config).unwrap();
        assert_eq!(field.gap, 18.0);
        assert_eq!(field.zoom.min, 0.2);
    }

    #[test]
    fn unsupported_zoom_ceiling_has_a_useful_error() {
        let config = RuneConfig::from_str("field:\n  zoom:\n    max 2.0\n  end\nend\n").unwrap();
        let error = parse_field_checked(&config).unwrap_err().to_string();
        assert!(error.contains("field.zoom.max"));
        assert!(error.contains("capped at native scale 1.0"));
    }
}
