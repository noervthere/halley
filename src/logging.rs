use std::fmt::Write as _;

use eventline::{LogLevel, LogPolicy, RunHeader};
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer, filter::Targets};

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
    init_smithay_bridge();
}

/// Forwards Smithay's `tracing` output into `eventline`, which otherwise never
/// sees it - Smithay is the only part of the process that logs through
/// `tracing`, so without a subscriber every DRM message it emits is discarded.
/// That blindness cost real debugging time on a DPMS wake stall: the modeset
/// path narrates what it is doing, and none of it reached the log.
///
/// Deliberately narrow, because this is diagnostic weight on a hot path.
/// Everything under `smithay` is `warn` and above; only the DRM backend is
/// opened up to `info`, which is where connector/mode changes are announced.
/// `debug` for that target is *not* wanted - the atomic surface debug-logs the
/// entire commit request, several screenfuls per modeset.
fn init_smithay_bridge() {
    let filter = Targets::new()
        .with_target("smithay", Level::WARN)
        .with_target("smithay::backend::drm", Level::INFO);

    if tracing_subscriber::registry()
        .with(EventlineLayer.with_filter(filter))
        .try_init()
        .is_err()
    {
        eventline::warn!("smithay log bridge unavailable: a tracing subscriber is already set");
    }
}

struct EventlineLayer;

impl<S: tracing::Subscriber> Layer<S> for EventlineLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        if message.is_empty() {
            return;
        }
        let target = event.metadata().target();
        match *event.metadata().level() {
            Level::ERROR => eventline::error!("{target}: {message}"),
            Level::WARN if known_benign_smithay_warning(target, &message) => {
                eventline::debug!("{target}: {message}")
            }
            Level::WARN => eventline::warn!("{target}: {message}"),
            // `info` and below land in the file only. The console is pinned to
            // `Info`, and a per-modeset connector list has no business there.
            _ => eventline::debug!("{target}: {message}"),
        }
    }
}

fn known_benign_smithay_warning(target: &str, message: &str) -> bool {
    // Smithay deliberately continues after this legacy drmSetMaster call:
    // current kernels authorize the modesetting fd without it. A genuine
    // permission/output failure still surfaces from DrmDevice/output setup.
    target == "smithay::backend::drm::device::fd"
        && message == "Unable to become drm master, assuming unprivileged mode"
}

/// Pulls out just the `message` field. Structured fields are dropped on
/// purpose: the DRM events worth reading put everything in the message, and
/// rendering the rest would bloat lines that already run long.
struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }
}

pub fn flush() {
    if let Err(err) = eventline::flush() {
        eventline::error!("failed to flush logging: {err}");
        let _ = eventline::flush();
    }
}

#[cfg(test)]
mod tests {
    use super::known_benign_smithay_warning;

    #[test]
    fn only_the_known_new_kernel_drm_fallback_is_downgraded() {
        assert!(known_benign_smithay_warning(
            "smithay::backend::drm::device::fd",
            "Unable to become drm master, assuming unprivileged mode"
        ));
        assert!(!known_benign_smithay_warning(
            "smithay::backend::drm::device::fd",
            "Failed to drop drm master state"
        ));
        assert!(!known_benign_smithay_warning(
            "smithay::backend::drm",
            "Unable to become drm master, assuming unprivileged mode"
        ));
    }
}
