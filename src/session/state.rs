use std::ffi::OsStr;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::XdgActivationState;

use crate::camera::OutputCameras;
use crate::cursor::CursorManager;
use crate::input::grab::{Grab, ResizeAnchor};
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::input::{Keyboard, SuppressedButtons, SuppressedKeys};
use crate::wayland::{ClientState, WaylandState};

/// The narrow contract shared compositor policy needs from a session driver.
/// Hardware setup, rendering, output reconfiguration, and event sources stay
/// inside the concrete driver modules.
pub trait SessionDriver: crate::ipc::OutputInfoSource + 'static {
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
    pub(super) launch_environment: super::environment::LaunchEnvironment,
    pub(super) autostart: super::autostart::Autostart,
    pub pointer: Pointer,
    pub cursor: CursorManager,
    pub(super) cursor_policy: super::cursor::Policy<D>,
    pub(super) publish_session_environment: bool,
    pub wayland: WaylandState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub start_time: std::time::Instant,
    pub nodes: crate::nodes::NodesState,
    pub input: halley_config::Input,
    pub decorations: halley_config::Decorations,
    pub cameras: OutputCameras,
    pub zoom: halley_config::Zoom,
    pub screenshot: halley_config::Screenshot,
    pub capture: crate::capture::CaptureState,
    pub screencast: crate::screencast::ScreencastState,
    pub grab: Grab,
    pub resize_anchor: Option<ResizeAnchor>,
    pub suppressed_buttons: SuppressedButtons,
    pub suppressed_keys: SuppressedKeys,
    pub wheel_accumulator: WheelAccumulator,
    pub(super) touch: super::touch::TouchState,
    pub(super) gestures: super::gesture::GestureState,
    pub pointer_constraints: super::pointer::PointerConstraintLifecycle,
    pub keyboard_monitor: Option<crate::accessibility::KeyboardMonitorService>,
    pub opening_origins: super::opening::OpeningOrigins,
    pub window_open_animations: crate::animation::WindowOpenAnimations,
    pub window_close_animations: crate::backend::close::WindowCloseAnimations,
    pub fullscreen: crate::wayland::fullscreen::FullscreenManager,
    pub fullscreen_textures: crate::backend::fullscreen_texture::FullscreenTextureTransitions,
    pub node_renderer: crate::backend::node::NodeRenderer,
    pub ui_text: crate::backend::text::UiTextRenderer,
    pub xwayland: crate::xwayland::State,
}

impl<D: SessionDriver> Session<D> {
    pub(crate) fn launch_environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.launch_environment.iter()
    }

    pub(crate) fn arm_autostart_once(&mut self, wayland_display: &OsStr, commands: Vec<String>) {
        self.autostart.arm_once(wayland_display, commands);
    }

    pub(crate) fn run_autostart_once(&mut self) {
        let x11_display = self.xwayland.display_name();
        self.autostart.run_once(
            x11_display.as_deref(),
            self.cursor.theme_name(),
            self.cursor.size(),
            &self.launch_environment,
        );
    }

    pub(crate) fn run_autostart_reload(&mut self, commands: &[String]) {
        let x11_display = self.xwayland.display_name();
        self.autostart.run_reload(
            commands,
            x11_display.as_deref(),
            self.cursor.theme_name(),
            self.cursor.size(),
            &self.launch_environment,
        );
    }

    pub(crate) fn reap_autostart(&mut self) {
        self.autostart.reap_finished();
    }

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
            XdgActivationState::new::<Self>(&display_handle),
            WlrLayerShellState::new::<Self>(&display_handle),
            XdgDecorationState::new::<Self>(&display_handle),
            ViewporterState::new::<Self>(&display_handle),
            FractionalScaleManagerState::new::<Self>(&display_handle),
            RelativePointerManagerState::new::<Self>(&display_handle),
            PointerConstraintsState::new::<Self>(&display_handle),
            PointerGesturesState::new::<Self>(&display_handle),
            CursorShapeManagerState::new::<Self>(&display_handle),
            VirtualKeyboardManagerState::new::<Self, _>(&display_handle, |client| {
                client.get_data::<ClientState>().is_some()
            }),
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
        let cancel_touch =
            self.input.gestures.touch_passthrough && !config.input.gestures.touch_passthrough;
        let cancel_gestures = self.input.gestures != config.input.gestures;
        if cancel_touch {
            super::touch::cancel_all(self);
        }
        if cancel_gestures {
            super::gesture::cancel_all(self);
        }
        self.launch_environment.reload(&config.env);
        let launch_path = self.launch_environment.path();
        self.keyboard
            .reload(&config.keybinds, D::BACKEND_KIND, launch_path.as_deref());
        crate::input::config::reload(self, &config.input);
        let cursor_changed = self.cursor.reload(&config.cursor);
        let cursor_visibility_changed = self.cursor_policy.reload(&config.cursor);
        if cursor_changed && self.publish_session_environment {
            super::environment::publish_cursor(&config.cursor);
        }
        let redraw = self.decorations != config.decorations
            || self.zoom != config.zoom
            || cursor_changed
            || cursor_visibility_changed;
        let nodes_redraw = self
            .nodes
            .reload(config, crate::frame_clock::monotonic_now());
        let font_redraw = self.ui_text.reload_font(&config.font);
        self.decorations = config.decorations;
        self.zoom = config.zoom;
        self.screenshot = config.screenshot.clone();
        self.window_open_animations.reload(config.animations);
        self.window_close_animations.reload(config.animations);
        if self.fullscreen.reload(config.animations) {
            self.fullscreen_textures.clear();
        }
        if nodes_redraw {
            crate::nodes::reconcile_landmarks(self, None);
        }
        if redraw || nodes_redraw || font_redraw {
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

    pub fn finish_x11_fullscreen_presentation(&mut self, surface: &WlSurface) -> bool {
        let root = crate::wayland::compositor::root_surface(surface);
        let window = self
            .wayland
            .space
            .elements()
            .find(|window| {
                window.x11_surface().is_some()
                    && window
                        .wl_surface()
                        .is_some_and(|candidate| candidate.as_ref() == &root)
            })
            .cloned();
        let Some(window) = window else {
            return false;
        };
        if !self
            .fullscreen
            .finish_external_presentation(&mut self.wayland, &window)
        {
            return false;
        }
        self.fullscreen_textures.remove(&root);
        true
    }
}
