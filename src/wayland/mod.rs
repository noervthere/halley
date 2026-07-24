pub mod compositor;
pub mod decoration;
pub mod selection;
pub mod xdg_shell;

use std::collections::HashMap;

use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;

/// Wayland protocol state shared by every top-level app struct (`App`,
/// `TtyApp`) - narrow on purpose: only what advertising the core globals,
/// tracking windows, and which one is focused actually needs, nothing about
/// backend rendering. Old halley put all of this directly on its `Halley`
/// god object; here it's one field (`wayland: WaylandState`) each app struct
/// owns, matching how `cursor`/`keyboard`/`pointer` were each added for one
/// concrete reason.
pub struct WaylandState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
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
    /// The surface that should have keyboard focus - `None` if nothing is
    /// mapped. Set by `xdg_shell::handle_commit` (new windows steal focus on
    /// map) and cleared by `xdg_shell::toplevel_destroyed` if the destroyed
    /// window was the focused one. Driving code re-asserts this onto the
    /// real seat every commit (see `session::tty`/`session::winit`); this
    /// field is just the source of truth, not the actual Wayland-facing
    /// focus state.
    pub focused: Option<WlSurface>,
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
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            space: Space::default(),
            unmapped: HashMap::new(),
            focused: None,
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
