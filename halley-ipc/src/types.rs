use serde::{Deserialize, Serialize};

/// Bumped whenever `Request`/`Response` change shape. `postcard` encodes
/// enum variants positionally (by index, not by name), so unlike the tidy
/// config parsers elsewhere in this project, adding a variant anywhere but
/// the end of `Request`/`Response` silently breaks wire-compatibility with
/// a differently-versioned build - worth remembering as this grows, not
/// solved here (this first pass has nothing to negotiate against yet).
pub const HALLEY_IPC_VERSION: u32 = 2;

/// A request from `halleyctl` (or any other client) to the compositor.
/// Deliberately just the two read-only queries this first pass needs -
/// nothing here mutates compositor state yet (see the plan's forward note
/// on why that changes the transport design once it does).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Outputs,
    Version,
}

/// The compositor's reply to a `Request`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Outputs(OutputsResponse),
    Version(VersionInfo),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputsResponse {
    pub outputs: Vec<OutputInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    /// Every mode advertised by the connector, in connector order.
    pub modes: Vec<ModeInfo>,
    /// Index of the active mode in `modes`, or `None` when this connected
    /// output could not be initialized.
    pub current_mode: Option<usize>,
    pub offset_x: i32,
    pub offset_y: i32,
    /// The *configured* VRR mode as a plain string ("off"/"on"/"auto") -
    /// not necessarily what's actually active at the hardware level yet
    /// (see `TtyBackend`'s own doc comment on why `vrr "on"` isn't wired to
    /// real DRM VRR in this pass).
    pub vrr: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModeInfo {
    pub width: i32,
    pub height: i32,
    /// Refresh rate in Smithay's lossless millihertz representation rather
    /// than an approximate floating value.
    pub refresh_millihz: i32,
    pub preferred: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub ipc_protocol: u32,
}
