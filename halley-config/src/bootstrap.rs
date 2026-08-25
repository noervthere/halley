use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The default config file contents are identical to the shipped top-level
/// example. Keeping one canonical template for the whole workspace prevents
/// fresh-install behavior and user-facing documentation from drifting apart.
pub const DEFAULT_CONFIG: &str = include_str!("../../examples/halley.rune");

/// Resolve the config file path: `$XDG_CONFIG_HOME/halley/halley.rune`,
/// falling back to `$HOME/.config/halley/halley.rune` when
/// `XDG_CONFIG_HOME` is unset. Returns `None` if neither env var is set -
/// no sensible home to put a config in.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("halley").join("halley.rune"))
}

/// If no config file exists yet at `config_path()`, write `DEFAULT_CONFIG`
/// there (creating parent directories as needed). Returns `Ok(true)` if a
/// file was actually written, `Ok(false)` if one already existed or the
/// path couldn't be resolved. Never overwrites an existing config - a
/// user's edits are never at risk from this.
pub fn bootstrap_default_config() -> io::Result<bool> {
    let Some(path) = config_path() else {
        return Ok(false);
    };
    bootstrap_default_config_at(&path)
}

/// Same as `bootstrap_default_config`, but against an explicit path - the
/// actual logic, factored out so it's testable against a temp directory
/// instead of the real `$HOME`.
pub fn bootstrap_default_config_at(path: &Path) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, DEFAULT_CONFIG)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune_cfg::RuneConfig;

    /// A fresh, uniquely-named scratch directory under the OS temp dir,
    /// scoped to one test so parallel test runs never collide.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(test_name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "halley-config-test-{}-{}-{}",
                std::process::id(),
                test_name,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_default_config_when_absent() {
        let scratch = ScratchDir::new("writes_default_config_when_absent");
        let config_file = scratch.path().join("halley").join("halley.rune");

        let wrote = bootstrap_default_config_at(&config_file).unwrap();

        assert!(wrote);
        assert!(config_file.exists());
        assert_eq!(fs::read_to_string(&config_file).unwrap(), DEFAULT_CONFIG);
    }

    #[test]
    fn does_not_overwrite_existing_config() {
        let scratch = ScratchDir::new("does_not_overwrite_existing_config");
        let config_file = scratch.path().join("halley").join("halley.rune");
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(&config_file, "keybinds:\n  mod \"alt\"\nend\n").unwrap();

        let wrote = bootstrap_default_config_at(&config_file).unwrap();

        assert!(!wrote);
        assert_eq!(
            fs::read_to_string(&config_file).unwrap(),
            "keybinds:\n  mod \"alt\"\nend\n"
        );
    }

    #[test]
    fn creates_missing_parent_directories() {
        let scratch = ScratchDir::new("creates_missing_parent_directories");
        let config_file = scratch
            .path()
            .join("nested")
            .join("halley")
            .join("halley.rune");
        assert!(!config_file.parent().unwrap().exists());

        let wrote = bootstrap_default_config_at(&config_file).unwrap();

        assert!(wrote);
        assert!(config_file.exists());
    }

    #[test]
    fn template_contains_overview_and_old_halley_controls() {
        for expected in [
            "\"$var.mod+n\" \"toggle-state\"",
            "\"$var.mod+o\" \"apogee\"",
            "\"alt+tab\" \"cycle-focus\"",
            "\"$var.mod+h\" \"center-last-focused\"",
            "\"$var.mod+p\" \"toggle-focused-pin\"",
            "\"$var.mod+left\" \"focus-left\"",
            "\"$var.mod+ctrl+right\" \"cluster-tile-swap-right\"",
            "\"$var.mod+shift+up\" \"monitor-focus up\"",
            "live-previews true",
            "max-rows 3",
            "overlays:",
            "notifications:",
            "success-duration-ms 4000",
            "\"retract\" - reverse \"launch\"",
            "close-restore-nodes false",
            "maximize:",
            "motion \"easing\"",
            "duration-ms 240",
            "damping-ratio 1.0",
            "stiffness 800.0",
            "bloom-direction \"clockwise\"",
            "resize-using-border true",
            "hide-on-keyboard-nav true",
            "pins:",
            "corner \"top-right\"",
            "colour \"#d65d26\"",
            "background-colour \"auto\"",
            "size 1.0",
            "titlebars:",
            "button-position \"left\"",
            "title-position \"center\"",
            "show-icons false",
            "show-title true",
            "height 32",
            "XF86AudioRaiseVolume\" \"wpctl",
            "with repeat true",
            "overlay-fps false",
            "halleyctl gamescope run -- %command%",
            "output-width \"auto\"",
        ] {
            assert!(
                DEFAULT_CONFIG.contains(expected),
                "bootstrap template is missing {expected:?}"
            );
        }
        assert!(!DEFAULT_CONFIG.contains("border-colour-hover"));
        assert!(!DEFAULT_CONFIG.contains("border-colour-inactive"));
    }

    #[test]
    fn template_uses_ring_only_view_entries() {
        let config = RuneConfig::from_str(DEFAULT_CONFIG).expect("bootstrap template parses");
        let view = crate::parse_view_checked(&config).expect("bootstrap view parses");

        assert!(view.outputs.is_empty());
        assert_eq!(view.focus_rings.by_output.len(), 2);
        assert!(view.focus_rings.by_output.contains_key("DP-1"));
        assert!(view.focus_rings.by_output.contains_key("DP-2"));
    }
}
