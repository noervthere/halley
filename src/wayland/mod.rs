pub mod compositor;
pub mod xdg_shell;

use std::collections::HashMap;

use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

/// Wayland protocol state shared by every top-level app struct (`App`,
/// `TtyApp`) - narrow on purpose: only what advertising the core globals and
/// tracking windows actually needs, nothing about input focus or backend
/// rendering. Old halley put all of this directly on its `Halley` god
/// object; here it's one field (`wayland: WaylandState`) each app struct
/// owns, matching how `cursor`/`keyboard`/`pointer` were each added for one
/// concrete reason.
pub struct WaylandState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
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
}

impl WaylandState {
    pub fn new(
        display_handle: DisplayHandle,
        compositor_state: CompositorState,
        xdg_shell_state: XdgShellState,
        shm_state: ShmState,
        output_manager_state: OutputManagerState,
    ) -> Self {
        Self {
            display_handle,
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            space: Space::default(),
            unmapped: HashMap::new(),
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
