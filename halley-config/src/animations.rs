use rune_cfg::RuneConfig;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationCurve {
    #[default]
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
}

impl AnimationCurve {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "linear" => Some(Self::Linear),
            "ease-out-quad" => Some(Self::EaseOutQuad),
            "ease-out-cubic" => Some(Self::EaseOutCubic),
            "ease-out-expo" => Some(Self::EaseOutExpo),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowOpenAnimation {
    pub off: bool,
    pub duration_ms: u32,
    pub curve: AnimationCurve,
}

impl Default for WindowOpenAnimation {
    fn default() -> Self {
        Self {
            off: false,
            duration_ms: 300,
            curve: AnimationCurve::Linear,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Animations {
    pub off: bool,
    pub window_open: WindowOpenAnimation,
}

pub fn parse_animations(config: &RuneConfig) -> Animations {
    let defaults = Animations::default();
    let curve = config
        .get_optional::<String>("animations.window-open.curve")
        .ok()
        .flatten()
        .and_then(|curve| AnimationCurve::parse(&curve))
        .unwrap_or(defaults.window_open.curve);

    Animations {
        off: config.get_or("animations.off", defaults.off),
        window_open: WindowOpenAnimation {
            off: config.get_or("animations.window-open.off", defaults.window_open.off),
            duration_ms: config.get_or(
                "animations.window-open.duration-ms",
                defaults.window_open.duration_ms,
            ),
            curve,
        },
    }
}

pub fn load_animations() -> Animations {
    let Some(path) = crate::config_path() else {
        eprintln!("animations: no config path resolvable, using defaults");
        return Animations::default();
    };

    if let Err(err) = crate::bootstrap_default_config_at(&path) {
        eprintln!("animations: failed to bootstrap default config: {err}");
    }

    match RuneConfig::from_file(&path) {
        Ok(config) => parse_animations(&config),
        Err(err) => {
            eprintln!("animations: failed to load {path:?}, using defaults: {err}");
            Animations::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_window_open_animation() {
        let config = RuneConfig::from_str(
            r#"
animations:
  off false
  window-open:
    off true
    duration-ms 450
    curve "ease-out-cubic"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        assert_eq!(
            parse_animations(&config),
            Animations {
                off: false,
                window_open: WindowOpenAnimation {
                    off: true,
                    duration_ms: 450,
                    curve: AnimationCurve::EaseOutCubic,
                },
            }
        );
    }

    #[test]
    fn missing_section_uses_newm_defaults() {
        let config = RuneConfig::from_str("keybinds:\n  mod \"super\"\nend\n")
            .expect("valid rune-cfg source");

        assert_eq!(parse_animations(&config), Animations::default());
        assert_eq!(Animations::default().window_open.duration_ms, 300);
        assert_eq!(
            Animations::default().window_open.curve,
            AnimationCurve::Linear
        );
    }

    #[test]
    fn invalid_curve_falls_back_to_window_open_default() {
        let config = RuneConfig::from_str(
            r#"
animations:
  window-open:
    curve "wobbly"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        assert_eq!(
            parse_animations(&config).window_open.curve,
            AnimationCurve::Linear
        );
    }
}
