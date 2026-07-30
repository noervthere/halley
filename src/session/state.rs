use std::ffi::OsStr;
use std::path::PathBuf;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::background_effect::BackgroundEffectState;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::DmabufState;
use smithay::wayland::drm_syncobj::{DrmSyncPointSource, DrmSyncobjState};
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::ext_data_control::DataControlState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::XdgActivationState;

use crate::cursor::CursorManager;
use crate::input::grab::{Grab, ResizeAnchor};
use crate::input::pointer::{Pointer, WheelAccumulator};
use crate::input::{Keyboard, SuppressedButtons, SuppressedKeys};
use crate::presentation::camera::OutputCameras;
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
    fn register_drm_syncobj_source(
        &mut self,
        _client: Client,
        _source: DrmSyncPointSource,
    ) -> bool {
        false
    }
    fn apply_dpms(
        &mut self,
        _command: halley_ipc::DpmsCommand,
        _output: Option<&str>,
    ) -> Result<(), String> {
        Err("dpms is only supported on the tty backend".to_string())
    }
    fn output_requires_lock_frame(&self, _output: &Output) -> bool {
        true
    }
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
    pub(crate) cursor_policy: super::cursor::Policy<D>,
    pub(super) publish_session_environment: bool,
    pub wayland: WaylandState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    pub idle_notifier_state: IdleNotifierState<Self>,
    pub presentation_state: PresentationState,
    pub drm_syncobj_state: Option<DrmSyncobjState>,
    pub session_lock: crate::wayland::session_lock::State,
    pub start_time: std::time::Instant,
    pub config_path: Option<PathBuf>,
    pub startup_config_diagnostic: Option<halley_config::ConfigDiagnostic>,
    pub overlays: crate::shell::overlay::OverlayManager,
    pub overlay_config: halley_config::Overlays,
    pub nodes: crate::nodes::NodesState,
    pub bearings: crate::shell::bearings::BearingsState,
    pub focus_cycle: crate::shell::focus_cycle::FocusCycleState,
    pub pending_pointer_warp: Option<WlSurface>,
    pub apogee: crate::shell::apogee::ApogeeState,
    pub apogee_config: halley_config::Apogee,
    pub input: halley_config::Input,
    pub decorations: halley_config::Decorations,
    pub effects: halley_config::Effects,
    pub cameras: OutputCameras,
    pub field_config: halley_config::Field,
    pub zoom: halley_config::Zoom,
    pub screenshot: halley_config::Screenshot,
    pub capture: crate::capture::CaptureState,
    pub screencast: crate::capture::screencast::ScreencastState,
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
    pub render: crate::render::resources::RenderState,
    pub fullscreen: crate::wayland::fullscreen::FullscreenManager,
    pub maximize: crate::presentation::maximize::FieldMaximizeManager,
    pub xwayland: crate::xwayland::State<D>,
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
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        let ext_data_control_state = DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );

        WaylandState::new(
            display_handle.clone(),
            CompositorState::new::<Self>(&display_handle),
            dmabuf_state,
            dmabuf_global,
            XdgShellState::new::<Self>(&display_handle),
            XdgActivationState::new::<Self>(&display_handle),
            WlrLayerShellState::new::<Self>(&display_handle),
            BackgroundEffectState::new::<Self>(&display_handle),
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
            crate::wayland::wlr_output_management::State::new::<Self>(&display_handle),
            DataDeviceState::new::<Self>(&display_handle),
            primary_selection_state,
            ext_data_control_state,
        )
    }

    pub fn request_redraw(&mut self) {
        self.driver.request_redraw(None);
    }

    pub fn request_output_redraw(&mut self, output: &Output) {
        self.driver.request_redraw(Some(output));
    }

    pub fn notification_output_name(&self) -> String {
        crate::wayland::focus::selected_output(&self.wayland)
            .unwrap_or_else(|| self.driver.primary_output())
            .name()
    }

    pub fn initialize_config_notification(&mut self) {
        let now = crate::frame_clock::monotonic_now();
        let output = self.notification_output_name();
        if self.startup_config_diagnostic.take().is_some() {
            self.overlays.show_config_error(
                output,
                self.overlay_config.notifications.error_duration_ms,
                now,
            );
        } else if let Some(path) = self.config_path.as_deref() {
            self.overlays.show_config_success(
                output,
                path,
                self.overlay_config.notifications.success_duration_ms,
                now,
            );
        }
        self.request_redraw();
    }

    pub fn show_config_reload_error(&mut self) {
        let output = self.notification_output_name();
        self.overlays.show_config_error(
            output,
            self.overlay_config.notifications.error_duration_ms,
            crate::frame_clock::monotonic_now(),
        );
        self.request_redraw();
    }

    pub fn clear_config_reload_error(&mut self) {
        if self
            .overlays
            .clear_config_error(crate::frame_clock::monotonic_now())
        {
            self.request_redraw();
        }
    }

    pub fn show_exit_confirmation(&mut self) {
        if !self.overlays.show_exit(crate::frame_clock::monotonic_now()) {
            return;
        }
        self.grab = Grab::None;
        self.cursor.set_override(None);
        super::gesture::cancel_all(self);
        super::touch::cancel_all(self);
        self.request_redraw();
    }

    pub fn cancel_exit_confirmation(&mut self) {
        if self
            .overlays
            .cancel_exit(crate::frame_clock::monotonic_now())
        {
            self.request_redraw();
        }
    }

    pub fn confirm_exit(&mut self) {
        if self.overlays.exit_modal_active() {
            self.driver.stop();
        }
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
            || self.effects != config.effects
            || self.zoom != config.field.zoom
            || self.overlay_config != config.overlays
            || cursor_changed
            || cursor_visibility_changed;
        let nodes_redraw = self
            .nodes
            .reload(config, crate::frame_clock::monotonic_now());
        let bearings_redraw = self.bearings.reload(config.bearings);
        let font_redraw = self.render.ui_text.reload_font(&config.font);
        self.apogee_config = config.apogee;
        self.decorations = config.decorations;
        self.effects = config.effects;
        self.field_config = config.field;
        self.overlay_config = config.overlays;
        self.zoom = config.field.zoom;
        self.screenshot = config.screenshot.clone();
        self.window_open_animations.reload(config.animations);
        self.render
            .window_close_animations
            .reload(config.animations);
        let fullscreen_redraw = self.fullscreen.reload(config.animations);
        if fullscreen_redraw {
            self.render.fullscreen_textures.remove_owner(
                crate::render::fullscreen_texture::TextureTransitionOwner::Fullscreen,
            );
        }
        let maximize_redraw = self.maximize.reload(config.field, config.animations);
        if maximize_redraw {
            self.render
                .fullscreen_textures
                .remove_owner(crate::render::fullscreen_texture::TextureTransitionOwner::Maximize);
        }
        if nodes_redraw {
            crate::nodes::reconcile_landmarks(self, None);
        }
        if redraw
            || nodes_redraw
            || bearings_redraw
            || font_redraw
            || fullscreen_redraw
            || maximize_redraw
        {
            self.request_redraw();
        }
    }

    pub fn cleanup_fullscreen(&mut self, now: std::time::Duration) -> bool {
        let cleanup = self.fullscreen.cleanup(now);
        let maximize_cleanup = self.maximize.cleanup(now);
        for surface in cleanup.finished_surfaces {
            self.render.fullscreen_textures.remove(&surface);
        }
        for surface in maximize_cleanup.finished_surfaces {
            self.render.fullscreen_textures.remove(&surface);
        }
        let outputs = self.wayland.space.outputs().cloned().collect::<Vec<_>>();
        for output in outputs {
            self.sync_fullscreen_camera(&output, now);
        }
        cleanup.visual_finished || maximize_cleanup.visual_finished
    }

    pub fn sync_fullscreen_camera(
        &mut self,
        output: &smithay::output::Output,
        now: std::time::Duration,
    ) -> bool {
        let frame = self
            .wayland
            .space
            .output_geometry(output)
            .and_then(|geometry| self.fullscreen.camera_frame(output, geometry, now));
        if let Some(frame) = frame {
            let field_changed = self.cameras.apply_field_maximize(&output.name(), None);
            field_changed | self.cameras.apply_fullscreen(&output.name(), Some(frame))
        } else {
            let fullscreen_changed = self.cameras.apply_fullscreen(&output.name(), None);
            let maximize_progress = self.maximize.camera_progress(output, now);
            fullscreen_changed
                | self
                    .cameras
                    .apply_field_maximize(&output.name(), maximize_progress)
        }
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
        true
    }
}
