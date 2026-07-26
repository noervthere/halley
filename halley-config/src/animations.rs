use rune_cfg::RuneConfig;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationCurve {
    #[default]
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
    EaseOutBack,
}

impl AnimationCurve {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "linear" => Some(Self::Linear),
            "ease-out-quad" => Some(Self::EaseOutQuad),
            "ease-out-cubic" => Some(Self::EaseOutCubic),
            "ease-out-expo" => Some(Self::EaseOutExpo),
            "ease-out-back" => Some(Self::EaseOutBack),
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
    pub epsilon: f64,
}

impl Default for SpringMotion {
    fn default() -> Self {
        Self {
            damping_ratio: 1.0,
            stiffness: 800.0,
            epsilon: 0.0001,
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
                    epsilon: finite_clamp(
                        config.get_or(&format!("{path}.epsilon"), defaults.epsilon),
                        0.00001,
                        0.1,
                        defaults.epsilon,
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
    Elastic,
}

impl WindowOpenAnimationType {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "center-out" => Some(Self::CenterOut),
            "elastic" => Some(Self::Elastic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowOpenAnimation {
    pub enabled: bool,
    pub animation_type: WindowOpenAnimationType,
    pub duration_ms: u32,
    pub curve: AnimationCurve,
    pub motion: AnimationMotion,
}

impl WindowOpenAnimation {
    fn defaults_for(animation_type: WindowOpenAnimationType) -> Self {
        match animation_type {
            WindowOpenAnimationType::CenterOut => Self {
                enabled: true,
                animation_type,
                duration_ms: 300,
                curve: AnimationCurve::Linear,
                motion: AnimationMotion::Easing(EasingMotion {
                    duration_ms: 300,
                    curve: AnimationCurve::Linear,
                }),
            },
            WindowOpenAnimationType::Elastic => Self {
                enabled: true,
                animation_type,
                duration_ms: 620,
                curve: AnimationCurve::EaseOutBack,
                motion: AnimationMotion::Easing(EasingMotion {
                    duration_ms: 620,
                    curve: AnimationCurve::EaseOutBack,
                }),
            },
        }
    }
}

impl Default for WindowOpenAnimation {
    fn default() -> Self {
        Self::defaults_for(WindowOpenAnimationType::default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FullscreenAnimation {
    pub enabled: bool,
    pub motion: AnimationMotion,
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
    pub fullscreen: FullscreenAnimation,
}

impl Default for Animations {
    fn default() -> Self {
        Self {
            enabled: true,
            window_open: WindowOpenAnimation::default(),
            fullscreen: FullscreenAnimation::default(),
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
    let type_defaults = WindowOpenAnimation::defaults_for(animation_type);
    let curve = config
        .get_optional::<String>("animations.window-open.curve")
        .ok()
        .flatten()
        .and_then(|curve| AnimationCurve::parse(&curve))
        .unwrap_or(type_defaults.curve);
    let legacy_motion = AnimationMotion::Easing(EasingMotion {
        duration_ms: config.get_or(
            "animations.window-open.duration-ms",
            type_defaults.duration_ms,
        ),
        curve,
    });
    let window_open_motion =
        AnimationMotion::parse(config, "animations.window-open", legacy_motion);

    Animations {
        enabled: config.get_or("animations.enabled", defaults.enabled),
        window_open: WindowOpenAnimation {
            enabled: config.get_or(
                "animations.window-open.enabled",
                defaults.window_open.enabled,
            ),
            animation_type,
            duration_ms: match window_open_motion {
                AnimationMotion::Easing(motion) => motion.duration_ms,
                AnimationMotion::Spring(_) => type_defaults.duration_ms,
            },
            curve,
            motion: window_open_motion,
        },
        fullscreen: FullscreenAnimation {
            enabled: config.get_or("animations.fullscreen.enabled", defaults.fullscreen.enabled),
            motion: AnimationMotion::parse(
                config,
                "animations.fullscreen",
                defaults.fullscreen.motion,
            ),
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
    type "elastic"
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
                enabled: true,
                window_open: WindowOpenAnimation {
                    enabled: false,
                    animation_type: WindowOpenAnimationType::Elastic,
                    duration_ms: 450,
                    curve: AnimationCurve::EaseOutCubic,
                    motion: AnimationMotion::Easing(EasingMotion {
                        duration_ms: 450,
                        curve: AnimationCurve::EaseOutCubic,
                    }),
                },
                fullscreen: FullscreenAnimation::default(),
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
            Animations::default().window_open.animation_type,
            WindowOpenAnimationType::CenterOut
        );
        assert_eq!(Animations::default().window_open.duration_ms, 300);
        assert_eq!(
            Animations::default().window_open.curve,
            AnimationCurve::Linear
        );
    }

    #[test]
    fn elastic_type_supplies_legacy_motion_defaults() {
        let config = RuneConfig::from_str(
            r#"
animations:
  window-open:
    type "elastic"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        let animation = parse_animations(&config).window_open;
        assert_eq!(animation.animation_type, WindowOpenAnimationType::Elastic);
        assert_eq!(animation.duration_ms, 620);
        assert_eq!(animation.curve, AnimationCurve::EaseOutBack);
    }

    #[test]
    fn explicit_values_override_elastic_defaults() {
        let config = RuneConfig::from_str(
            r#"
animations:
  window-open:
    type "elastic"
    duration-ms 480
    curve "ease-out-quad"
  end
end
"#,
        )
        .expect("valid rune-cfg source");

        let animation = parse_animations(&config).window_open;
        assert_eq!(animation.duration_ms, 480);
        assert_eq!(animation.curve, AnimationCurve::EaseOutQuad);
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
        assert_eq!(animation.duration_ms, 300);
        assert_eq!(animation.curve, AnimationCurve::Linear);
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
    epsilon 0.001
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
                epsilon: 0.001,
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
    epsilon 1.0
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
                epsilon: 0.1,
            })
        );
    }
}
