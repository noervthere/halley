use std::borrow::Cow;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode as CalloopMode, PostAction};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::input::pointer::PointerHandle;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    add_pre_commit_hook, with_states,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::PointerConstraintsHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_keyboard_shortcuts_inhibit, delegate_layer_shell, delegate_output,
    delegate_pointer_constraints, delegate_primary_selection, delegate_relative_pointer,
    delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_decoration, delegate_xdg_shell,
    delegate_xdg_activation,
};

use super::state::{Session, SessionDriver};
use crate::wayland::{self, ClientState};

const XDG_ACTIVATION_TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

fn activation_token_is_fresh(created_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(created_at) < XDG_ACTIVATION_TOKEN_TIMEOUT
}

pub fn init_wayland_listener<D: SessionDriver>(
    display: Display<Session<D>>,
    event_loop: &mut EventLoop<Session<D>>,
) -> OsString {
    let listening_socket =
        ListeningSocketSource::new_auto().expect("failed to create wayland listening socket");
    let socket_name = listening_socket.socket_name().to_os_string();

    event_loop
        .handle()
        .insert_source(listening_socket, move |client_stream, _, session| {
            if let Err(err) = session
                .wayland
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                eventline::warn!("failed to insert new wayland client: {err}");
            }
        })
        .expect("failed to insert wayland listening socket source");

    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, session| {
                // Safety: the display is owned by this source for the loop's
                // lifetime and is never dropped while dispatch is active.
                unsafe {
                    display.get_mut().dispatch_clients(session)?;
                }
                super::sync_keyboard_focus(session, SERIAL_COUNTER.next_serial());
                Ok(PostAction::Continue)
            },
        )
        .expect("failed to insert wayland display dispatch source");

    socket_name
}

impl<D: SessionDriver> CompositorHandler for Session<D> {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.wayland.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(state) = client.get_data::<ClientState>() {
            &state.compositor_state
        } else {
            crate::xwayland::compositor_client_state(client)
                .expect("compositor client has no recognized client data")
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        wayland::compositor::prepare_commit::<Self>(surface);
        let root = wayland::compositor::root_surface(surface);
        let unmap = wayland::xdg_shell::will_unmap(&self.wayland, &root)
            .then(|| super::prepare_window_unmap(self, &root));
        let primary_output = self.driver.primary_output().clone();
        match wayland::compositor::commit(
            &mut self.wayland,
            &self.cameras,
            &primary_output,
            surface,
        ) {
            wayland::xdg_shell::ToplevelCommit::Mapped(mapped) => {
                super::closing::mapped(self, &mapped);
                if self.fullscreen.is_fullscreen_or_pending(&mapped) {
                    self.opening_origins.forget(&mapped);
                } else if let Some(output) = super::opening::output_for_surface(self, &mapped) {
                    super::opening::start(
                        self,
                        mapped,
                        &output,
                        crate::frame_clock::monotonic_now(),
                    );
                } else {
                    self.window_open_animations
                        .start(mapped, crate::frame_clock::monotonic_now());
                }
            }
            wayland::xdg_shell::ToplevelCommit::Unmapped(unmapped) => {
                let preparation =
                    unmap.expect("mapped toplevel unmap must have been prepared before commit");
                debug_assert_eq!(preparation.surface(), &unmapped);
                super::finish_window_unmap(self, preparation);
                super::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
            }
            wayland::xdg_shell::ToplevelCommit::None => {}
        }
        crate::xwayland::handle_commit(self, &root);
        self.fullscreen.handle_commit(
            &mut self.wayland,
            &self.cameras,
            &root,
            crate::frame_clock::monotonic_now(),
        );
        crate::input::grab::finish_resize_commit(&mut self.resize_anchor, &mut self.wayland.space);
        super::pointer::reconcile_state(self);
        self.request_redraw();
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        let root = wayland::compositor::root_surface(surface);
        super::closing::capture_surface(self, &root);
    }
}

impl<D: SessionDriver> BufferHandler for Session<D> {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl<D: SessionDriver> DmabufHandler for Session<D> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.wayland.dmabuf_state
    }

    fn dmabuf_imported(&mut self, global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.wayland.dmabuf_global.as_ref() != Some(global)
            || !self.driver.import_dmabuf(&dmabuf)
        {
            notifier.failed();
            return;
        }

        let _ = notifier.successful::<Self>();
    }
}

impl<D: SessionDriver> ShmHandler for Session<D> {
    fn shm_state(&self) -> &ShmState {
        &self.wayland.shm_state
    }
}

impl<D: SessionDriver> XdgShellHandler for Session<D> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.wayland.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface().clone();
        if let Some(output) = wayland::focus::selected_output(&self.wayland).cloned() {
            super::opening::prepare(self, wl_surface.clone(), &output);
        }
        wayland::xdg_shell::new_toplevel(&mut self.wayland, surface);
        add_pre_commit_hook::<Self, _>(&wl_surface, |session, _display, surface| {
            let removes_buffer = with_states(surface, |states| {
                matches!(
                    states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .pending()
                        .buffer,
                    Some(BufferAssignment::Removed)
                )
            });
            if removes_buffer {
                super::closing::capture_surface(session, surface);
            }

            let commit_serial = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok()?.last_acked.as_ref().map(|ack| ack.serial))
            });
            let Some(commit_serial) = commit_serial else {
                return;
            };
            if !session
                .fullscreen
                .should_capture_snapshot(surface, commit_serial)
            {
                return;
            }
            let window = session
                .wayland
                .space
                .elements()
                .find(|window| {
                    window
                        .toplevel()
                        .is_some_and(|toplevel| toplevel.wl_surface() == surface)
                })
                .cloned();
            let Some(window) = window else {
                return;
            };
            let textures = &mut session.fullscreen_textures;
            let capture = session
                .driver
                .with_renderer(|renderer| textures.capture_previous(renderer, &window));
            if let Err(err) = capture {
                eventline::warn!("fullscreen: failed to capture previous window texture: {err}");
            }
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        super::closing::capture_surface(self, surface.wl_surface());
        let preparation = super::prepare_window_unmap(self, surface.wl_surface());
        wayland::xdg_shell::toplevel_destroyed(&mut self.wayland, &surface);
        super::finish_window_unmap(self, preparation);
        super::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        super::cancel_grab_for_surface(self, surface.wl_surface());
        self.fullscreen.request(&mut self.wayland, &surface, output);
        super::pointer::reconcile_state(self);
        self.request_redraw();
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.fullscreen.unrequest(&self.wayland, &surface);
        super::pointer::reconcile_state(self);
        self.request_redraw();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        wayland::popup::track(&mut self.wayland, &self.cameras, surface);
        self.request_redraw();
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        wayland::popup::reposition(&self.wayland, &self.cameras, surface, positioner, token);
        self.request_redraw();
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let seat = Seat::<Self>::from_resource(&seat).expect("popup grab used an unknown wl_seat");
        let grab =
            wayland::popup::begin_grab(&mut self.wayland.popup_manager, &seat, surface, serial);
        if let Some(grab) = grab {
            wayland::popup::install_grab(self, &seat, grab, serial);
        }
    }
}

impl<D: SessionDriver> XdgActivationHandler for Session<D> {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.wayland.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        self.wayland
            .xdg_activation_state
            .retain_tokens(|_, token| activation_token_is_fresh(token.timestamp, Instant::now()));
        if let Some(surface) = data.surface.as_ref()
            && let Some(origin) = super::opening::surface_visual_center(self, surface)
        {
            data.user_data
                .insert_if_missing_threadsafe(|| super::opening::ActivationOrigin(origin));
        }

        let Some((serial, seat_resource)) = data.serial else {
            data.user_data
                .insert_if_missing_threadsafe(|| super::opening::OriginOnlyActivation);
            return true;
        };
        let Some(seat) = Seat::<Self>::from_resource(&seat_resource) else {
            return false;
        };
        let keyboard_valid = seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.last_enter())
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));
        let pointer_valid = seat
            .get_pointer()
            .and_then(|pointer| pointer.last_enter())
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));
        keyboard_valid || pointer_valid
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if activation_token_is_fresh(token_data.timestamp, Instant::now()) {
            let root = wayland::compositor::root_surface(&surface);
            let mapped = self
                .wayland
                .space
                .elements()
                .find(|window| {
                    window
                        .wl_surface()
                        .is_some_and(|candidate| candidate.as_ref() == &root)
                })
                .cloned();
            if let Some(window) = mapped {
                if token_data
                    .user_data
                    .get::<super::opening::OriginOnlyActivation>()
                    .is_none()
                {
                    super::focus_window(self, &window, SERIAL_COUNTER.next_serial());
                    self.request_redraw();
                }
            } else if let Some(origin) = token_data
                .user_data
                .get::<super::opening::ActivationOrigin>()
            {
                self.opening_origins.remember_launcher(root, origin.0);
            }
        }
        self.wayland.xdg_activation_state.remove_token(&token);
    }
}

impl<D: SessionDriver> WlrLayerShellHandler for Session<D> {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.wayland.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let requested = output.as_ref().and_then(Output::from_resource);
        let output = wayland::focus::output_for_new_surface(
            &self.wayland,
            requested,
            self.driver.primary_output(),
        );
        wayland::layer_shell::new_surface(&mut self.wayland, surface, Some(output), namespace);
        self.request_redraw();
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        wayland::layer_shell::destroyed(&mut self.wayland, &surface);
        self.request_redraw();
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        wayland::popup::unconstrain_surface(&self.wayland, &self.cameras, popup);
        self.request_redraw();
    }
}

impl<D: SessionDriver> XdgDecorationHandler for Session<D> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        wayland::decoration::new_decoration(toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        wayland::decoration::request_mode(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        wayland::decoration::unset_mode(toplevel);
    }
}

impl<D: SessionDriver> SeatHandler for Session<D> {
    type KeyboardFocus = crate::xwayland::KeyboardFocusTarget;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(
        &mut self,
        seat: &Seat<Self>,
        focused: Option<&crate::xwayland::KeyboardFocusTarget>,
    ) {
        let display_handle = self.wayland.display_handle.clone();
        let focused = focused.and_then(|target| target.wl_surface().map(Cow::into_owned));
        wayland::selection::sync_selection_focus(&display_handle, seat, focused.as_ref());
    }
}

impl<D: SessionDriver> OutputHandler for Session<D> {}

impl<D: SessionDriver> FractionalScaleHandler for Session<D> {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self
            .driver
            .primary_output()
            .current_scale()
            .fractional_scale();
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(scale);
            });
        });
    }
}

impl<D: SessionDriver> PointerConstraintsHandler for Session<D> {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Confined pointers consume absolute presentation geometry and must
        // settle it first. Locked pointers remain relative-only, so their
        // exact owner can stay active while the compositor presentation moves.
        if super::pointer::new_constraint_requires_stable_presentation(surface, pointer)
            && self.finish_x11_fullscreen_presentation(surface)
        {
            self.request_redraw();
        }
        super::pointer::activate_new(self, surface, pointer);
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        super::pointer::apply_position_hint(self, surface, pointer, location);
    }
}

impl<D: SessionDriver> KeyboardShortcutsInhibitHandler for Session<D> {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.wayland.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }
}

impl<D: SessionDriver> SelectionHandler for Session<D> {
    type SelectionUserData = ();
}

impl<D: SessionDriver> DataDeviceHandler for Session<D> {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.wayland.data_device_state
    }
}

impl<D: SessionDriver> WaylandDndGrabHandler for Session<D> {
    fn dnd_requested<S: smithay::input::dnd::Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: smithay::input::dnd::GrabType,
    ) {
        let display_handle = self.wayland.display_handle.clone();
        wayland::selection::start_dnd_grab(self, &display_handle, source, seat, serial, type_);
    }
}

impl<D: SessionDriver> smithay::input::dnd::DndGrabHandler for Session<D> {}

impl<D: SessionDriver> PrimarySelectionHandler for Session<D> {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.wayland.primary_selection_state
    }
}

delegate_compositor!(@<D: SessionDriver> Session<D>);
delegate_dmabuf!(@<D: SessionDriver> Session<D>);
delegate_shm!(@<D: SessionDriver> Session<D>);
delegate_xdg_shell!(@<D: SessionDriver> Session<D>);
delegate_xdg_activation!(@<D: SessionDriver> Session<D>);
delegate_layer_shell!(@<D: SessionDriver> Session<D>);
delegate_xdg_decoration!(@<D: SessionDriver> Session<D>);
delegate_seat!(@<D: SessionDriver> Session<D>);
delegate_output!(@<D: SessionDriver> Session<D>);
delegate_viewporter!(@<D: SessionDriver> Session<D>);
delegate_fractional_scale!(@<D: SessionDriver> Session<D>);
delegate_relative_pointer!(@<D: SessionDriver> Session<D>);
delegate_pointer_constraints!(@<D: SessionDriver> Session<D>);
delegate_keyboard_shortcuts_inhibit!(@<D: SessionDriver> Session<D>);
delegate_data_device!(@<D: SessionDriver> Session<D>);
delegate_primary_selection!(@<D: SessionDriver> Session<D>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_tokens_expire_at_ten_seconds() {
        let created = Instant::now();

        assert!(activation_token_is_fresh(
            created,
            created + Duration::from_secs(9)
        ));
        assert!(!activation_token_is_fresh(
            created,
            created + XDG_ACTIVATION_TOKEN_TIMEOUT
        ));
    }
}
