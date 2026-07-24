mod codec;
mod types;

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub use codec::{CodecError, decode_request, decode_response, encode_request, encode_response, read_frame, write_frame};
pub use types::{HALLEY_IPC_VERSION, ModeInfo, OutputInfo, OutputsResponse, Request, Response, VersionInfo};

/// `$XDG_RUNTIME_DIR/halley/halley.sock` - matches old halley's own socket
/// path convention. No fallback for a missing `XDG_RUNTIME_DIR`: any real
/// Wayland session already requires it (the Wayland socket itself lives
/// there too), so a missing one means something more fundamental is wrong,
/// not a case worth a synthetic `/tmp` fallback for.
pub fn default_socket_path() -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(PathBuf::from(dir).join("halley").join("halley.sock"))
}

/// Connects to the running compositor's default socket, sends `req`, and
/// returns its response. One-shot: a fresh connection per call, matching
/// old halley's own client shape - there's no persistent-connection use
/// case yet for a CLI tool that runs once and exits.
pub fn send_request(req: &Request) -> Result<Response, CodecError> {
    let path = default_socket_path()?;
    send_request_to(&path, req)
}

pub fn send_request_to(path: &Path, req: &Request) -> Result<Response, CodecError> {
    let mut stream = UnixStream::connect(path)?;
    let bytes = encode_request(req)?;
    write_frame(&mut stream, &bytes)?;

    let resp_bytes = read_frame(&mut stream)?;
    decode_response(&resp_bytes)
}
