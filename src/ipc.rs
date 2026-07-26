use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode as CalloopMode, PostAction};
use smithay::output::Mode as OutputMode;

/// Backend-agnostic access to real per-output info, mirroring `Renderable`'s
/// existing shape (one small trait, implemented once per backend) rather
/// than threading backend-specific types through the IPC layer.
pub trait OutputInfoSource {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo>;
}

/// Shared by both backends' `OutputInfoSource` impls - keeps Smithay's
/// millihertz refresh representation intact on the wire.
pub fn mode_info(mode: OutputMode, preferred: bool) -> halley_ipc::ModeInfo {
    halley_ipc::ModeInfo {
        width: mode.size.w,
        height: mode.size.h,
        refresh_millihz: mode.refresh,
        preferred,
    }
}

pub fn vrr_str(vrr: halley_config::Vrr) -> &'static str {
    match vrr {
        halley_config::Vrr::Off => "off",
        halley_config::Vrr::On => "on",
        halley_config::Vrr::Auto => "auto",
    }
}

fn version_info() -> halley_ipc::VersionInfo {
    halley_ipc::VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        ipc_protocol: halley_ipc::HALLEY_IPC_VERSION,
    }
}

/// Answers one already-connected client synchronously: read one request
/// frame, reply with one response frame, done. `halleyctl`'s own client is
/// a one-shot connect/request/reply/disconnect (see `halley-ipc`'s
/// `send_request`), so there's nothing to keep this connection open for -
/// no per-client registered source or background thread needed, unlike old
/// halley's IPC (which needs both because most of its commands mutate
/// compositor state and have to hop onto the main-loop thread; this first
/// pass is entirely read-only cached-snapshot queries answered inline).
fn handle_client(mut stream: UnixStream, outputs: &[halley_ipc::OutputInfo]) {
    let response = match halley_ipc::read_frame(&mut stream).and_then(|bytes| halley_ipc::decode_request(&bytes)) {
        Ok(halley_ipc::Request::Outputs) => halley_ipc::Response::Outputs(halley_ipc::OutputsResponse {
            outputs: outputs.to_vec(),
        }),
        Ok(halley_ipc::Request::Version) => halley_ipc::Response::Version(version_info()),
        Err(err) => halley_ipc::Response::Error(err.to_string()),
    };

    let Ok(bytes) = halley_ipc::encode_response(&response) else {
        eventline::error!("ipc: failed to encode response");
        return;
    };
    if let Err(err) = halley_ipc::write_frame(&mut stream, &bytes) {
        eventline::warn!("ipc: failed to write response: {err}");
    }
}

/// If a socket file already exists at `path`, checks whether it's actually
/// live (another halley IPC listener still holds it) or stale (the process
/// that created it is gone) - refuses to start in the former case, removes
/// the file and proceeds in the latter. Mirrors old halley's own
/// stale-socket handling.
fn remove_stale_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::other(
            "another halley IPC listener is already active on this socket",
        )),
        Err(_) => std::fs::remove_file(path),
    }
}

/// Binds the IPC socket and wires it into the given event loop as another
/// `calloop` source, exactly like `init_wayland_listener` already does for
/// the Wayland socket - `output_info` is called fresh per accepted
/// connection so it always reflects live state.
pub fn init_ipc_listener<App: 'static>(
    loop_handle: &LoopHandle<'_, App>,
    output_info: impl Fn(&App) -> Vec<halley_ipc::OutputInfo> + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = halley_ipc::ensure_runtime_dir()?.join("halley.sock");
    remove_stale_socket(&path)?;

    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    loop_handle.insert_source(Generic::new(listener, Interest::READ, CalloopMode::Level), move |_, listener, app| {
        loop {
            match listener.accept() {
                Ok((stream, _addr)) => handle_client(stream, &output_info(app)),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => {
                    eventline::error!("ipc: accept failed: {err}");
                    break;
                }
            }
        }
        Ok(PostAction::Continue)
    })?;

    Ok(())
}
