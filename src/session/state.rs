use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::viewporter::ViewporterState;

use crate::camera::OutputCameras;
use crate::cursor::CursorImage;
use crate::input::grab::{Grab, ResizeAnchor};
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::input::{Keyboard, SuppressedButtons};
use crate::wayland::WaylandState;

/// The narrow contract shared compositor policy needs from a session driver.
/// Hardware setup, rendering, output reconfiguration, and event sources stay
/// inside the concrete driver modules.
pub trait SessionDriver: 'static {
    const BACKEND_KIND: crate::input::keybinds::BackendKind;

    fn primary_output(&self) -> &Output;
    fn request_redraw(&mut self, output: Option<&Output>);
    fn stop(&mut self);
}

/// Backend-independent compositor state.
///
/// `D` owns only backend mechanics. Wayland policy, input state, cameras, and
/// runtime visual state live here once so nested and real-hardware sessions
/// cannot evolve different behavior.
pub struct Session<D: SessionDriver> {
    pub driver: D,
    pub keyboard: Keyboard,
    pub pointer: Pointer,
    pub cursor: CursorImage,
    pub wayland: WaylandState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub start_time: std::time::Instant,
    pub decorations: halley_config::Decorations,
    pub cameras: OutputCameras,
    pub zoom: halley_config::Zoom,
    pub grab: Grab,
    pub resize_anchor: Option<ResizeAnchor>,
    pub suppressed_buttons: SuppressedButtons,
    pub wheel_accumulator: WheelAccumulator,
    pub window_open_animations: crate::animation::WindowOpenAnimations,
}

impl<D: SessionDriver> Session<D> {
    pub fn create_wayland_state(display_handle: DisplayHandle) -> WaylandState {
        WaylandState::new(
            display_handle.clone(),
            CompositorState::new::<Self>(&display_handle),
            XdgShellState::new::<Self>(&display_handle),
            WlrLayerShellState::new::<Self>(&display_handle),
            XdgDecorationState::new::<Self>(&display_handle),
            ViewporterState::new::<Self>(&display_handle),
            FractionalScaleManagerState::new::<Self>(&display_handle),
            ShmState::new::<Self>(&display_handle, vec![]),
            OutputManagerState::new_with_xdg_output::<Self>(&display_handle),
            DataDeviceState::new::<Self>(&display_handle),
            PrimarySelectionState::new::<Self>(&display_handle),
        )
    }

    pub fn request_redraw(&mut self) {
        self.driver.request_redraw(None);
    }

    pub fn request_output_redraw(&mut self, output: &Output) {
        self.driver.request_redraw(Some(output));
    }

    /// Applies every backend-independent setting from one validated config
    /// snapshot. Output hardware policy remains with the concrete driver.
    pub fn apply_common_config(&mut self, config: &halley_config::RuntimeConfig) {
        self.keyboard
            .reload(&config.keybinds, D::BACKEND_KIND);
        let redraw =
            self.decorations != config.decorations || self.zoom != config.zoom;
        self.decorations = config.decorations;
        self.zoom = config.zoom;
        self.window_open_animations.reload(config.animations);
        if redraw {
            self.request_redraw();
        }
    }
}
