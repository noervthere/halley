use std::collections::HashSet;
use std::fmt;

use rune_cfg::RuneConfig;
use rune_cfg::ast::{ObjectItem, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct GamescopeConfig {
    pub enabled: bool,
    pub monitor: String,
    pub output_width: String,
    pub output_height: String,
    pub game_width: String,
    pub game_height: String,
    pub refresh: String,
    pub fullscreen: bool,
    pub borderless: bool,
    pub suppress_overlays: bool,
    pub passthrough_pointer_lock: bool,
    pub bypass_spatial_camera: bool,
    pub games: Vec<GamescopeGameProfile>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GamescopeGameProfile {
    pub name: Option<String>,
    pub app_id: Option<String>,
    pub enabled: Option<bool>,
    pub monitor: Option<String>,
    pub output_width: Option<String>,
    pub output_height: Option<String>,
    pub game_width: Option<String>,
    pub game_height: Option<String>,
    pub refresh: Option<String>,
    pub fullscreen: Option<bool>,
    pub borderless: Option<bool>,
    pub suppress_overlays: Option<bool>,
    pub passthrough_pointer_lock: Option<bool>,
    pub bypass_spatial_camera: Option<bool>,
}

impl Default for GamescopeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor: "focused".to_string(),
            output_width: "auto".to_string(),
            output_height: "auto".to_string(),
            game_width: "auto".to_string(),
            game_height: "auto".to_string(),
            refresh: "auto".to_string(),
            fullscreen: true,
            borderless: false,
            suppress_overlays: true,
            passthrough_pointer_lock: true,
            bypass_spatial_camera: true,
            games: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gaming {
    pub games: Vec<String>,
    pub gamescope: GamescopeConfig,
}

impl Default for Gaming {
    fn default() -> Self {
        Self {
            games: vec!["steam_app_*".to_string(), "gamescope".to_string()],
            gamescope: GamescopeConfig::default(),
        }
    }
}

impl Gaming {
    pub fn matches_game(&self, app_id: &str) -> bool {
        self.games.iter().any(|pattern| glob_match(pattern, app_id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamescopeParseError(String);

impl fmt::Display for GamescopeParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GamescopeParseError {}

pub fn parse_gaming(config: &RuneConfig) -> Result<Gaming, GamescopeParseError> {
    let Value::Object(root) = config
        .get_value("")
        .map_err(|error| GamescopeParseError(format!("gaming config: {error}")))?
    else {
        return Err(GamescopeParseError(
            "gaming config root must be an object".to_string(),
        ));
    };
    let gaming_sections = root
        .iter()
        .filter_map(|item| object_named(item, "gaming"))
        .collect::<Vec<_>>();
    if gaming_sections.len() > 1 {
        return Err(GamescopeParseError(
            "gaming may only be specified once".to_string(),
        ));
    }
    if let Some(fields) = gaming_sections.first() {
        return parse_gaming_fields(fields);
    }

    // Preserve old Halley's original top-level `gamescope:` spelling.
    let legacy = root
        .iter()
        .filter_map(|item| object_named(item, "gamescope"))
        .collect::<Vec<_>>();
    if legacy.len() > 1 {
        return Err(GamescopeParseError(
            "gamescope may only be specified once".to_string(),
        ));
    }
    let mut gaming = Gaming::default();
    if let Some(fields) = legacy.first() {
        gaming.gamescope = parse_gamescope_fields(fields)?;
    }
    Ok(gaming)
}

fn parse_gaming_fields(fields: &[ObjectItem]) -> Result<Gaming, GamescopeParseError> {
    let mut gaming = Gaming::default();
    let mut saw_games = false;
    let mut saw_gamescope = false;
    for item in fields {
        let ObjectItem::Assign(key, value) = item else {
            return Err(GamescopeParseError(
                "conditionals are not supported directly inside gaming".to_string(),
            ));
        };
        match normalize_key(key).as_str() {
            "games" if !saw_games => {
                gaming.games = string_list(value, "gaming.games")?;
                saw_games = true;
            }
            "games" => {
                return Err(GamescopeParseError(
                    "gaming.games may only be specified once".to_string(),
                ));
            }
            "gamescope" if !saw_gamescope => {
                let Value::Object(fields) = value else {
                    return Err(GamescopeParseError(
                        "gaming.gamescope must be an object".to_string(),
                    ));
                };
                gaming.gamescope = parse_gamescope_fields(fields)?;
                saw_gamescope = true;
            }
            "gamescope" => {
                return Err(GamescopeParseError(
                    "gaming.gamescope may only be specified once".to_string(),
                ));
            }
            other => {
                return Err(GamescopeParseError(format!("unknown gaming key {other:?}")));
            }
        }
    }
    Ok(gaming)
}

fn parse_gamescope_fields(fields: &[ObjectItem]) -> Result<GamescopeConfig, GamescopeParseError> {
    let mut config = GamescopeConfig::default();
    let mut assigned = HashSet::new();
    for item in fields {
        let ObjectItem::Assign(key, value) = item else {
            return Err(GamescopeParseError(
                "conditionals are not supported directly inside gamescope".to_string(),
            ));
        };
        let key = normalize_key(key);
        if key == "game" {
            let Value::Object(fields) = value else {
                return Err(GamescopeParseError(
                    "gamescope.game must be an object".to_string(),
                ));
            };
            config.games.push(parse_game_profile(fields)?);
            continue;
        }
        if !assigned.insert(key.clone()) {
            return Err(GamescopeParseError(format!(
                "gamescope.{key} may only be specified once"
            )));
        }
        match key.as_str() {
            "enabled" => config.enabled = boolean(value, &key)?,
            "monitor" => config.monitor = string(value, &key)?,
            "output-width" => config.output_width = dimension(value, &key)?,
            "output-height" => config.output_height = dimension(value, &key)?,
            "game-width" => config.game_width = dimension(value, &key)?,
            "game-height" => config.game_height = dimension(value, &key)?,
            "refresh" => config.refresh = dimension(value, &key)?,
            "fullscreen" => config.fullscreen = boolean(value, &key)?,
            "borderless" => config.borderless = boolean(value, &key)?,
            "suppress-overlays" => config.suppress_overlays = boolean(value, &key)?,
            "passthrough-pointer-lock" => config.passthrough_pointer_lock = boolean(value, &key)?,
            "bypass-spatial-camera" => config.bypass_spatial_camera = boolean(value, &key)?,
            other => {
                return Err(GamescopeParseError(format!(
                    "unknown gamescope key {other:?}"
                )));
            }
        }
    }
    Ok(config)
}

fn parse_game_profile(fields: &[ObjectItem]) -> Result<GamescopeGameProfile, GamescopeParseError> {
    let mut profile = GamescopeGameProfile::default();
    let mut assigned = HashSet::new();
    for item in fields {
        let ObjectItem::Assign(key, value) = item else {
            return Err(GamescopeParseError(
                "conditionals are not supported inside gamescope.game".to_string(),
            ));
        };
        let key = normalize_key(key);
        if !assigned.insert(key.clone()) {
            return Err(GamescopeParseError(format!(
                "gamescope.game.{key} may only be specified once"
            )));
        }
        match key.as_str() {
            "name" => profile.name = Some(string(value, &key)?),
            "app-id" => profile.app_id = Some(string(value, &key)?),
            "enabled" => profile.enabled = Some(boolean(value, &key)?),
            "monitor" => profile.monitor = Some(string(value, &key)?),
            "output-width" => profile.output_width = Some(dimension(value, &key)?),
            "output-height" => profile.output_height = Some(dimension(value, &key)?),
            "game-width" => profile.game_width = Some(dimension(value, &key)?),
            "game-height" => profile.game_height = Some(dimension(value, &key)?),
            "refresh" => profile.refresh = Some(dimension(value, &key)?),
            "fullscreen" => profile.fullscreen = Some(boolean(value, &key)?),
            "borderless" => profile.borderless = Some(boolean(value, &key)?),
            "suppress-overlays" => profile.suppress_overlays = Some(boolean(value, &key)?),
            "passthrough-pointer-lock" => {
                profile.passthrough_pointer_lock = Some(boolean(value, &key)?)
            }
            "bypass-spatial-camera" => profile.bypass_spatial_camera = Some(boolean(value, &key)?),
            other => {
                return Err(GamescopeParseError(format!(
                    "unknown gamescope.game key {other:?}"
                )));
            }
        }
    }
    Ok(profile)
}

fn object_named<'a>(item: &'a ObjectItem, name: &str) -> Option<&'a [ObjectItem]> {
    match item {
        ObjectItem::Assign(key, Value::Object(fields)) if key == name => Some(fields),
        _ => None,
    }
}

fn normalize_key(key: &str) -> String {
    key.replace('_', "-")
}

fn boolean(value: &Value, key: &str) -> Result<bool, GamescopeParseError> {
    let Value::Bool(value) = value else {
        return Err(GamescopeParseError(format!(
            "gamescope.{key} must be true or false"
        )));
    };
    Ok(*value)
}

fn string(value: &Value, key: &str) -> Result<String, GamescopeParseError> {
    let Value::String(value) = value else {
        return Err(GamescopeParseError(format!(
            "gamescope.{key} must be a string"
        )));
    };
    Ok(value.clone())
}

fn dimension(value: &Value, key: &str) -> Result<String, GamescopeParseError> {
    match value {
        Value::String(value) if value == "auto" || value.parse::<u32>().is_ok() => {
            Ok(value.clone())
        }
        Value::Number(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value > 0.0
                && *value <= u32::MAX as f64 =>
        {
            Ok((*value as u32).to_string())
        }
        _ => Err(GamescopeParseError(format!(
            "gamescope.{key} must be \"auto\" or a positive whole number"
        ))),
    }
}

fn string_list(value: &Value, key: &str) -> Result<Vec<String>, GamescopeParseError> {
    match value {
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::String(value) => Ok(value.clone()),
                _ => Err(GamescopeParseError(format!("{key} accepts only strings"))),
            })
            .collect(),
        _ => Err(GamescopeParseError(format!(
            "{key} must be a string or array"
        ))),
    }
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let text = text.chars().collect::<Vec<_>>();
    let (mut pattern_index, mut text_index) = (0, 0);
    let (mut star, mut star_text) = (None, 0);
    while text_index < text.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == text[text_index] {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            star_text = text_index;
            pattern_index += 1;
        } else if let Some(star) = star {
            pattern_index = star + 1;
            star_text += 1;
            text_index = star_text;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimSpec {
    Auto,
    Fixed(u32),
}

impl DimSpec {
    fn parse(value: &str) -> Self {
        value
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .map(Self::Fixed)
            .unwrap_or(Self::Auto)
    }

    fn resolve(self, automatic: Option<u32>) -> Option<u32> {
        match self {
            Self::Auto => automatic,
            Self::Fixed(value) => Some(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedGamescopeProfile {
    pub monitor: String,
    pub output_width: DimSpec,
    pub output_height: DimSpec,
    pub game_width: DimSpec,
    pub game_height: DimSpec,
    pub refresh: DimSpec,
    pub fullscreen: bool,
    pub borderless: bool,
    pub suppress_overlays: bool,
    pub passthrough_pointer_lock: bool,
    pub bypass_spatial_camera: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GamescopeDecision {
    Disabled,
    Skip,
    Wrap(ResolvedGamescopeProfile),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TargetDims {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub refresh_hz: Option<f64>,
}

pub fn resolve_profile(config: &GamescopeConfig, app_id: Option<&str>) -> GamescopeDecision {
    if !config.enabled {
        return GamescopeDecision::Disabled;
    }
    let matched = app_id.and_then(|app_id| {
        config
            .games
            .iter()
            .find(|profile| profile.app_id.as_deref() == Some(app_id))
    });
    if matched.is_some_and(|profile| profile.enabled == Some(false)) {
        return GamescopeDecision::Skip;
    }
    GamescopeDecision::Wrap(merge_profile(config, matched))
}

fn merge_profile(
    config: &GamescopeConfig,
    profile: Option<&GamescopeGameProfile>,
) -> ResolvedGamescopeProfile {
    let string = |override_value: Option<&String>, default: &str| {
        override_value
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    ResolvedGamescopeProfile {
        monitor: string(
            profile.and_then(|profile| profile.monitor.as_ref()),
            &config.monitor,
        ),
        output_width: DimSpec::parse(&string(
            profile.and_then(|profile| profile.output_width.as_ref()),
            &config.output_width,
        )),
        output_height: DimSpec::parse(&string(
            profile.and_then(|profile| profile.output_height.as_ref()),
            &config.output_height,
        )),
        game_width: DimSpec::parse(&string(
            profile.and_then(|profile| profile.game_width.as_ref()),
            &config.game_width,
        )),
        game_height: DimSpec::parse(&string(
            profile.and_then(|profile| profile.game_height.as_ref()),
            &config.game_height,
        )),
        refresh: DimSpec::parse(&string(
            profile.and_then(|profile| profile.refresh.as_ref()),
            &config.refresh,
        )),
        fullscreen: profile
            .and_then(|profile| profile.fullscreen)
            .unwrap_or(config.fullscreen),
        borderless: profile
            .and_then(|profile| profile.borderless)
            .unwrap_or(config.borderless),
        suppress_overlays: profile
            .and_then(|profile| profile.suppress_overlays)
            .unwrap_or(config.suppress_overlays),
        passthrough_pointer_lock: profile
            .and_then(|profile| profile.passthrough_pointer_lock)
            .unwrap_or(config.passthrough_pointer_lock),
        bypass_spatial_camera: profile
            .and_then(|profile| profile.bypass_spatial_camera)
            .unwrap_or(config.bypass_spatial_camera),
    }
}

pub fn build_gamescope_argv(
    profile: &ResolvedGamescopeProfile,
    target: &TargetDims,
    command: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut arguments = vec!["gamescope".to_string()];
    let mut diagnostics = Vec::new();
    let mut push_dimension = |flag: &str, value: Option<u32>| {
        if let Some(value) = value.filter(|value| *value > 0) {
            arguments.push(flag.to_string());
            arguments.push(value.to_string());
        }
    };
    push_dimension("-W", profile.output_width.resolve(target.width));
    push_dimension("-H", profile.output_height.resolve(target.height));
    push_dimension("-w", profile.game_width.resolve(target.width));
    push_dimension("-h", profile.game_height.resolve(target.height));
    let refresh = match profile.refresh {
        DimSpec::Auto => target.refresh_hz.map(|refresh| refresh.round() as u32),
        DimSpec::Fixed(refresh) => Some(refresh),
    };
    push_dimension("-r", refresh);
    if profile.fullscreen {
        if profile.borderless {
            diagnostics.push(
                "gamescope: fullscreen and borderless are both enabled; using fullscreen (-f)"
                    .to_string(),
            );
        }
        arguments.push("-f".to_string());
    } else if profile.borderless {
        arguments.push("-b".to_string());
    }
    arguments.push("--".to_string());
    arguments.extend(command.iter().cloned());
    (arguments, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_gaming_and_profiles() {
        let config = RuneConfig::from_str(
            r#"
gaming:
  games ["steam_app_*", "tf_linux64", "gamescope"]
  gamescope:
    monitor "cursor"
    output-width 2560
    fullscreen false
    borderless true
    game:
      app-id "steam_app_42"
      game-width 1920
      enabled false
    end
  end
end
"#,
        )
        .unwrap();
        let gaming = parse_gaming(&config).unwrap();
        assert!(gaming.matches_game("steam_app_42"));
        assert!(gaming.matches_game("tf_linux64"));
        assert_eq!(gaming.gamescope.monitor, "cursor");
        assert_eq!(gaming.gamescope.output_width, "2560");
        assert!(gaming.gamescope.borderless);
        assert_eq!(
            gaming.gamescope.games[0].game_width.as_deref(),
            Some("1920")
        );
        assert_eq!(
            resolve_profile(&gaming.gamescope, Some("steam_app_42")),
            GamescopeDecision::Skip
        );
    }

    #[test]
    fn top_level_gamescope_remains_compatible() {
        let config =
            RuneConfig::from_str("gamescope:\n  enabled false\n  monitor \"DP-1\"\nend\n").unwrap();
        let gaming = parse_gaming(&config).unwrap();
        assert!(!gaming.gamescope.enabled);
        assert_eq!(gaming.gamescope.monitor, "DP-1");
    }

    #[test]
    fn automatic_dimensions_use_the_live_monitor() {
        let GamescopeDecision::Wrap(profile) = resolve_profile(&GamescopeConfig::default(), None)
        else {
            panic!("default profile should wrap")
        };
        let target = TargetDims {
            width: Some(2560),
            height: Some(1440),
            refresh_hz: Some(179.998),
        };
        let command = vec!["game".to_string(), "--flag".to_string()];
        let (arguments, diagnostics) = build_gamescope_argv(&profile, &target, &command);
        assert_eq!(
            arguments,
            [
                "gamescope",
                "-W",
                "2560",
                "-H",
                "1440",
                "-w",
                "2560",
                "-h",
                "1440",
                "-r",
                "180",
                "-f",
                "--",
                "game",
                "--flag",
            ]
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn invalid_profile_keys_are_rejected() {
        let config = RuneConfig::from_str(
            "gaming:\n  gamescope:\n    game:\n      magic true\n    end\n  end\nend\n",
        )
        .unwrap();
        assert!(parse_gaming(&config).is_err());
    }
}
