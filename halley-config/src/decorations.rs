use rune_cfg::RuneConfig;

/// A border's flat RGB color (always fully opaque) - matches old halley's
/// own `DecorationBorderColor` (`layout/types.rs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

/// Primary compositor border styling.
///
/// The radius describes the client-content corner. The renderer grows the
/// outer border radius concentrically by the configured border width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decorations {
    pub border_width_px: i32,
    pub border_radius_px: i32,
    pub border_color_focused: BorderColor,
    pub border_color_unfocused: BorderColor,
}

impl Default for Decorations {
    /// Same defaults as old halley's `PrimaryBorderConfig`.
    fn default() -> Self {
        Self {
            border_width_px: 3,
            border_radius_px: 8,
            border_color_focused: BorderColor {
                r: 0.22,
                g: 0.82,
                b: 0.92,
            },
            border_color_unfocused: BorderColor {
                r: 0.28,
                g: 0.30,
                b: 0.35,
            },
        }
    }
}

/// Parse the `decorations.border` section - tolerant of a missing section
/// or missing individual keys (each just falls back to `Decorations`'s
/// default), matching `parse_keybinds`'s style. Unlike old halley's own
/// parser, this doesn't need to enumerate kebab/snake-case alias spellings
/// by hand - rune-cfg's dotted-path getters already try both case
/// conventions for every path segment. `colour`/`color` is a genuine
/// spelling alias rather than a case convention though, so that part still
/// needs both paths tried explicitly, same as old halley did.
pub fn parse_decorations(config: &RuneConfig) -> Decorations {
    let defaults = Decorations::default();

    let border_width_px = config
        .get_or("decorations.border.size", defaults.border_width_px)
        .clamp(0, 64);
    let border_radius_px = config
        .get_or(
            "decorations.border.radius",
            config.get_or("decorations.border-radius", defaults.border_radius_px),
        )
        .clamp(0, 256);
    let border_color_focused = parse_color(
        config,
        &[
            "decorations.border.colour-focused",
            "decorations.border.color-focused",
        ],
        defaults.border_color_focused,
    );
    let border_color_unfocused = parse_color(
        config,
        &[
            "decorations.border.colour-unfocused",
            "decorations.border.color-unfocused",
        ],
        defaults.border_color_unfocused,
    );

    Decorations {
        border_width_px,
        border_radius_px,
        border_color_focused,
        border_color_unfocused,
    }
}

/// Loads `Decorations` from the user's config file, falling back to
/// `Decorations::default()` on any failure (missing `$HOME`, unwritable
/// config dir, parse error) - mirrors `input::keybinds::load_keybinds`'s
/// load-or-default shape, but self-contained here since decorations need no
/// backend-specific resolution.
pub fn load_decorations() -> Decorations {
    let Some(path) = crate::config_path() else {
        eprintln!("decorations: no config path resolvable, using defaults");
        return Decorations::default();
    };

    if let Err(err) = crate::bootstrap_default_config_at(&path) {
        eprintln!("decorations: failed to bootstrap default config: {err}");
    }

    match RuneConfig::from_file(&path) {
        Ok(config) => parse_decorations(&config),
        Err(err) => {
            eprintln!("decorations: failed to load {path:?}, using defaults: {err}");
            Decorations::default()
        }
    }
}

fn parse_color(config: &RuneConfig, paths: &[&str], default: BorderColor) -> BorderColor {
    let Some(raw) = paths
        .iter()
        .find_map(|path| config.get_optional::<String>(path).ok().flatten())
    else {
        return default;
    };
    parse_hex_rgb(raw.trim())
        .map(|(r, g, b)| BorderColor { r, g, b })
        .unwrap_or(default)
}

/// Parses `"#rrggbb"` or `"#rgb"` into 0.0-1.0 float components - ported
/// directly from old halley's own `parse/primitives.rs::parse_hex_rgb`
/// (halley's own prior code, not a third party's).
fn parse_hex_rgb(value: &str) -> Option<(f32, f32, f32)> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    let expanded = match hex.len() {
        3 => {
            let mut out = String::with_capacity(6);
            for ch in hex.chars() {
                out.push(ch);
                out.push(ch);
            }
            out
        }
        6 => hex.to_string(),
        _ => return None,
    };

    let r = u8::from_str_radix(&expanded[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&expanded[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&expanded[4..6], 16).ok()? as f32 / 255.0;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_border_section() {
        let config = RuneConfig::from_str(
            r##"
decorations:
  border:
    size 4
    radius 9
    colour-focused "#d65d26"
    color-unfocused "#333333"
  end
end
"##,
        )
        .expect("valid rune-cfg source");

        let decorations = parse_decorations(&config);

        assert_eq!(decorations.border_width_px, 4);
        assert_eq!(decorations.border_radius_px, 9);
        assert_eq!(
            decorations.border_color_focused,
            BorderColor {
                r: 0xd6 as f32 / 255.0,
                g: 0x5d as f32 / 255.0,
                b: 0x26 as f32 / 255.0,
            }
        );
        assert_eq!(
            decorations.border_color_unfocused,
            BorderColor {
                r: 0x33 as f32 / 255.0,
                g: 0x33 as f32 / 255.0,
                b: 0x33 as f32 / 255.0,
            }
        );
    }

    #[test]
    fn missing_section_falls_back_to_defaults() {
        let config = RuneConfig::from_str("keybinds:\n  mod \"super\"\nend\n")
            .expect("valid rune-cfg source");

        assert_eq!(parse_decorations(&config), Decorations::default());
    }

    #[test]
    fn defaults_match_old_halley() {
        let defaults = Decorations::default();
        assert_eq!(defaults.border_width_px, 3);
        assert_eq!(defaults.border_radius_px, 8);
        assert_eq!(
            defaults.border_color_focused,
            BorderColor {
                r: 0.22,
                g: 0.82,
                b: 0.92
            }
        );
        assert_eq!(
            defaults.border_color_unfocused,
            BorderColor {
                r: 0.28,
                g: 0.30,
                b: 0.35
            }
        );
    }

    #[test]
    fn clamps_border_metrics_and_accepts_the_old_flat_radius_key() {
        let config = RuneConfig::from_str(
            "decorations:\n  border:\n    size 99\n  end\n  border-radius 999\nend\n",
        )
        .expect("valid rune-cfg source");
        let decorations = parse_decorations(&config);
        assert_eq!(decorations.border_width_px, 64);
        assert_eq!(decorations.border_radius_px, 256);
    }
}
