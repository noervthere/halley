use eventline::{LogLevel, LogPolicy, RunHeader};

const LOG_FILE_NAME: &str = "halley.log";

/// Initializes console logging first so runtime-directory or file failures
/// remain visible without preventing the compositor from starting.
pub fn init() {
    eventline::init_sync();
    eventline::enable_console_output(true);
    eventline::set_console_level(LogLevel::Info);
    eventline::enable_console_color(true);
    eventline::enable_console_duration(false);

    let runtime_dir = match halley_ipc::ensure_runtime_dir() {
        Ok(path) => path,
        Err(err) => {
            eventline::warn!("file logging unavailable: {err}");
            return;
        }
    };
    let log_path = runtime_dir.join(LOG_FILE_NAME);
    if let Err(err) = eventline::enable_file_output_rotating(
        &log_path,
        LogPolicy::default(),
        Some(RunHeader::new("halley")),
    ) {
        eventline::warn!(
            "failed to initialize file logging at {}: {err}",
            log_path.display()
        );
        return;
    }
    eventline::set_file_level(LogLevel::Debug);
    eventline::info!("file logging enabled at {}", log_path.display());
}

pub fn flush() {
    if let Err(err) = eventline::flush() {
        eventline::error!("failed to flush logging: {err}");
        let _ = eventline::flush();
    }
}
