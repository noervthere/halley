use halley_config::{Action, DefaultTerminal, ModifierKey, parse_keybinds};
use rune_cfg::RuneConfig;

const EXAMPLE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../examples/halley.rune");

/// Real end-to-end check: parse the actual shipped example file through
/// real rune-cfg, not just hand-constructed strings like the unit tests
/// elsewhere in this crate use.
#[test]
fn example_config_parses_end_to_end() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let keybinds = parse_keybinds(&config).expect("example keybinds section parses");

    assert_eq!(keybinds.modifier, ModifierKey::Super);
    assert_eq!(keybinds.default_terminal, DefaultTerminal::Auto);
    assert_eq!(keybinds.binds.len(), 11);

    let quit = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::Quit)
        .expect("quit bind present");
    assert_eq!(quit.key, "e");
    assert!(quit.modifiers.super_key);
    assert!(quit.modifiers.shift);

    let close = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::CloseFocusedWindow)
        .expect("close-focused bind present");
    assert_eq!(close.key, "c");
    assert!(close.modifiers.super_key);
    assert!(!close.modifiers.shift);

    let fullscreen = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::ToggleFullscreen)
        .expect("toggle-fullscreen bind present");
    assert_eq!(fullscreen.key, "f");
    assert!(fullscreen.modifiers.super_key);
    assert!(!fullscreen.modifiers.shift);

    let terminal = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::OpenTerminal)
        .expect("open-terminal bind present");
    assert_eq!(terminal.key, "t");
    assert!(terminal.modifiers.super_key);

    for (key, command) in [
        (
            "XF86AudioRaiseVolume",
            "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+ --limit 1.0",
        ),
        (
            "XF86AudioLowerVolume",
            "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-",
        ),
        (
            "XF86AudioMute",
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle",
        ),
    ] {
        let binding = keybinds
            .binds
            .iter()
            .find(|binding| binding.key == key)
            .expect("media key bind present");
        assert_eq!(binding.modifiers, halley_config::Modifiers::default());
        assert_eq!(&binding.action, &Action::Spawn(command.to_string()));
    }

    let zoom_out = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::ZoomOut)
        .expect("zoom-out bind present");
    assert_eq!(zoom_out.key, "minus");
    assert!(zoom_out.modifiers.super_key);

    let zoom_in = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::ZoomIn)
        .expect("zoom-in bind present");
    assert_eq!(zoom_in.key, "equal");
    assert!(zoom_in.modifiers.super_key);

    let zoom_reset = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::ZoomReset)
        .expect("zoom-reset bind present");
    assert_eq!(zoom_reset.key, "0");
    assert!(zoom_reset.modifiers.super_key);

    let screenshot = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::Screenshot)
        .expect("screenshot bind present");
    assert_eq!(screenshot.key, "Print");
    assert_eq!(screenshot.modifiers, halley_config::Modifiers::default());
}

/// Confirms the shipped example's `zoom:` section parses to non-default
/// values matching the file's own documented settings - a lighter version of
/// `example_config_parses_end_to_end` for the zoom section specifically.
#[test]
fn example_config_zoom_section_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let zoom = halley_config::parse_zoom(&config);

    assert!(zoom.enabled);
    assert_eq!(zoom.min, 0.35);
    assert_eq!(zoom.step, 1.10);
    assert_eq!(zoom.smooth_rate, 12.5);
}

#[test]
fn example_config_window_open_animation_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let animations = halley_config::parse_animations(&config);

    assert!(animations.enabled);
    assert!(animations.window_open.enabled);
    assert_eq!(
        animations.window_open.animation_type,
        halley_config::WindowOpenAnimationType::CenterOut
    );
    assert_eq!(animations.window_open.duration_ms, 300);
    assert_eq!(
        animations.window_open.curve,
        halley_config::AnimationCurve::Linear
    );
}

#[test]
fn example_config_fullscreen_animation_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let fullscreen = halley_config::parse_animations(&config).fullscreen;

    assert!(fullscreen.enabled);
    assert_eq!(
        fullscreen.motion,
        halley_config::AnimationMotion::Spring(halley_config::SpringMotion {
            damping_ratio: 1.0,
            stiffness: 800.0,
            epsilon: 0.0001,
        })
    );
}

/// The shipped example's `output:` block is commented out on purpose (an
/// active block whose `name` happened to match a real connector would force
/// a mode onto hardware this file wasn't written for) - confirm it really
/// parses to zero outputs, not just that it happens to not error.
#[test]
fn example_config_output_section_is_commented_out_by_default() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    assert_eq!(halley_config::parse_outputs(&config), Vec::new());
}
