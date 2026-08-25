use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rune_cfg::RuneConfig;

use crate::{Action, BindingScope, Keybind, ModifierKey};

pub const CONFIG_VERSION: u32 = 1;

const VERSION_PREFIX: &str = "# halley-config-version:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationMode {
    /// Startup migration of the canonical user config. Split configs are left
    /// alone because the root file may not own the keybind section.
    Automatic,
    /// A migration explicitly requested through `halleyctl config migrate`.
    Explicit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStatus {
    UpToDate,
    Skipped,
    WouldUpdate,
    Updated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    pub status: MigrationStatus,
    pub from_version: u32,
    pub to_version: u32,
    pub applied: Vec<String>,
    pub skipped: Vec<String>,
    pub reason: Option<String>,
    pub backup: Option<PathBuf>,
}

impl MigrationReport {
    fn up_to_date(version: u32) -> Self {
        Self {
            status: MigrationStatus::UpToDate,
            from_version: version,
            to_version: version,
            applied: Vec::new(),
            skipped: Vec::new(),
            reason: None,
            backup: None,
        }
    }

    fn skipped(version: u32, reason: impl Into<String>) -> Self {
        Self {
            status: MigrationStatus::Skipped,
            from_version: version,
            to_version: version,
            applied: Vec::new(),
            skipped: Vec::new(),
            reason: Some(reason.into()),
            backup: None,
        }
    }
}

#[derive(Debug)]
pub enum MigrationError {
    Io(io::Error),
    Invalid(String),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<io::Error> for MigrationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy)]
struct BindingCandidate {
    name: &'static str,
    line: &'static str,
}

/// Bindings added while the 0.6 compositor surface was being completed.
///
/// These are deliberately exact and finite. Config migration must never turn
/// into a template merge that floods a customized file with every new option.
const VERSION_1_BINDINGS: &[BindingCandidate] = &[
    BindingCandidate {
        name: "pin focused Field node",
        line: r#""$var.mod+p" "toggle-focused-pin""#,
    },
    BindingCandidate {
        name: "focus left",
        line: r#""$var.mod+left" "focus-left""#,
    },
    BindingCandidate {
        name: "focus right",
        line: r#""$var.mod+right" "focus-right""#,
    },
    BindingCandidate {
        name: "focus up",
        line: r#""$var.mod+up" "focus-up""#,
    },
    BindingCandidate {
        name: "focus down",
        line: r#""$var.mod+down" "focus-down""#,
    },
    BindingCandidate {
        name: "Trail previous",
        line: r#""$var.mod+comma" "trail-prev""#,
    },
    BindingCandidate {
        name: "Trail next",
        line: r#""$var.mod+period" "trail-next""#,
    },
    BindingCandidate {
        name: "move Field node left",
        line: r#""$var.mod+alt+left" "node-move left""#,
    },
    BindingCandidate {
        name: "move Field node right",
        line: r#""$var.mod+alt+right" "node-move right""#,
    },
    BindingCandidate {
        name: "move Field node up",
        line: r#""$var.mod+alt+up" "node-move up""#,
    },
    BindingCandidate {
        name: "move Field node down",
        line: r#""$var.mod+alt+down" "node-move down""#,
    },
    BindingCandidate {
        name: "swap cluster tile left",
        line: r#""$var.mod+ctrl+left" "cluster-tile-swap-left" with scope "tile""#,
    },
    BindingCandidate {
        name: "swap cluster tile right",
        line: r#""$var.mod+ctrl+right" "cluster-tile-swap-right" with scope "tile""#,
    },
    BindingCandidate {
        name: "swap cluster tile up",
        line: r#""$var.mod+ctrl+up" "cluster-tile-swap-up" with scope "tile""#,
    },
    BindingCandidate {
        name: "swap cluster tile down",
        line: r#""$var.mod+ctrl+down" "cluster-tile-swap-down" with scope "tile""#,
    },
    BindingCandidate {
        name: "resize Field window left",
        line: r#""$var.mod+ctrl+left" "resize-window-left" with scope "field""#,
    },
    BindingCandidate {
        name: "resize Field window right",
        line: r#""$var.mod+ctrl+right" "resize-window-right" with scope "field""#,
    },
    BindingCandidate {
        name: "resize Field window up",
        line: r#""$var.mod+ctrl+up" "resize-window-up" with scope "field""#,
    },
    BindingCandidate {
        name: "resize Field window down",
        line: r#""$var.mod+ctrl+down" "resize-window-down" with scope "field""#,
    },
    BindingCandidate {
        name: "focus monitor left",
        line: r#""$var.mod+shift+left" "monitor-focus left""#,
    },
    BindingCandidate {
        name: "focus monitor right",
        line: r#""$var.mod+shift+right" "monitor-focus right""#,
    },
    BindingCandidate {
        name: "focus monitor up",
        line: r#""$var.mod+shift+up" "monitor-focus up""#,
    },
    BindingCandidate {
        name: "focus monitor down",
        line: r#""$var.mod+shift+down" "monitor-focus down""#,
    },
    BindingCandidate {
        name: "manual config reload",
        line: r#""$var.mod+shift+r" "reload""#,
    },
    BindingCandidate {
        name: "pointer window move",
        line: r#""$var.mod+click-left" "move-window""#,
    },
    BindingCandidate {
        name: "pointer window resize",
        line: r#""$var.mod+click-right" "resize-window""#,
    },
    BindingCandidate {
        name: "pointer Field pan",
        line: r#""click-left" "pan-field""#,
    },
];

pub fn migrate_config_at(
    path: &Path,
    mode: MigrationMode,
    dry_run: bool,
) -> Result<MigrationReport, MigrationError> {
    let source = fs::read_to_string(path)?;
    let from_version = config_version(&source)?;
    if from_version == CONFIG_VERSION {
        return Ok(MigrationReport::up_to_date(from_version));
    }
    if from_version > CONFIG_VERSION {
        return Err(MigrationError::Invalid(format!(
            "config version {from_version} is newer than this Halley build (supports {CONFIG_VERSION})"
        )));
    }
    if mode == MigrationMode::Automatic && contains_gather(&source) {
        return Ok(MigrationReport::skipped(
            from_version,
            "split config uses gather; run `halleyctl config migrate --dry-run` to inspect it",
        ));
    }

    let runtime = crate::load_runtime_config_at(path).map_err(|error| {
        MigrationError::Invalid(format!(
            "existing configuration is invalid; leaving it unchanged: {error}"
        ))
    })?;
    let mut known_binds = runtime.keybinds.binds;
    let modifier = runtime.keybinds.modifier;
    let mut accepted = Vec::new();
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for candidate in VERSION_1_BINDINGS {
        let binding = parse_candidate(modifier, candidate.line)?;
        if known_binds
            .iter()
            .any(|existing| existing.action == binding.action)
        {
            continue;
        }
        if let Some(conflict) = known_binds
            .iter()
            .find(|existing| bindings_conflict(existing, &binding))
        {
            skipped.push(format!(
                "{}: chord is already occupied in {:?} scope",
                candidate.name, conflict.scope
            ));
            continue;
        }
        known_binds.push(binding);
        accepted.push(*candidate);
        applied.push(candidate.name.to_string());
    }

    let mut updated = source;
    if !accepted.is_empty() {
        updated = insert_bindings(&updated, &accepted)?;
    }
    updated = set_config_version(&updated, CONFIG_VERSION)?;
    validate_candidate(path, &updated)?;

    let status = if dry_run {
        MigrationStatus::WouldUpdate
    } else {
        MigrationStatus::Updated
    };
    let backup = if dry_run {
        None
    } else {
        let backup = backup_path(path)?;
        fs::copy(path, &backup)?;
        if let Err(error) = atomic_replace(path, updated.as_bytes()) {
            return Err(MigrationError::Invalid(format!(
                "failed to install migrated config (backup retained at {}): {error}",
                backup.display()
            )));
        }
        Some(backup)
    };

    Ok(MigrationReport {
        status,
        from_version,
        to_version: CONFIG_VERSION,
        applied,
        skipped,
        reason: None,
        backup,
    })
}

fn config_version(source: &str) -> Result<u32, MigrationError> {
    let markers = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix(VERSION_PREFIX))
        .collect::<Vec<_>>();
    match markers.as_slice() {
        [] => Ok(0),
        [value] => value.trim().parse::<u32>().map_err(|_| {
            MigrationError::Invalid(format!("invalid Halley config version marker {value:?}"))
        }),
        _ => Err(MigrationError::Invalid(
            "configuration contains more than one Halley version marker".to_string(),
        )),
    }
}

fn contains_gather(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with('#')
            && line
                .strip_prefix("gather")
                .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
    })
}

fn parse_candidate(modifier: ModifierKey, line: &str) -> Result<Keybind, MigrationError> {
    let source = format!(
        "keybinds:\n  mod \"{}\"\n  {line}\nend\n",
        modifier_name(modifier)
    );
    let config = RuneConfig::from_str(&source).map_err(|error| {
        MigrationError::Invalid(format!("internal migration binding is invalid: {error}"))
    })?;
    let mut keybinds = crate::parse_keybinds(&config).map_err(|error| {
        MigrationError::Invalid(format!("internal migration binding is invalid: {error}"))
    })?;
    keybinds.binds.pop().ok_or_else(|| {
        MigrationError::Invalid("internal migration binding produced no action".to_string())
    })
}

fn modifier_name(modifier: ModifierKey) -> &'static str {
    match modifier {
        ModifierKey::Super => "super",
        ModifierKey::LeftSuper => "left-super",
        ModifierKey::RightSuper => "right-super",
        ModifierKey::Alt => "alt",
        ModifierKey::LeftAlt => "left-alt",
        ModifierKey::RightAlt => "right-alt",
        ModifierKey::Ctrl => "ctrl",
        ModifierKey::LeftCtrl => "left-ctrl",
        ModifierKey::RightCtrl => "right-ctrl",
        ModifierKey::Shift => "shift",
        ModifierKey::LeftShift => "left-shift",
        ModifierKey::RightShift => "right-shift",
    }
}

fn bindings_conflict(existing: &Keybind, candidate: &Keybind) -> bool {
    if existing.modifiers != candidate.modifiers
        || !existing.key.eq_ignore_ascii_case(&candidate.key)
        || !scopes_overlap(existing.scope, candidate.scope)
    {
        return false;
    }

    !matches!(
        (&existing.action, &candidate.action),
        (Action::ClusterTileFocus(old), Action::FocusDirection(new)) if old == new
    )
}

fn scopes_overlap(left: BindingScope, right: BindingScope) -> bool {
    use BindingScope::{Cluster, Global, Stack, Tile};
    matches!(left, Global)
        || matches!(right, Global)
        || left == right
        || matches!(
            (left, right),
            (Cluster, Tile | Stack) | (Tile | Stack, Cluster)
        )
}

fn insert_bindings(source: &str, bindings: &[BindingCandidate]) -> Result<String, MigrationError> {
    let Some(offset) = keybind_end_offset(source) else {
        return Err(MigrationError::Invalid(
            "the selected root file does not own a keybinds section; migrate the gathered keybind file explicitly"
                .to_string(),
        ));
    };
    let mut block = String::new();
    if !source[..offset].ends_with("\n\n") {
        block.push('\n');
    }
    block.push_str(
        "  # Added by Halley config migration 1. These remain ordinary, editable bindings.\n",
    );
    for binding in bindings {
        block.push_str("  ");
        block.push_str(binding.line);
        block.push('\n');
    }
    let mut updated = String::with_capacity(source.len() + block.len());
    updated.push_str(&source[..offset]);
    updated.push_str(&block);
    updated.push_str(&source[offset..]);
    Ok(updated)
}

fn keybind_end_offset(source: &str) -> Option<usize> {
    let mut offset = 0usize;
    let mut depth = 0usize;
    let mut in_keybinds = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if !in_keybinds {
            if trimmed == "keybinds:" {
                in_keybinds = true;
                depth = 1;
            }
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            if trimmed == "end" {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(offset);
                }
            } else if trimmed.ends_with(':') {
                depth += 1;
            }
        }
        offset += line.len();
    }
    None
}

fn set_config_version(source: &str, version: u32) -> Result<String, MigrationError> {
    let marker = format!("{VERSION_PREFIX} {version}");
    let mut offset = 0usize;
    let mut found = None;
    for line in source.split_inclusive('\n') {
        if line.trim().starts_with(VERSION_PREFIX) {
            let line_end = offset + line.trim_end_matches(['\r', '\n']).len();
            if found.replace((offset, line_end)).is_some() {
                return Err(MigrationError::Invalid(
                    "configuration contains more than one Halley version marker".to_string(),
                ));
            }
        }
        offset += line.len();
    }
    if let Some((start, end)) = found {
        let mut updated = source.to_string();
        updated.replace_range(start..end, &marker);
        return Ok(updated);
    }

    let insert_at = metadata_prefix_end(source);
    let mut updated = String::with_capacity(source.len() + marker.len() + 1);
    updated.push_str(&source[..insert_at]);
    updated.push_str(&marker);
    updated.push('\n');
    updated.push_str(&source[insert_at..]);
    Ok(updated)
}

fn metadata_prefix_end(source: &str) -> usize {
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        if !line.trim_start().starts_with('@') {
            break;
        }
        offset += line.len();
    }
    offset
}

fn validate_candidate(path: &Path, source: &str) -> Result<(), MigrationError> {
    let temp = temporary_path(path, "validate")?;
    let result = (|| {
        write_new_file(&temp, source.as_bytes(), path)?;
        crate::load_runtime_config_at(&temp).map_err(|error| {
            MigrationError::Invalid(format!(
                "migrated configuration did not validate; leaving the original unchanged: {error}"
            ))
        })?;
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result
}

fn backup_path(path: &Path) -> Result<PathBuf, MigrationError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| MigrationError::Invalid(format!("system clock error: {error}")))?
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MigrationError::Invalid("config path has no UTF-8 file name".to_string()))?;
    Ok(path.with_file_name(format!("{file_name}.bak-{timestamp}")))
}

fn temporary_path(path: &Path, purpose: &str) -> Result<PathBuf, MigrationError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| MigrationError::Invalid(format!("system clock error: {error}")))?
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MigrationError::Invalid("config path has no UTF-8 file name".to_string()))?;
    Ok(path.with_file_name(format!(
        ".{file_name}.{purpose}-{}-{timestamp}",
        std::process::id()
    )))
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<(), MigrationError> {
    let temp = temporary_path(path, "migrate")?;
    if let Err(error) = write_new_file(&temp, contents, path) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(MigrationError::Io(error));
    }
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn write_new_file(
    path: &Path,
    contents: &[u8],
    permissions_from: &Path,
) -> Result<(), MigrationError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Ok(metadata) = fs::metadata(permissions_from) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "halley-migrate-{}-{name}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config(&self) -> PathBuf {
            self.0.join("halley.rune")
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn minimal(bindings: &str) -> String {
        format!("keybinds:\n  mod \"super\"\n{bindings}end\n")
    }

    #[test]
    fn dry_run_backfills_missing_bindings_without_writing() {
        let scratch = ScratchDir::new("dry-run");
        let path = scratch.config();
        let original = minimal("  \"$var.mod+q\" \"close-focused\"\n");
        fs::write(&path, &original).unwrap();

        let report = migrate_config_at(&path, MigrationMode::Explicit, true).unwrap();

        assert_eq!(report.status, MigrationStatus::WouldUpdate);
        assert!(report.applied.iter().any(|name| name == "Trail previous"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(report.backup.is_none());
    }

    #[test]
    fn migration_is_atomic_validated_backed_up_and_idempotent() {
        let scratch = ScratchDir::new("write");
        let path = scratch.config();
        let original = minimal("  \"$var.mod+q\" \"close-focused\"\n");
        fs::write(&path, &original).unwrap();

        let report = migrate_config_at(&path, MigrationMode::Explicit, false).unwrap();

        assert_eq!(report.status, MigrationStatus::Updated);
        let backup = report.backup.expect("backup path");
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains("# halley-config-version: 1"));
        assert!(updated.contains("\"$var.mod+comma\" \"trail-prev\""));
        crate::load_runtime_config_at(&path).expect("migrated config validates");

        assert_eq!(
            migrate_config_at(&path, MigrationMode::Explicit, false)
                .unwrap()
                .status,
            MigrationStatus::UpToDate
        );
    }

    #[test]
    fn a_custom_overlapping_chord_is_not_overwritten() {
        let scratch = ScratchDir::new("conflict");
        let path = scratch.config();
        fs::write(&path, minimal("  \"$var.mod+p\" \"notify-send custom\"\n")).unwrap();

        let report = migrate_config_at(&path, MigrationMode::Explicit, false).unwrap();
        let updated = fs::read_to_string(&path).unwrap();

        assert!(
            report
                .skipped
                .iter()
                .any(|item| item.contains("pin focused"))
        );
        assert!(updated.contains("\"$var.mod+p\" \"notify-send custom\""));
        assert!(!updated.contains("\"$var.mod+p\" \"toggle-focused-pin\""));
    }

    #[test]
    fn scoped_resize_can_share_a_chord_with_tile_swap() {
        let scratch = ScratchDir::new("scopes");
        let path = scratch.config();
        fs::write(
            &path,
            minimal("  \"$var.mod+ctrl+left\" \"cluster-tile-swap-left\" with scope \"tile\"\n"),
        )
        .unwrap();

        migrate_config_at(&path, MigrationMode::Explicit, false).unwrap();
        let updated = fs::read_to_string(&path).unwrap();

        assert!(
            updated.contains("\"$var.mod+ctrl+left\" \"resize-window-left\" with scope \"field\"")
        );
    }

    #[test]
    fn automatic_migration_skips_gathered_configs() {
        let scratch = ScratchDir::new("gather");
        let path = scratch.config();
        let keys = scratch.0.join("keys.rune");
        fs::write(&path, "gather \"keys.rune\"\n").unwrap();
        fs::write(&keys, minimal("  \"$var.mod+q\" \"close-focused\"\n")).unwrap();

        let report = migrate_config_at(&path, MigrationMode::Automatic, false).unwrap();

        assert_eq!(report.status, MigrationStatus::Skipped);
        assert!(report.reason.unwrap().contains("gather"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "gather \"keys.rune\"\n");
    }

    #[test]
    fn explicit_migration_reports_ambiguous_gather_owner() {
        let scratch = ScratchDir::new("gather-explicit");
        let path = scratch.config();
        let keys = scratch.0.join("keys.rune");
        fs::write(&path, "gather \"keys.rune\"\n").unwrap();
        fs::write(&keys, minimal("  \"$var.mod+q\" \"close-focused\"\n")).unwrap();

        let error = migrate_config_at(&path, MigrationMode::Explicit, true).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not own a keybinds section")
        );
        assert!(!fs::read_to_string(&path).unwrap().contains(VERSION_PREFIX));
    }

    #[test]
    fn metadata_stays_at_the_start_of_the_file() {
        let source = format!(
            "@author \"Dustin\"\n@description \"Halley\"\n{}",
            minimal("")
        );
        let updated = set_config_version(&source, CONFIG_VERSION).unwrap();
        assert!(updated.starts_with(
            "@author \"Dustin\"\n@description \"Halley\"\n# halley-config-version: 1\n"
        ));
    }

    #[test]
    fn future_config_version_is_rejected() {
        let scratch = ScratchDir::new("future");
        let path = scratch.config();
        fs::write(
            &path,
            format!("# halley-config-version: 99\n{}", minimal("")),
        )
        .unwrap();

        let error = migrate_config_at(&path, MigrationMode::Explicit, true).unwrap_err();
        assert!(error.to_string().contains("newer than this Halley build"));
    }
}
