mod codec;
mod types;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub use codec::{CodecError, decode_request, decode_response, encode_request, encode_response, read_frame, write_frame};
pub use types::{HALLEY_IPC_VERSION, ModeInfo, OutputInfo, OutputsResponse, Request, Response, VersionInfo};

fn runtime_dir_from(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join("halley")
}

/// `$XDG_RUNTIME_DIR/halley`, shared by Halley's IPC socket and runtime log.
/// Resolving the path has no filesystem side effects.
pub fn halley_runtime_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(runtime_dir_from(PathBuf::from(base)))
}

fn ensure_runtime_dir_at(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Creates Halley's private runtime directory and enforces owner-only access.
pub fn ensure_runtime_dir() -> io::Result<PathBuf> {
    let path = halley_runtime_dir()?;
    ensure_runtime_dir_at(&path)?;
    Ok(path)
}

/// `$XDG_RUNTIME_DIR/halley/halley.sock` - matches old halley's own socket
/// path convention. No fallback for a missing `XDG_RUNTIME_DIR`: any real
/// Wayland session already requires it (the Wayland socket itself lives
/// there too), so a missing one means something more fundamental is wrong,
/// not a case worth a synthetic `/tmp` fallback for.
pub fn default_socket_path() -> io::Result<PathBuf> {
    Ok(halley_runtime_dir()?.join("halley.sock"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "halley-ipc-runtime-test-{}-{unique}",
                std::process::id()
            )))
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn runtime_and_socket_paths_share_the_halley_directory() {
        let runtime = runtime_dir_from("/run/user/1000");
        assert_eq!(runtime, PathBuf::from("/run/user/1000/halley"));
        assert_eq!(
            runtime.join("halley.sock"),
            PathBuf::from("/run/user/1000/halley/halley.sock")
        );
        assert_eq!(
            runtime.join("halley.log"),
            PathBuf::from("/run/user/1000/halley/halley.log")
        );
    }

    #[test]
    fn runtime_directory_is_private() {
        let scratch = ScratchDir::new();
        let runtime = scratch.0.join("nested").join("halley");

        ensure_runtime_dir_at(&runtime).unwrap();

        assert!(runtime.is_dir());
        assert_eq!(
            fs::metadata(runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
