use std::error::Error;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use calloop::channel::{sync_channel, Event};
use calloop::LoopHandle;

const POLLING_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
struct FileProps {
    canonical: PathBuf,
    modified: SystemTime,
}

impl FileProps {
    fn read(path: &Path) -> std::io::Result<Self> {
        let canonical = path.canonicalize()?;
        let modified = canonical.metadata()?.modified()?;
        Ok(Self {
            canonical,
            modified,
        })
    }
}

struct ConfigFileState {
    path: PathBuf,
    last_props: Option<FileProps>,
}

impl ConfigFileState {
    fn new(path: PathBuf) -> Self {
        let last_props = FileProps::read(&path).ok();
        Self { path, last_props }
    }

    fn changed(&mut self) -> bool {
        let Ok(props) = FileProps::read(&self.path) else {
            return false;
        };
        if self.last_props.as_ref() == Some(&props) {
            return false;
        }
        self.last_props = Some(props);
        true
    }
}

/// Loads the initial validated snapshot and returns the path to watch.
/// Startup remains resilient: a missing/invalid file uses defaults, while a
/// later valid save is still picked up by the watcher.
pub fn load_initial() -> (Option<PathBuf>, halley_config::RuntimeConfig) {
    let Some(path) = halley_config::config_path() else {
        eprintln!("config: no config path resolvable, using defaults");
        return (None, halley_config::RuntimeConfig::default());
    };
    if let Err(err) = halley_config::bootstrap_default_config_at(&path) {
        eprintln!("config: failed to bootstrap default config: {err}");
    }

    let config = match halley_config::load_runtime_config_at(&path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config: failed to load {path:?}, using defaults: {err}");
            halley_config::RuntimeConfig::default()
        }
    };
    (Some(path), config)
}

/// Mirrors niri's deliberately simple config watcher: poll canonical path
/// plus mtime on a background thread, parse there, and deliver complete
/// snapshots onto the compositor event loop.
pub fn watch<App: 'static>(
    loop_handle: &LoopHandle<'_, App>,
    path: PathBuf,
    mut apply: impl FnMut(&mut App, halley_config::RuntimeConfig) + 'static,
) -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = sync_channel(1);
    thread::Builder::new()
        .name(format!("halley config watcher for {path:?}"))
        .spawn(move || {
            let mut state = ConfigFileState::new(path);
            loop {
                thread::sleep(POLLING_INTERVAL);
                if !state.changed() {
                    continue;
                }
                let loaded = halley_config::load_runtime_config_at(&state.path)
                    .map_err(|err| err.to_string());
                if sender.send(loaded).is_err() {
                    break;
                }
            }
        })?;

    loop_handle.insert_source(receiver, move |event, _, app| match event {
        Event::Msg(Ok(config)) => apply(app, config),
        Event::Msg(Err(err)) => {
            eprintln!("config: reload rejected, keeping last valid config: {err}")
        }
        Event::Closed => {}
    })?;
    Ok(())
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
    fn file_state_detects_create_and_replacement_but_not_unchanged_file() {
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
}
