pub mod compositor;
pub mod decoration;
pub mod dmabuf;
pub mod focus;
pub mod fullscreen;
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
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::XdgActivationState;

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

pub fn window_output_name(window: &Window) -> Option<String> {
    window
        .user_data()
        .get::<WindowOutput>()
        .map(|owner| owner.0.read().expect("window output lock poisoned").clone())
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
/// without creating a compositor-wide god object.
pub struct WaylandState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub xdg_shell_state: XdgShellState,
    pub xdg_activation_state: XdgActivationState,
    pub layer_shell_state: WlrLayerShellState,
    // Retained for the lifetime of its advertised global.
    _xdg_decoration_state: XdgDecorationState,
    _viewporter_state: ViewporterState,
    _fractional_scale_manager_state: FractionalScaleManagerState,
    _relative_pointer_manager_state: RelativePointerManagerState,
    _pointer_constraints_state: PointerConstraintsState,
    _pointer_gestures_state: PointerGesturesState,
    _cursor_shape_manager_state: CursorShapeManagerState,
    _virtual_keyboard_manager_state: VirtualKeyboardManagerState,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub shm_state: ShmState,
    // Retained alongside the wl_output globals it serves.
    _output_manager_state: OutputManagerState,
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
    /// Managed-window stacking independent of render-space implementation
    /// details. Focus succession and future window policy read this order.
    pub managed_windows: crate::window::ManagedWindowStack,
    /// Toplevels that exist but haven't mapped a buffer yet, keyed by their
    /// `wl_surface`. This defers entering `space` until there is a real
    /// buffer to show, without mixing placement policy into surface
    /// lifecycle tracking.
    pub unmapped: HashMap<WlSurface, Window>,
    /// Last mapped position retained across a null-buffer unmap. Initial maps
    /// have no entry and use normal placement policy.
    pub unmapped_locations: HashMap<WlSurface, Point<i32, Logical>>,
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
    /// The output selected by the most recent focus click. New toplevels
    /// use it as their spawn output; storing only the stable output name
    /// keeps output handles and camera state in their existing owners.
    pub focused_output: Option<String>,
}

impl WaylandState {
    // Protocol construction is centralized by the generic session. This
    // constructor only assembles the resulting globals with shell state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        display_handle: DisplayHandle,
        compositor_state: CompositorState,
        dmabuf_state: DmabufState,
        dmabuf_global: Option<DmabufGlobal>,
        xdg_shell_state: XdgShellState,
        xdg_activation_state: XdgActivationState,
        layer_shell_state: WlrLayerShellState,
        xdg_decoration_state: XdgDecorationState,
        viewporter_state: ViewporterState,
        fractional_scale_manager_state: FractionalScaleManagerState,
        relative_pointer_manager_state: RelativePointerManagerState,
        pointer_constraints_state: PointerConstraintsState,
        pointer_gestures_state: PointerGesturesState,
        cursor_shape_manager_state: CursorShapeManagerState,
        virtual_keyboard_manager_state: VirtualKeyboardManagerState,
        keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
        shm_state: ShmState,
        output_manager_state: OutputManagerState,
        data_device_state: DataDeviceState,
        primary_selection_state: PrimarySelectionState,
    ) -> Self {
        Self {
            display_handle,
            compositor_state,
            dmabuf_state,
            dmabuf_global,
            xdg_shell_state,
            xdg_activation_state,
            layer_shell_state,
            _xdg_decoration_state: xdg_decoration_state,
            _viewporter_state: viewporter_state,
            _fractional_scale_manager_state: fractional_scale_manager_state,
            _relative_pointer_manager_state: relative_pointer_manager_state,
            _pointer_constraints_state: pointer_constraints_state,
            _pointer_gestures_state: pointer_gestures_state,
            _cursor_shape_manager_state: cursor_shape_manager_state,
            _virtual_keyboard_manager_state: virtual_keyboard_manager_state,
            keyboard_shortcuts_inhibit_state,
            shm_state,
            _output_manager_state: output_manager_state,
            data_device_state,
            primary_selection_state,
            popup_manager: PopupManager::default(),
            space: Space::default(),
            managed_windows: crate::window::ManagedWindowStack::default(),
            unmapped: HashMap::new(),
            unmapped_locations: HashMap::new(),
            unmapped_layers: HashSet::new(),
            focused_layer: None,
            focused_window: None,
            focused_output: None,
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

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        match reason {
            DisconnectReason::ConnectionClosed => {
                eventline::debug!("wayland client {client_id:?} disconnected");
            }
            DisconnectReason::ProtocolError(error) => {
                eventline::warn!(
                    "wayland client {client_id:?} disconnected after protocol error: {error:?}"
                );
            }
        }
    }
}
