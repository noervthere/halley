use std::error::Error;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use calloop::LoopHandle;
use calloop::channel::{Event, sync_channel};

const POLLING_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
enum FileProps {
    Present {
        canonical: PathBuf,
        modified: SystemTime,
        len: u64,
        device: u64,
        inode: u64,
    },
    Unavailable(String),
}

impl FileProps {
    fn read(path: &Path) -> Self {
        let result = (|| {
            let canonical = path.canonicalize()?;
            let metadata = canonical.metadata()?;
            Ok::<_, std::io::Error>(Self::Present {
                canonical,
                modified: metadata.modified()?,
                len: metadata.len(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        })();
        result.unwrap_or_else(|error| {
            Self::Unavailable(format!(
                "{:?}:{}",
                error.kind(),
                error.raw_os_error().unwrap_or(0)
            ))
        })
    }
}

struct ConfigFileState {
    path: PathBuf,
    last_props: FileProps,
}

impl ConfigFileState {
    fn new(path: PathBuf) -> Self {
        let last_props = FileProps::read(&path);
        Self { path, last_props }
    }

    fn changed(&mut self) -> bool {
        let props = FileProps::read(&self.path);
        if self.last_props == props {
            return false;
        }
        self.last_props = props;
        true
    }
}

#[derive(Clone, Debug)]
pub struct InitialConfig {
    pub path: Option<PathBuf>,
    pub config: halley_config::RuntimeConfig,
    pub diagnostic: Option<halley_config::ConfigDiagnostic>,
}

#[derive(Clone, Debug)]
pub enum ConfigReload {
    Loaded(Box<halley_config::RuntimeConfig>),
    Rejected(halley_config::ConfigDiagnostic),
}

#[derive(Clone)]
pub struct ConfigWatcher {
    reload_requested: Arc<AtomicBool>,
}

impl ConfigWatcher {
    pub fn request_reload(&self) {
        self.reload_requested.store(true, Ordering::Release);
    }
}

/// Resolve a user-supplied path without requiring the file to exist.
///
/// Keeping the lexical path, rather than canonicalizing it, lets Halley
/// report and watch an explicitly selected file that will be created later.
pub fn absolute_path(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Loads the initial validated snapshot and retains the exact path that later
/// reloads and `halleyctl config verify` must use.
pub fn load_initial(explicit_path: Option<PathBuf>) -> InitialConfig {
    let explicit = explicit_path.is_some();
    let selected = match explicit_path {
        Some(path) => absolute_path(path).map(Some),
        None => match halley_config::config_path() {
            Some(path) => absolute_path(path).map(Some),
            None => Ok(None),
        },
    };

    let path = match selected {
        Ok(path) => path,
        Err(error) => {
            let diagnostic = halley_config::ConfigDiagnostic::message(
                None,
                format!("could not resolve the configuration path: {error}"),
            );
            eventline::warn!("config: {}, using defaults", diagnostic.message);
            return InitialConfig {
                path: None,
                config: halley_config::RuntimeConfig::default(),
                diagnostic: Some(diagnostic),
            };
        }
    };

    let Some(path) = path else {
        let diagnostic = halley_config::ConfigDiagnostic::message(
            None,
            "no configuration path is available because HOME and XDG_CONFIG_HOME are unset",
        );
        eventline::warn!("config: {}, using defaults", diagnostic.message);
        return InitialConfig {
            path: None,
            config: halley_config::RuntimeConfig::default(),
            diagnostic: Some(diagnostic),
        };
    };

    if !explicit && let Err(error) = halley_config::bootstrap_default_config_at(&path) {
        eventline::warn!("config: failed to bootstrap default config: {error}");
    }

    match halley_config::load_runtime_config_diagnostic_at(&path) {
        Ok(config) => InitialConfig {
            path: Some(path),
            config,
            diagnostic: None,
        },
        Err(diagnostic) => {
            eventline::warn!(
                "config: failed to load {:?}, using defaults: {}",
                diagnostic.path,
                diagnostic.message
            );
            InitialConfig {
                path: Some(path),
                config: halley_config::RuntimeConfig::default(),
                diagnostic: Some(diagnostic),
            }
        }
    }
}

/// Polls the selected file identity and mtime on a background thread, parses
/// there, and delivers both valid snapshots and rejected attempts onto the
/// compositor event loop.
pub fn watch<App: 'static>(
    loop_handle: &LoopHandle<'_, App>,
    path: PathBuf,
    mut notify: impl FnMut(&mut App, ConfigReload) + 'static,
) -> Result<ConfigWatcher, Box<dyn Error>> {
    let (sender, receiver) = sync_channel(1);
    let reload_requested = Arc::new(AtomicBool::new(false));
    let watcher_reload_requested = reload_requested.clone();
    thread::Builder::new()
        .name(format!("halley config watcher for {path:?}"))
        .spawn(move || {
            let mut state = ConfigFileState::new(path);
            loop {
                thread::sleep(POLLING_INTERVAL);
                let forced = watcher_reload_requested.swap(false, Ordering::AcqRel);
                if !forced && !state.changed() {
                    continue;
                }
                let loaded = halley_config::load_runtime_config_diagnostic_at(&state.path);
                if sender.send(loaded).is_err() {
                    break;
                }
            }
        })?;

    loop_handle.insert_source(receiver, move |event, _, app| {
        if let Event::Msg(loaded) = event {
            notify(app, classify_reload(loaded));
        }
    })?;
    Ok(ConfigWatcher { reload_requested })
}

fn classify_reload(
    loaded: Result<halley_config::RuntimeConfig, halley_config::ConfigDiagnostic>,
) -> ConfigReload {
    match loaded {
        Ok(config) => ConfigReload::Loaded(Box::new(config)),
        Err(diagnostic) => {
            eventline::warn!(
                "config: reload rejected, keeping last valid config: {}",
                diagnostic.message
            );
            ConfigReload::Rejected(diagnostic)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};
    use std::time::UNIX_EPOCH;

    struct ScratchFile {
        path: PathBuf,
    }

    impl ScratchFile {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("halley-config-watch-{}-{name}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self {
                path: dir.join("halley.rune"),
            }
        }
    }

    impl Drop for ScratchFile {
        fn drop(&mut self) {
            if let Some(parent) = self.path.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }

    #[test]
    fn file_state_detects_create_delete_and_replacement_but_not_unchanged_file() {
        let scratch = ScratchFile::new("props");
        let mut state = ConfigFileState::new(scratch.path.clone());
        assert!(!state.changed());

        fs::write(&scratch.path, "first").unwrap();
        File::options()
            .write(true)
            .open(&scratch.path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
            .unwrap();
        assert!(state.changed());
        assert!(!state.changed());

        fs::remove_file(&scratch.path).unwrap();
        assert!(state.changed());
        assert!(!state.changed());

        let replacement = scratch.path.with_extension("new");
        fs::write(&replacement, "second").unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(2)))
            .unwrap();
        fs::rename(&replacement, &scratch.path).unwrap();
        assert!(state.changed());
    }

    #[test]
    fn explicit_missing_path_is_not_bootstrapped() {
        let scratch = ScratchFile::new("explicit");
        let initial = load_initial(Some(scratch.path.clone()));

        assert_eq!(initial.path.as_deref(), Some(scratch.path.as_path()));
        assert!(initial.diagnostic.is_some());
        assert!(!scratch.path.exists());
    }

    #[test]
    fn relative_paths_are_resolved_against_the_launch_directory() {
        let relative = PathBuf::from("relative/halley.rune");
        assert_eq!(
            absolute_path(relative.clone()).unwrap(),
            std::env::current_dir().unwrap().join(relative)
        );
    }

    #[test]
    fn rejected_live_reload_keeps_structured_diagnostics() {
        let diagnostic =
            halley_config::ConfigDiagnostic::message(None, "invalid keybind".to_string());
        assert!(matches!(
            classify_reload(Err(diagnostic)),
            ConfigReload::Rejected(_)
        ));
        assert!(matches!(
            classify_reload(Ok(halley_config::RuntimeConfig::default())),
            ConfigReload::Loaded(_)
        ));
    }
}
