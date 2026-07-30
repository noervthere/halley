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
    match &keybinds.default_terminal {
        DefaultTerminal::Auto => {}
        DefaultTerminal::Explicit(command) => {
            assert!(
                !command.trim().is_empty(),
                "explicit terminal cannot be empty"
            )
        }
    }
    assert_eq!(keybinds.binds.len(), 17);

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
    assert_eq!(close.key, "q");
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

    let toggle_state = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::ToggleState)
        .expect("toggle-state bind present");
    assert_eq!(toggle_state.key, "n");
    assert!(toggle_state.modifiers.super_key);

    let bearings_show = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::BearingsShow)
        .expect("bearings-show bind present");
    assert_eq!(bearings_show.key, "z");
    assert!(bearings_show.modifiers.super_key);
    assert!(!bearings_show.modifiers.shift);

    let bearings_toggle = keybinds
        .binds
        .iter()
        .find(|b| b.action == Action::BearingsToggle)
        .expect("bearings-toggle bind present");
    assert_eq!(bearings_toggle.key, "z");
    assert!(bearings_toggle.modifiers.super_key);
    assert!(bearings_toggle.modifiers.shift);

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

#[test]
fn example_config_keeps_environment_and_autostart_inactive() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let runtime = halley_config::parse_runtime_config(&config).expect("runtime config parses");

    assert!(runtime.env.is_empty());
    assert!(runtime.autostart.once.is_empty());
    assert!(runtime.autostart.on_reload.is_empty());
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
fn example_config_cursor_section_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let cursor = halley_config::parse_cursor(&config);

    assert_eq!(cursor, halley_config::Cursor::default());
}

#[test]
fn example_config_apogee_section_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let apogee = halley_config::parse_apogee(&config);

    assert!(apogee.enabled);
    assert!(apogee.live_previews);
    assert_eq!(apogee.preview_max_fps, 30);
    assert_eq!(apogee.max_rows, 3);
}

#[test]
fn example_config_overlay_section_is_the_bootstrap_style() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let overlays = halley_config::parse_overlays_checked(&config).expect("overlays parse");

    assert_eq!(overlays.shape, halley_config::OverlayShape::Square);
    assert!(overlays.borders);
    assert_eq!(
        overlays.notifications.position,
        halley_config::NotificationPosition::TopCenter
    );
    assert_eq!(overlays.notifications.success_duration_ms, 4_000);
    assert_eq!(overlays.notifications.error_duration_ms, 9_000);
}

#[test]
fn example_config_has_per_output_rings_font_and_landmarks() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let runtime = halley_config::parse_runtime_config(&config).expect("runtime config parses");

    assert_eq!(runtime.font.family, "monospace");
    assert_eq!(runtime.font.size, 11);
    assert_eq!(runtime.bearings, halley_config::Bearings::default());
    assert_eq!(runtime.focus_rings.for_output("DP-1").radius_x, 820.0);
    assert_eq!(runtime.focus_rings.for_output("DP-2").radius_y, 420.0);
    assert_eq!(runtime.landmarks.gap_px, 20.0);
    assert!(runtime.physics.enabled);
    assert_eq!(runtime.physics.damping, 0.45);
    assert_eq!(
        runtime.nodes.restore_centering,
        halley_config::RestoreCentering::Never
    );
}

#[test]
fn example_config_input_section_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let input = halley_config::parse_input(&config).expect("example input section parses");

    assert_eq!(input.repeat_rate, 30);
    assert_eq!(input.repeat_delay, 500);
    assert_eq!(input.focus_mode, halley_config::FocusMode::Click);
    assert!(input.raise_on_click);
    assert_eq!(input.keyboard.layout, "us");
    assert_eq!(input.gestures, halley_config::GestureSettings::default());
    assert_eq!(input.touchpad, halley_config::DeviceSettings::default());
    assert_eq!(input.mouse, halley_config::MouseSettings::default());
    assert_eq!(input.trackpoint, halley_config::DeviceSettings::default());
    assert_eq!(input.trackball, halley_config::DeviceSettings::default());
    assert_eq!(input.touchscreen, halley_config::DeviceSettings::default());
    assert!(input.devices.is_empty());
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
    assert_eq!(
        animations.window_open.motion,
        halley_config::AnimationMotion::Easing(halley_config::EasingMotion {
            duration_ms: 300,
            curve: halley_config::AnimationCurve::Linear,
        })
    );
}

#[test]
fn example_config_window_close_animation_parses() {
    let config = RuneConfig::from_file(EXAMPLE_PATH).expect("example config parses");
    let close = halley_config::parse_animations(&config).window_close;

    assert!(close.enabled);
    assert_eq!(
        close.animation_type,
        halley_config::WindowCloseAnimationType::Shrink
    );
    assert_eq!(close.duration_ms, 270);
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
