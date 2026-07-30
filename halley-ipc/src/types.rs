use serde::{Deserialize, Serialize};

/// Bumped whenever `Request`/`Response` change shape. `postcard` encodes
/// enum variants positionally (by index, not by name), so unlike the tidy
/// config parsers elsewhere in this project, adding a variant anywhere but
/// the end of `Request`/`Response` silently breaks wire-compatibility with
/// a differently-versioned build - worth remembering as this grows, not
/// solved here (this first pass has nothing to negotiate against yet).
pub const HALLEY_IPC_VERSION: u32 = 8;

/// A request from `halleyctl`, the portal backend, or another local client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Outputs,
    Version,
    Screenshot(ScreenshotRequest),
    CancelScreenshot {
        request_handle: String,
    },
    ChooseSource(SourceChooserRequest),
    CancelSourceChooser {
        request_handle: String,
    },
    RegisterDmabuf(RegisterDmabufRequest),
    RemoveDmabuf {
        stream_handle: String,
        buffer_id: u64,
    },
    CaptureFrame(CaptureFrameRequest),
    Node(NodeRequest),
    Bearings(BearingsRequest),
    Quit,
    ConfigPath,
}

/// The compositor's reply to a `Request`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Outputs(OutputsResponse),
    Version(VersionInfo),
    Screenshot(ScreenshotResponse),
    Source(SourceChooserResponse),
    Frame(CaptureFrameResponse),
    Ack,
    Error(String),
    NodeList(NodeListResponse),
    NodeInfo(NodeInfo),
    BearingsStatus(BearingsStatusResponse),
    ConfigPath(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BearingsRequest {
    Show,
    Hide,
    Toggle,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearingsStatusResponse {
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeMoveDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeSelector {
    Focused,
    Latest,
    Id(u64),
    Title(String),
    App(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRequest {
    List {
        output: Option<String>,
    },
    Info {
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
    Focus {
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
    Move {
        direction: NodeMoveDirection,
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
    Close {
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
    Collapse {
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
    Restore {
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
    Toggle {
        selector: Option<NodeSelector>,
        output: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    Surface,
    Core,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeState {
    Active,
    Drifting,
    Node,
    Core,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    NormalToplevel,
    Dialog,
    Popup,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeProtocolFamily {
    XdgToplevel,
    XdgPopup,
    Xwayland,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRelationInfo {
    pub node_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: u64,
    pub title: String,
    pub app_id: Option<String>,
    pub output: Option<String>,
    pub kind: NodeKind,
    pub state: NodeState,
    pub visible: bool,
    pub focused: bool,
    pub latest: bool,
    pub pinned: bool,
    pub role: NodeRole,
    pub protocol_family: NodeProtocolFamily,
    pub modal: bool,
    pub parent: Option<NodeRelationInfo>,
    pub transient_for: Option<NodeRelationInfo>,
    pub child_popup_count: usize,
    pub pos_x: f32,
    pub pos_y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeOutputGroup {
    pub output: String,
    pub nodes: Vec<NodeInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeListResponse {
    pub outputs: Vec<NodeOutputGroup>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenshotTarget {
    Screen,
    Window,
    Area,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotRequest {
    pub request_handle: String,
    pub target: ScreenshotTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenshotResponse {
    Saved { path: String },
    Cancelled,
    Failed { message: String },
}

pub const SOURCE_MONITOR: u32 = 1;
pub const SOURCE_WINDOW: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChooserRequest {
    pub request_handle: String,
    pub source_types: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceChooserResponse {
    Selected(CaptureSource),
    Cancelled,
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureSource {
    Monitor {
        name: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Window {
        surface_id: u32,
        width: i32,
        height: i32,
    },
}

impl CaptureSource {
    pub fn width(&self) -> i32 {
        match self {
            Self::Monitor { width, .. } | Self::Window { width, .. } => *width,
        }
    }

    pub fn height(&self) -> i32 {
        match self {
            Self::Monitor { height, .. } | Self::Window { height, .. } => *height,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorMode {
    Hidden,
    Embedded,
    Metadata,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmabufPlane {
    pub fd_index: u32,
    pub plane_index: u32,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDmabufRequest {
    pub stream_handle: String,
    pub buffer_id: u64,
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub modifier: u64,
    pub flags: u32,
    pub planes: Vec<DmabufPlane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureBuffer {
    MemFd {
        fd_index: u32,
        offset: u64,
        size: u64,
        stride: u32,
    },
    Dmabuf {
        buffer_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureFrameRequest {
    pub stream_handle: String,
    pub source: CaptureSource,
    pub cursor_mode: CursorMode,
    pub buffer: CaptureBuffer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorMetadata {
    pub x: i32,
    pub y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureFrameResponse {
    pub cursor: Option<CursorMetadata>,
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
