use std::collections::{BTreeMap, HashSet};
use std::fmt;

use rune_cfg::RuneConfig;
use rune_cfg::ast::{ObjectItem, Value};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusRing {
    pub radius_x: f32,
    pub radius_y: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FocusRings {
    pub fallback: FocusRing,
    pub by_output: BTreeMap<String, FocusRing>,
}

impl FocusRings {
    pub fn for_output(&self, output: &str) -> FocusRing {
        self.by_output.get(output).copied().unwrap_or(self.fallback)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusRingParseError(String);

impl fmt::Display for FocusRingParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FocusRingParseError {}

impl Default for FocusRing {
    fn default() -> Self {
        Self {
            radius_x: 820.0,
            radius_y: 420.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decay {
    pub enabled: bool,
    pub outside_delay_seconds: u64,
    pub inside_delay_seconds: u64,
}

impl Default for Decay {
    fn default() -> Self {
        Self {
            enabled: true,
            outside_delay_seconds: 180,
            inside_delay_seconds: 1_800,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeShape {
    Square,
    #[default]
    Squircle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeDisplayPolicy {
    Off,
    Hover,
    #[default]
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RestoreCentering {
    #[default]
    Never,
    IfOffscreen,
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum NodeBackgroundColor {
    #[default]
    Auto,
    Light,
    Dark,
    Fixed(f32, f32, f32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeBorderColor {
    #[default]
    UseWindowActive,
    UseWindowInactive,
    UseWindowSecondaryActive,
    UseWindowSecondaryInactive,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nodes {
    pub shape: NodeShape,
    pub label_shape: NodeShape,
    pub show_labels: NodeDisplayPolicy,
    pub show_app_icons: NodeDisplayPolicy,
    pub icon_size: f32,
    pub opacity: f32,
    pub restore_centering: RestoreCentering,
    pub background_color: NodeBackgroundColor,
    pub border_color_hover: NodeBorderColor,
    pub border_color_inactive: NodeBorderColor,
}

impl Default for Nodes {
    fn default() -> Self {
        Self {
            shape: NodeShape::Squircle,
            label_shape: NodeShape::Squircle,
            show_labels: NodeDisplayPolicy::Hover,
            show_app_icons: NodeDisplayPolicy::Always,
            icon_size: 0.72,
            opacity: 1.0,
            restore_centering: RestoreCentering::Never,
            background_color: NodeBackgroundColor::Auto,
            border_color_hover: NodeBorderColor::UseWindowActive,
            border_color_inactive: NodeBorderColor::UseWindowInactive,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LandmarkPlacement {
    pub gap_px: f32,
}

impl Default for LandmarkPlacement {
    fn default() -> Self {
        Self { gap_px: 20.0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Debug {
    pub show_focus_ring: bool,
}

pub fn parse_focus_rings(config: &RuneConfig) -> FocusRings {
    parse_focus_rings_checked(config).unwrap_or_default()
}

pub fn parse_focus_rings_checked(config: &RuneConfig) -> Result<FocusRings, FocusRingParseError> {
    let Value::Object(root) = config
        .get_value("")
        .map_err(|error| FocusRingParseError(format!("focus-ring config: {error}")))?
    else {
        return Err(FocusRingParseError(
            "focus-ring config root must be an object".to_string(),
        ));
    };
    let blocks = root.iter().filter_map(|item| match item {
        ObjectItem::Assign(key, value) if key == "focus-ring" => Some(value),
        _ => None,
    });
    let mut rings = FocusRings::default();
    let mut outputs = HashSet::new();
    let mut saw_fallback = false;
    for block in blocks {
        let Value::Object(fields) = block else {
            return Err(FocusRingParseError(
                "focus-ring block must be an object".to_string(),
            ));
        };
        let output = object_field(fields, &["output", "name"])
            .map(|value| match value {
                Value::String(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
                _ => Err(FocusRingParseError(
                    "focus-ring output must be a non-empty string".to_string(),
                )),
            })
            .transpose()?;
        let ring = parse_one_focus_ring(fields, output.as_deref())?;
        if let Some(output) = output {
            if !outputs.insert(output.clone()) {
                return Err(FocusRingParseError(format!(
                    "duplicate focus-ring block for {output:?}"
                )));
            }
            rings.by_output.insert(output, ring);
        } else if saw_fallback {
            return Err(FocusRingParseError(
                "only one unkeyed focus-ring fallback is allowed".to_string(),
            ));
        } else {
            saw_fallback = true;
            rings.fallback = ring;
        }
    }
    Ok(rings)
}

pub fn parse_focus_ring(config: &RuneConfig) -> FocusRing {
    parse_focus_rings(config).fallback
}

pub fn parse_landmark_placement(config: &RuneConfig) -> LandmarkPlacement {
    let defaults = LandmarkPlacement::default();
    LandmarkPlacement {
        gap_px: finite_clamp(
            config.get_or("field.gap", config.get_or("field.gap-px", defaults.gap_px)),
            0.0,
            256.0,
            defaults.gap_px,
        ),
    }
}

pub fn parse_decay(config: &RuneConfig) -> Decay {
    let defaults = Decay::default();
    Decay {
        enabled: config.get_or("decay.enabled", defaults.enabled),
        outside_delay_seconds: config
            .get_or(
                "decay.outside-delay-seconds",
                defaults.outside_delay_seconds,
            )
            .max(1),
        inside_delay_seconds: config
            .get_or("decay.inside-delay-seconds", defaults.inside_delay_seconds)
            .max(1),
    }
}

pub fn parse_nodes(config: &RuneConfig) -> Nodes {
    let defaults = Nodes::default();
    Nodes {
        shape: parse_shape(config, "node.shape", defaults.shape),
        label_shape: parse_shape(config, "node.label-shape", defaults.label_shape),
        show_labels: parse_display_policy(config, "node.show-labels", defaults.show_labels),
        show_app_icons: parse_display_policy(
            config,
            "node.show-app-icons",
            defaults.show_app_icons,
        ),
        icon_size: finite_clamp(
            config.get_or("node.icon-size", defaults.icon_size),
            0.1,
            1.0,
            defaults.icon_size,
        ),
        opacity: finite_clamp(
            config.get_or("node.opacity", defaults.opacity),
            0.0,
            1.0,
            defaults.opacity,
        ),
        restore_centering: config
            .get_optional::<String>("node.click-collapsed-pan")
            .ok()
            .flatten()
            .or_else(|| {
                config
                    .get_optional::<String>("node.restore-centering")
                    .ok()
                    .flatten()
            })
            .and_then(|value| match value.as_str() {
                "never" => Some(RestoreCentering::Never),
                "if-offscreen" | "if_offscreen" => Some(RestoreCentering::IfOffscreen),
                "always" => Some(RestoreCentering::Always),
                _ => None,
            })
            .unwrap_or(defaults.restore_centering),
        background_color: parse_background_color(config, defaults.background_color),
        border_color_hover: parse_border_color(
            config,
            "node.border-colour-hover",
            defaults.border_color_hover,
        ),
        border_color_inactive: parse_border_color(
            config,
            "node.border-colour-inactive",
            defaults.border_color_inactive,
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeParseError(String);

impl fmt::Display for NodeParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NodeParseError {}

pub fn parse_nodes_checked(config: &RuneConfig) -> Result<Nodes, NodeParseError> {
    let Value::Object(root) = config
        .get_value("")
        .map_err(|error| NodeParseError(format!("node config: {error}")))?
    else {
        return Err(NodeParseError(
            "node config root must be an object".to_string(),
        ));
    };
    for fields in root.iter().filter_map(|item| match item {
        ObjectItem::Assign(key, Value::Object(fields)) if key == "node" => Some(fields),
        _ => None,
    }) {
        for (removed, replacement) in [
            ("node-shape", "shape"),
            ("node_shape", "shape"),
            ("node-label-shape", "label-shape"),
            ("node_label_shape", "label-shape"),
            ("label_shape", "label-shape"),
        ] {
            if object_field(fields, &[removed]).is_some() {
                return Err(NodeParseError(format!(
                    "node.{removed} was removed; use node.{replacement}"
                )));
            }
        }
    }
    Ok(parse_nodes(config))
}

pub fn parse_debug(config: &RuneConfig) -> Debug {
    Debug {
        show_focus_ring: config.get_or("debug.show-focus-ring", false),
    }
}

fn parse_shape(config: &RuneConfig, path: &str, default: NodeShape) -> NodeShape {
    config
        .get_optional::<String>(path)
        .ok()
        .flatten()
        .and_then(|value| match value.as_str() {
            "square" => Some(NodeShape::Square),
            "squircle" => Some(NodeShape::Squircle),
            _ => None,
        })
        .unwrap_or(default)
}

fn parse_background_color(
    config: &RuneConfig,
    default: NodeBackgroundColor,
) -> NodeBackgroundColor {
    let Some(value) = config
        .get_optional::<String>("node.background-colour")
        .ok()
        .flatten()
    else {
        return default;
    };
    match value.to_ascii_lowercase().as_str() {
        "auto" | "theme" => NodeBackgroundColor::Auto,
        "light" => NodeBackgroundColor::Light,
        "dark" => NodeBackgroundColor::Dark,
        _ => parse_hex_rgb(&value)
            .map(|[r, g, b]| NodeBackgroundColor::Fixed(r, g, b))
            .unwrap_or(default),
    }
}

fn parse_border_color(
    config: &RuneConfig,
    path: &str,
    default: NodeBorderColor,
) -> NodeBorderColor {
    config
        .get_optional::<String>(path)
        .ok()
        .flatten()
        .and_then(|value| match value.as_str() {
            "use-window-active" => Some(NodeBorderColor::UseWindowActive),
            "use-window-inactive" => Some(NodeBorderColor::UseWindowInactive),
            "use-window-secondary-active" => Some(NodeBorderColor::UseWindowSecondaryActive),
            "use-window-secondary-inactive" => Some(NodeBorderColor::UseWindowSecondaryInactive),
            _ => None,
        })
        .unwrap_or(default)
}

fn parse_hex_rgb(value: &str) -> Option<[f32; 3]> {
    let hex = value.strip_prefix('#')?;
    let expanded;
    let hex = if hex.len() == 3 {
        expanded = hex.chars().flat_map(|ch| [ch, ch]).collect::<String>();
        expanded.as_str()
    } else {
        hex
    };
    if hex.len() != 6 {
        return None;
    }
    let raw = u32::from_str_radix(hex, 16).ok()?;
    Some([
        ((raw >> 16) & 0xff) as f32 / 255.0,
        ((raw >> 8) & 0xff) as f32 / 255.0,
        (raw & 0xff) as f32 / 255.0,
    ])
}

fn parse_one_focus_ring(
    fields: &[ObjectItem],
    output: Option<&str>,
) -> Result<FocusRing, FocusRingParseError> {
    let defaults = FocusRing::default();
    let label = output.unwrap_or("fallback");
    let read = |names: &[&str], default: f32, positive: bool| {
        let Some(value) = object_field(fields, names) else {
            return Ok(default);
        };
        let Value::Number(value) = value else {
            return Err(FocusRingParseError(format!(
                "focus-ring {label:?}: {} must be numeric",
                names[0]
            )));
        };
        let value = *value as f32;
        if !value.is_finite() || (positive && value <= 0.0) {
            return Err(FocusRingParseError(format!(
                "focus-ring {label:?}: {} must be {}finite",
                names[0],
                if positive { "positive and " } else { "" }
            )));
        }
        Ok(value)
    };
    Ok(FocusRing {
        radius_x: read(&["radius-x", "radius_x"], defaults.radius_x, true)?,
        radius_y: read(&["radius-y", "radius_y"], defaults.radius_y, true)?,
        offset_x: read(&["offset-x", "offset_x"], defaults.offset_x, false)?,
        offset_y: read(&["offset-y", "offset_y"], defaults.offset_y, false)?,
    })
}

fn object_field<'a>(fields: &'a [ObjectItem], names: &[&str]) -> Option<&'a Value> {
    fields.iter().find_map(|item| match item {
        ObjectItem::Assign(key, value) if names.contains(&key.as_str()) => Some(value),
        _ => None,
    })
}

fn parse_display_policy(
    config: &RuneConfig,
    path: &str,
    default: NodeDisplayPolicy,
) -> NodeDisplayPolicy {
    config
        .get_optional::<String>(path)
        .ok()
        .flatten()
        .and_then(|value| match value.as_str() {
            "off" => Some(NodeDisplayPolicy::Off),
            "hover" => Some(NodeDisplayPolicy::Hover),
            "always" => Some(NodeDisplayPolicy::Always),
            _ => None,
        })
        .unwrap_or(default)
}

fn finite_clamp(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_decay_ring_and_debug_sections() {
        let config = RuneConfig::from_str(
            r#"
focus-ring:
  radius-x 900.0
  radius-y 500.0
  offset-x 20.0
end
decay:
  enabled false
  outside-delay-seconds 9
  inside-delay-seconds 99
end
node:
  shape "square"
  show-labels "always"
  show-app-icons "off"
  restore-centering "if-offscreen"
end
debug:
  show-focus-ring true
end
"#,
        )
        .unwrap();

        assert_eq!(parse_focus_ring(&config).radius_x, 900.0);
        assert_eq!(parse_focus_ring(&config).offset_x, 20.0);
        assert_eq!(parse_decay(&config).outside_delay_seconds, 9);
        assert!(!parse_decay(&config).enabled);
        assert_eq!(parse_nodes(&config).shape, NodeShape::Square);
        assert_eq!(
            parse_nodes(&config).restore_centering,
            RestoreCentering::IfOffscreen
        );
        assert!(parse_debug(&config).show_focus_ring);
    }

    #[test]
    fn parses_repeatable_per_output_focus_rings() {
        let config = RuneConfig::from_str(
            r#"
focus-ring:
  output "DP-1"
  radius-x 900
  radius-y 500
end
focus-ring:
  output "DP-2"
  radius-x 700
  offset-x 40
end
"#,
        )
        .unwrap();
        let rings = parse_focus_rings_checked(&config).unwrap();
        assert_eq!(rings.for_output("DP-1").radius_x, 900.0);
        assert_eq!(rings.for_output("DP-2").radius_x, 700.0);
        assert_eq!(rings.for_output("DP-2").offset_x, 40.0);
        assert_eq!(rings.for_output("HDMI-A-1"), FocusRing::default());
    }

    #[test]
    fn duplicate_focus_ring_output_rejects_atomic_reload() {
        let config = RuneConfig::from_str(
            r#"
focus-ring:
  output "DP-1"
end
focus-ring:
  output "DP-1"
end
"#,
        )
        .unwrap();
        assert!(parse_focus_rings_checked(&config).is_err());
    }

    #[test]
    fn legacy_unkeyed_focus_ring_is_the_fallback() {
        let config = RuneConfig::from_str(
            r#"
focus-ring:
  radius-x 640
  radius-y 360
end
"#,
        )
        .unwrap();
        assert_eq!(
            parse_focus_rings_checked(&config)
                .unwrap()
                .for_output("DP-9")
                .radius_x,
            640.0
        );
    }

    #[test]
    fn parses_canonical_node_names_and_landmark_gap() {
        let config = RuneConfig::from_str(
            r##"
field:
  gap-px 24
end
node:
  shape "square"
  label-shape "square"
  background-colour "#102030"
  click-collapsed-pan "always"
end
"##,
        )
        .unwrap();
        assert_eq!(parse_landmark_placement(&config).gap_px, 24.0);
        let nodes = parse_nodes_checked(&config).unwrap();
        assert_eq!(nodes.shape, NodeShape::Square);
        assert_eq!(nodes.label_shape, NodeShape::Square);
        assert_eq!(nodes.restore_centering, RestoreCentering::Always);
        assert!(matches!(
            nodes.background_color,
            NodeBackgroundColor::Fixed(_, _, _)
        ));
    }

    #[test]
    fn removed_shape_names_fail_with_their_replacements() {
        for (removed, replacement) in [
            ("node-shape", "node.shape"),
            ("node_shape", "node.shape"),
            ("node-label-shape", "node.label-shape"),
            ("node_label_shape", "node.label-shape"),
            ("label_shape", "node.label-shape"),
        ] {
            let config =
                RuneConfig::from_str(&format!("node:\n  {removed} \"square\"\nend\n")).unwrap();
            let error = parse_nodes_checked(&config).unwrap_err().to_string();
            assert!(error.contains(replacement), "{error}");
        }
    }
}
