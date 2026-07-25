pub mod compositor;
pub mod decoration;
pub mod focus;
pub mod layer_shell;
pub mod popup;
pub mod selection;
pub mod xdg_shell;

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use smithay::desktop::{LayerSurface, PopupManager, Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shm::ShmState;

/// The one output responsible for painting a window. Smithay's `Space`
/// still owns output geometry and pointer routing; this is only Halley's
/// whole-window handoff policy, matching the original compositor.
struct WindowOutput(RwLock<String>);

pub fn set_window_output(window: &Window, output: &Output) {
    let owner = window
        .user_data()
        .get_or_insert_threadsafe(|| WindowOutput(RwLock::new(output.name())));
    *owner.0.write().expect("window output lock poisoned") = output.name();
}

pub fn window_is_on_output(window: &Window, output: &Output, primary: &Output) -> bool {
    window
        .user_data()
        .get::<WindowOutput>()
        .map(|owner| {
            owner
                .0
                .read()
                .expect("window output lock poisoned")
                .as_str()
                == output.name()
        })
        // Windows are assigned during their initial map. Retaining this
        // fallback keeps an already-mapped window visible if that invariant
        // is ever broken while developing.
        .unwrap_or_else(|| output == primary)
}

/// Wayland protocol and shell-model state shared by both session drivers.
///
/// Backend rendering, cameras, input devices, and redraw scheduling stay out
/// of this type. That keeps the Smithay globals and shell lifecycle together
/// without recreating old Halley's compositor-wide god object.
pub struct WaylandState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    /// Clipboard (ctrl+c/ctrl+v) and drag-and-drop. Smithay owns the actual
    /// transfer - it hands the source client's fd straight to the receiving
    /// client - so this is only the global's state, with no compositor-side
    /// buffering of clipboard contents.
    pub data_device_state: DataDeviceState,
    /// Middle-click paste. A separate protocol from `data_device_state`, not
    /// a mode of it: clients set the two selections independently and
    /// expect them to hold different contents.
    pub primary_selection_state: PrimarySelectionState,
    /// Tracks popup trees once for both xdg-toplevel and layer-shell roots.
    /// Rendering and input can then ask Smithay for the same canonical tree
    /// instead of each subsystem inventing its own parent/offset bookkeeping.
    pub popup_manager: PopupManager,
    /// Real, visible windows - a surface only enters `space` once it has
    /// actually attached a buffer (see `unmapped`), not merely once its
    /// toplevel role exists.
    pub space: Space<Window>,
    /// Toplevels that exist but haven't mapped a buffer yet, keyed by their
    /// `wl_surface`. A narrower version of niri's `Unmapped`/`Mapped` split -
    /// just enough to defer entering `space` until there's something real
    /// to show, without niri's window-rules/credentials/placement-policy
    /// weight this milestone doesn't need.
    pub unmapped: HashMap<WlSurface, Window>,
    /// Layer surfaces that have not attached a buffer since creation (or
    /// since a null-buffer unmap). The Smithay `LayerMap` retains the role so
    /// it can calculate and send the next configure; this set records the
    /// separate Wayland mapped/unmapped lifecycle.
    pub unmapped_layers: HashSet<WlSurface>,
    /// The one on-demand layer surface selected by a click. Exclusive layer
    /// focus is derived from mapped protocol state and is never stored here.
    pub focused_layer: Option<LayerSurface>,
    /// Persistent normal-window focus. Layer-shell focus is resolved
    /// separately so a temporary launcher or exclusive overlay does not
    /// erase the window that should regain focus after it closes.
    pub focused_window: Option<WlSurface>,
}

impl WaylandState {
    // One parameter per protocol global, each constructed by the caller
    // because `State::new::<D>` needs the concrete app type (`App` vs
    // `TtyApp`) that only the session driver knows. Grouping them into a
    // struct would just move the same list somewhere else; this grows by
    // exactly one line per protocol added, which is the point.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        display_handle: DisplayHandle,
        compositor_state: CompositorState,
        xdg_shell_state: XdgShellState,
        layer_shell_state: WlrLayerShellState,
        xdg_decoration_state: XdgDecorationState,
        shm_state: ShmState,
        output_manager_state: OutputManagerState,
        data_device_state: DataDeviceState,
        primary_selection_state: PrimarySelectionState,
    ) -> Self {
        Self {
            display_handle,
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            popup_manager: PopupManager::default(),
            space: Space::default(),
            unmapped: HashMap::new(),
            unmapped_layers: HashSet::new(),
            focused_layer: None,
            focused_window: None,
        }
    }
}

/// Per-client Wayland state - one instance per connected client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
