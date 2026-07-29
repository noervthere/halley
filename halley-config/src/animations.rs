use rune_cfg::RuneConfig;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationCurve {
    #[default]
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
    Elastic,
}

impl AnimationCurve {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "linear" => Some(Self::Linear),
            "ease-out-quad" => Some(Self::EaseOutQuad),
            "ease-out-cubic" => Some(Self::EaseOutCubic),
            "ease-out-expo" => Some(Self::EaseOutExpo),
            "elastic" => Some(Self::Elastic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EasingMotion {
    pub duration_ms: u32,
    pub curve: AnimationCurve,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringMotion {
    pub damping_ratio: f64,
    pub stiffness: f64,
}

impl Default for SpringMotion {
    fn default() -> Self {
        Self {
            damping_ratio: 1.0,
            stiffness: 800.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimationMotion {
    Easing(EasingMotion),
    Spring(SpringMotion),
}

impl AnimationMotion {
    fn parse(config: &RuneConfig, path: &str, default: Self) -> Self {
        let kind = config
            .get_optional::<String>(&format!("{path}.motion"))
            .ok()
            .flatten();

        match kind.as_deref() {
            Some("spring") => {
                let defaults = match default {
                    Self::Spring(defaults) => defaults,
                    Self::Easing(_) => SpringMotion::default(),
                };
                Self::Spring(SpringMotion {
                    damping_ratio: finite_clamp(
                        config.get_or(&format!("{path}.damping-ratio"), defaults.damping_ratio),
                        0.1,
                        10.0,
                        defaults.damping_ratio,
                    ),
                    stiffness: finite_clamp(
                        config.get_or(&format!("{path}.stiffness"), defaults.stiffness),
                        1.0,
                        100_000.0,
                        defaults.stiffness,
                    ),
                })
            }
            Some("easing") => {
                let defaults = match default {
                    Self::Easing(defaults) => defaults,
                    Self::Spring(_) => EasingMotion {
                        duration_ms: 250,
                        curve: AnimationCurve::EaseOutCubic,
                    },
                };
                Self::Easing(EasingMotion {
                    duration_ms: config
                        .get_or(&format!("{path}.duration-ms"), defaults.duration_ms),
                    curve: config
                        .get_optional::<String>(&format!("{path}.curve"))
                        .ok()
                        .flatten()
                        .and_then(|curve| AnimationCurve::parse(&curve))
                        .unwrap_or(defaults.curve),
                })
            }
            _ => default,
        }
    }
}

fn finite_clamp(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowOpenAnimationType {
    #[default]
    CenterOut,
    Fade,
    Launch,
}

impl WindowOpenAnimationType {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "center-out" => Some(Self::CenterOut),
            "fade" => Some(Self::Fade),
            "launch" => Some(Self::Launch),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowOpenAnimation {
    pub enabled: bool,
    pub animation_type: WindowOpenAnimationType,
    pub motion: AnimationMotion,
}

impl Default for WindowOpenAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            animation_type: WindowOpenAnimationType::default(),
            motion: AnimationMotion::Easing(EasingMotion {
                duration_ms: 300,
                curve: AnimationCurve::Linear,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowCloseAnimationType {
    #[default]
    Shrink,
    Fade,
}

impl WindowCloseAnimationType {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "shrink" => Some(Self::Shrink),
            "fade" => Some(Self::Fade),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowCloseAnimation {
    pub enabled: bool,
    pub animation_type: WindowCloseAnimationType,
    pub duration_ms: u32,
}

impl Default for WindowCloseAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            animation_type: WindowCloseAnimationType::default(),
            duration_ms: 270,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FullscreenAnimation {
    pub enabled: bool,
    pub motion: AnimationMotion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeAnimation {
    pub enabled: bool,
    pub duration_ms: u32,
}

impl Default for NodeAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 280,
        }
    }
}

impl Default for FullscreenAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            motion: AnimationMotion::Spring(SpringMotion::default()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Animations {
    pub enabled: bool,
    pub window_open: WindowOpenAnimation,
    pub window_close: WindowCloseAnimation,
    pub fullscreen: FullscreenAnimation,
    pub node: NodeAnimation,
}

impl Default for Animations {
    fn default() -> Self {
        Self {
            enabled: true,
            window_open: WindowOpenAnimation::default(),
            window_close: WindowCloseAnimation::default(),
            fullscreen: FullscreenAnimation::default(),
            node: NodeAnimation::default(),
        }
    }
}

pub fn parse_animations(config: &RuneConfig) -> Animations {
    let defaults = Animations::default();
    let animation_type = config
        .get_optional::<String>("animations.window-open.type")
        .ok()
        .flatten()
        .and_then(|value| WindowOpenAnimationType::parse(&value))
        .unwrap_or(defaults.window_open.animation_type);
    let default_easing = match defaults.window_open.motion {
        AnimationMotion::Easing(easing) => easing,
        AnimationMotion::Spring(_) => unreachable!("window-open defaults use easing motion"),
    };
    let curve = config
        .get_optional::<String>("animations.window-open.curve")
        .ok()
        .flatten()
        .and_then(|curve| AnimationCurve::parse(&curve))
        .unwrap_or(default_easing.curve);
    let configured_easing = AnimationMotion::Easing(EasingMotion {
        duration_ms: config.get_or(
            "animations.window-open.duration-ms",
            default_easing.duration_ms,
        ),
        curve,
    });
    let window_open_motion =
        AnimationMotion::parse(config, "animations.window-open", configured_easing);

    Animations {
        enabled: config.get_or("animations.enabled", defaults.enabled),
        window_open: WindowOpenAnimation {
            enabled: config.get_or(
                "animations.window-open.enabled",
                defaults.window_open.enabled,
            ),
            animation_type,
            motion: window_open_motion,
        },
        window_close: WindowCloseAnimation {
            enabled: config.get_or(
                "animations.window-close.enabled",
                defaults.window_close.enabled,
            ),
            animation_type: config
                .get_optional::<String>("animations.window-close.type")
                .ok()
                .flatten()
                .and_then(|value| WindowCloseAnimationType::parse(&value))
                .unwrap_or(defaults.window_close.animation_type),
            duration_ms: config.get_or(
                "animations.window-close.duration-ms",
                defaults.window_close.duration_ms,
            ),
        },
        fullscreen: FullscreenAnimation {
            enabled: config.get_or("animations.fullscreen.enabled", defaults.fullscreen.enabled),
            motion: AnimationMotion::parse(
                config,
                "animations.fullscreen",
                defaults.fullscreen.motion,
            ),
        },
        node: NodeAnimation {
            enabled: config.get_or("animations.node.enabled", defaults.node.enabled),
            duration_ms: config.get_or("animations.node.duration-ms", defaults.node.duration_ms),
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
  enabled true
  window-open:
    enabled false
    type "fade"
    duration-ms 450
    curve "elastic"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        assert_eq!(
            parse_animations(&config),
            Animations {
                enabled: true,
                window_open: WindowOpenAnimation {
                    enabled: false,
                    animation_type: WindowOpenAnimationType::Fade,
                    motion: AnimationMotion::Easing(EasingMotion {
                        duration_ms: 450,
                        curve: AnimationCurve::Elastic,
                    }),
                },
                window_close: WindowCloseAnimation::default(),
                fullscreen: FullscreenAnimation::default(),
                node: NodeAnimation::default(),
            }
        );
    }

    #[test]
    fn parses_window_close_animation() {
        let config = RuneConfig::from_str(
            r#"
animations:
  window-close:
    enabled false
    type "fade"
    duration-ms 410
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        assert_eq!(
            parse_animations(&config).window_close,
            WindowCloseAnimation {
                enabled: false,
                animation_type: WindowCloseAnimationType::Fade,
                duration_ms: 410,
            }
        );
    }

    #[test]
    fn missing_section_uses_center_out_defaults() {
        let config = RuneConfig::from_str("keybinds:\n  mod \"super\"\nend\n")
            .expect("valid rune-cfg source");

        assert_eq!(parse_animations(&config), Animations::default());
        assert!(Animations::default().enabled);
        assert!(Animations::default().window_open.enabled);
        assert_eq!(
            Animations::default().window_close,
            WindowCloseAnimation {
                enabled: true,
                animation_type: WindowCloseAnimationType::Shrink,
                duration_ms: 270,
            }
        );
        assert_eq!(
            Animations::default().window_open.animation_type,
            WindowOpenAnimationType::CenterOut
        );
        assert_eq!(
            Animations::default().window_open.motion,
            AnimationMotion::Easing(EasingMotion {
                duration_ms: 300,
                curve: AnimationCurve::Linear,
            })
        );
    }

    #[test]
    fn window_open_style_does_not_select_motion_defaults() {
        let parse = |animation_type| {
            let config = RuneConfig::from_str(&format!(
                r#"
animations:
  window-open:
    type "{animation_type}"
  end
end
"#
            ))
            .expect("valid rune-cfg source");
            parse_animations(&config).window_open
        };

        let center_out = parse("center-out");
        let fade = parse("fade");
        let launch = parse("launch");

        assert_eq!(
            center_out.animation_type,
            WindowOpenAnimationType::CenterOut
        );
        assert_eq!(fade.animation_type, WindowOpenAnimationType::Fade);
        assert_eq!(launch.animation_type, WindowOpenAnimationType::Launch);
        assert_eq!(center_out.motion, fade.motion);
        assert_eq!(center_out.motion, launch.motion);
    }

    #[test]
    fn removed_animation_names_have_no_compatibility_aliases() {
        assert_eq!(WindowOpenAnimationType::parse("elastic"), None);
        assert_eq!(AnimationCurve::parse("ease-out-back"), None);
        assert_eq!(
            WindowOpenAnimationType::parse("fade"),
            Some(WindowOpenAnimationType::Fade)
        );
        assert_eq!(
            WindowOpenAnimationType::parse("launch"),
            Some(WindowOpenAnimationType::Launch)
        );
        assert_eq!(
            AnimationCurve::parse("elastic"),
            Some(AnimationCurve::Elastic)
        );
    }

    #[test]
    fn invalid_values_fall_back_to_center_out_defaults() {
        let config = RuneConfig::from_str(
            r#"
animations:
  window-open:
    type "stretchy"
    curve "wobbly"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        let animation = parse_animations(&config).window_open;
        assert_eq!(animation.animation_type, WindowOpenAnimationType::CenterOut);
        assert_eq!(
            animation.motion,
            AnimationMotion::Easing(EasingMotion {
                duration_ms: 300,
                curve: AnimationCurve::Linear,
            })
        );

        let config = RuneConfig::from_str(
            r#"
animations:
  window-close:
    type "vanish"
  end
end
"#,
        )
        .expect("valid rune-cfg source");
        assert_eq!(
            parse_animations(&config).window_close.animation_type,
            WindowCloseAnimationType::Shrink
        );
    }

    #[test]
    fn fullscreen_defaults_to_critical_spring() {
        let config = RuneConfig::from_str(
            r#"
animations:
  fullscreen:
    enabled false
    motion "spring"
    damping-ratio 0.8
    stiffness 600.0
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        let fullscreen = parse_animations(&config).fullscreen;
        assert!(!fullscreen.enabled);
        assert_eq!(
            fullscreen.motion,
            AnimationMotion::Spring(SpringMotion {
                damping_ratio: 0.8,
                stiffness: 600.0,
            })
        );
    }

    #[test]
    fn fullscreen_can_use_easing_motion() {
        let config = RuneConfig::from_str(
            r#"
animations:
  fullscreen:
    motion "easing"
    duration-ms 180
    curve "ease-out-expo"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        assert_eq!(
            parse_animations(&config).fullscreen.motion,
            AnimationMotion::Easing(EasingMotion {
                duration_ms: 180,
                curve: AnimationCurve::EaseOutExpo,
            })
        );
    }

    #[test]
    fn spring_values_are_constrained_to_stable_ranges() {
        let config = RuneConfig::from_str(
            r#"
animations:
  fullscreen:
    motion "spring"
    damping-ratio 0.0
    stiffness 999999.0
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        assert_eq!(
            parse_animations(&config).fullscreen.motion,
            AnimationMotion::Spring(SpringMotion {
                damping_ratio: 0.1,
                stiffness: 100_000.0,
            })
        );
    }
}
