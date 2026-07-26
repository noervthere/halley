use smithay::input::{Seat, SeatState};
use smithay::output::Output;

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
    fn primary_output(&self) -> &Output;
    fn request_redraw(&mut self, output: Option<&Output>);
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
    pub fn request_redraw(&mut self) {
        self.driver.request_redraw(None);
    }

    pub fn request_output_redraw(&mut self, output: &Output) {
        self.driver.request_redraw(Some(output));
    }
}
