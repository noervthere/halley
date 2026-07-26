use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
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
    fn dmabuf_capabilities(&mut self) -> crate::backend::dmabuf::DmabufCapabilities;
    fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool;
    fn dmabuf_feedback(
        &self,
        output: &Output,
    ) -> Option<&crate::backend::dmabuf::SurfaceDmabufFeedback>;
    fn request_redraw(&mut self, output: Option<&Output>);
    fn with_renderer<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> T;
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
    pub fullscreen: crate::wayland::fullscreen::FullscreenManager,
    pub fullscreen_textures: crate::backend::fullscreen_texture::FullscreenTextureTransitions,
}

impl<D: SessionDriver> Session<D> {
    pub fn create_wayland_state(display_handle: DisplayHandle, driver: &mut D) -> WaylandState {
        let capabilities = driver.dmabuf_capabilities();
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = crate::wayland::dmabuf::create_global::<Self>(
            &mut dmabuf_state,
            &display_handle,
            &capabilities,
        );

        WaylandState::new(
            display_handle.clone(),
            CompositorState::new::<Self>(&display_handle),
            dmabuf_state,
            dmabuf_global,
            XdgShellState::new::<Self>(&display_handle),
            WlrLayerShellState::new::<Self>(&display_handle),
            XdgDecorationState::new::<Self>(&display_handle),
            ViewporterState::new::<Self>(&display_handle),
            FractionalScaleManagerState::new::<Self>(&display_handle),
            RelativePointerManagerState::new::<Self>(&display_handle),
            PointerConstraintsState::new::<Self>(&display_handle),
            KeyboardShortcutsInhibitState::new::<Self>(&display_handle),
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
        self.keyboard.reload(&config.keybinds, D::BACKEND_KIND);
        let redraw = self.decorations != config.decorations || self.zoom != config.zoom;
        self.decorations = config.decorations;
        self.zoom = config.zoom;
        self.window_open_animations.reload(config.animations);
        if self.fullscreen.reload(config.animations) {
            self.fullscreen_textures.clear();
        }
        if redraw {
            self.request_redraw();
        }
    }

    pub fn cleanup_fullscreen(&mut self, now: std::time::Duration) -> bool {
        let cleanup = self.fullscreen.cleanup(now);
        for surface in cleanup.finished_surfaces {
            self.fullscreen_textures.remove(&surface);
        }
        cleanup.visual_finished
    }
}
