use std::borrow::Cow;
use std::cell::RefCell;
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
use smithay::reexports::wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, Resource};
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    add_pre_commit_hook, with_states,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::drm_syncobj::{
    DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState,
};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::PointerConstraintsHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::ext_data_control::{
    DataControlHandler as ExtDataControlHandler, DataControlState as ExtDataControlState,
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
    delegate_background_effect, delegate_compositor, delegate_cursor_shape, delegate_data_device,
    delegate_dmabuf, delegate_drm_syncobj, delegate_ext_data_control,
    delegate_fractional_scale,
    delegate_idle_inhibit,
    delegate_idle_notify,
    delegate_keyboard_shortcuts_inhibit, delegate_layer_shell, delegate_output,
    delegate_pointer_constraints, delegate_primary_selection, delegate_relative_pointer,
    delegate_pointer_gestures, delegate_presentation, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_virtual_keyboard_manager,
    delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
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
                session.refresh_idle_inhibit();
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

    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, |session, _dh, surface| {
            if session.drm_syncobj_state.is_none() {
                return;
            }
            let acquire_point = with_states(surface, |states| {
                let opted_in = states
                    .data_map
                    .get::<RefCell<Option<WpLinuxDrmSyncobjSurfaceV1>>>()
                    .is_some_and(|surface| surface.borrow().is_some());
                if !opted_in {
                    return None;
                }
                states
                    .cached_state
                    .get::<DrmSyncobjCachedState>()
                    .pending()
                    .acquire_point
                    .clone()
            });
            let Some(acquire_point) = acquire_point else {
                return;
            };
            let Some(client) = surface.client() else {
                return;
            };
            match acquire_point.generate_blocker() {
                Ok((blocker, source)) => {
                    let registered = session.driver.register_drm_syncobj_source(client, source);
                    smithay::wayland::compositor::add_blocker(surface, blocker);
                    if !registered {
                        eventline::warn!(
                            "explicit sync: acquire-point source was not registered; keeping the affected surface commit blocked"
                        );
                    }
                }
                Err(err) => {
                    eventline::warn!(
                        "explicit sync: failed to create acquire-point blocker: {err}"
                    );
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        wayland::compositor::prepare_commit::<Self>(surface);
        let root = wayland::compositor::root_surface(surface);
        let rule_window = self
            .wayland
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == &root)
            })
            .or_else(|| self.wayland.unmapped.get(&root))
            .or_else(|| self.wayland.collapsed.get(&root))
            .cloned();
        let rule = rule_window
            .as_ref()
            .map(|window| self.window_rules.track_window(window))
            .unwrap_or_default();
        if let Some(size) = rule.initial_size
            && let Some(toplevel) = rule_window.as_ref().and_then(|window| window.toplevel())
            && !toplevel.is_initial_configure_sent()
        {
            toplevel.with_pending_state(|pending| {
                pending.size = Some(
                    (
                        i32::try_from(size.0).unwrap_or(i32::MAX).max(96),
                        i32::try_from(size.1).unwrap_or(i32::MAX).max(72),
                    )
                        .into(),
                );
            });
        }
        let unmap = wayland::xdg_shell::will_unmap(&self.wayland, &root)
            .then(|| super::prepare_window_unmap(self, &root));
        let primary_output = self.driver.primary_output().clone();
        let toplevel_commit = wayland::compositor::commit(
            &mut self.wayland,
            &self.cameras,
            &primary_output,
            surface,
            rule,
            smithay::utils::Point::from(self.pointer.position()),
            self.field_config.gap,
        );
        match toplevel_commit.clone() {
            wayland::xdg_shell::ToplevelCommit::Mapped(mapped) => {
                self.nodes.register_mapped(
                    &self.wayland.space,
                    &mapped,
                    self.start_time.elapsed().as_millis() as u64,
                );
                if let Some(id) = self.nodes.id_for_surface(&mapped)
                    && let Some(output) = self.nodes.record(id).map(|record| record.output.clone())
                    && self.clusters.admit_mapped_window(
                        &mut self.nodes.field,
                        &output,
                        id,
                        rule.cluster_participation,
                    )
                {
                    self.request_redraw();
                }
                let remains_collapsed = self
                    .nodes
                    .id_for_surface(&mapped)
                    .and_then(|id| self.nodes.record(id))
                    .is_some_and(|record| record.collapsed);
                let remapped_window = remains_collapsed
                    .then(|| {
                        self.wayland
                            .space
                            .elements()
                            .find(|window| {
                                window
                                    .wl_surface()
                                    .is_some_and(|surface| surface.as_ref() == &mapped)
                            })
                            .cloned()
                    })
                    .flatten();
                if let Some(window) = remapped_window {
                    self.wayland.space.unmap_elem(&window);
                    self.wayland.collapsed.insert(mapped.clone(), window);
                    self.wayland.focused_window = None;
                    super::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
                } else {
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
            }
            wayland::xdg_shell::ToplevelCommit::Unmapped(unmapped) => {
                let preparation =
                    unmap.expect("mapped toplevel unmap must have been prepared before commit");
                debug_assert_eq!(preparation.surface(), &unmapped);
                super::finish_window_unmap(self, preparation);
                self.nodes.mark_detached(&unmapped);
                self.window_rules.forget(&unmapped);
                super::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
            }
            wayland::xdg_shell::ToplevelCommit::None => {}
        }
        if !matches!(
            toplevel_commit,
            wayland::xdg_shell::ToplevelCommit::Unmapped(_)
        ) && let Some(window) = self
            .wayland
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == &root)
            })
            .or_else(|| self.wayland.unmapped.get(&root))
            .or_else(|| self.wayland.collapsed.get(&root))
            .cloned()
        {
            self.window_rules.track_window(&window);
        }
        crate::cursor::surface::handle_commit(&self.cursor, surface, &root);
        crate::wayland::session_lock::surface_committed(self, surface);
        crate::xwayland::handle_commit(self, &root);
        self.fullscreen.handle_commit(
            &mut self.wayland,
            &self.cameras,
            &root,
            crate::frame_clock::monotonic_now(),
        );
        self.maximize.handle_commit(&mut self.wayland, &root);
        crate::input::grab::finish_resize_commit(&mut self.resize_anchor, &mut self.wayland.space);
        crate::nodes::reconcile_landmarks(self, None);
        super::pointer::reconcile_state(self);
        let preview_node = self.nodes.id_for_surface(&root);
        if let Some(id) = preview_node {
            self.render.overlay_previews.mark_dirty(id);
        }
        let apogee_preview_commit = self.apogee_config.live_previews
            && self.apogee.accepts_live_previews()
            && preview_node.is_some_and(|id| self.apogee.contains(id));
        if apogee_preview_commit {
            self.apogee.mark_preview_dirty();
        } else {
            self.request_redraw();
        }
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        crate::wayland::session_lock::surface_destroyed(self, surface);
        super::touch::cancel_surface(self, surface);
        super::gesture::cancel_surface(self, surface);
        let root = wayland::compositor::root_surface(surface);
        super::closing::capture_surface(self, &root);
        if self.cursor.surface_destroyed(surface) {
            self.request_redraw();
        }
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

impl<D: SessionDriver> DrmSyncobjHandler for Session<D> {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
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
            let textures = &mut session.render.fullscreen_textures;
            let capture = session.driver.with_renderer(|renderer| {
                textures.capture_previous(
                    renderer,
                    &window,
                    crate::render::fullscreen_texture::TextureTransitionOwner::Fullscreen,
                )
            });
            if let Err(err) = capture {
                eventline::warn!("fullscreen: failed to capture previous window texture: {err}");
            }
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        super::closing::capture_surface(self, surface.wl_surface());
        self.window_rules.forget(surface.wl_surface());
        let preparation = super::prepare_window_unmap(self, surface.wl_surface());
        wayland::xdg_shell::toplevel_destroyed(&mut self.wayland, &surface);
        super::finish_window_unmap(self, preparation);
        if let Some(id) = self.nodes.id_for_surface(surface.wl_surface()) {
            self.clusters
                .forget_destroyed_member(&mut self.nodes.field, id);
        }
        if let Some(record) = self.nodes.remove_surface(surface.wl_surface()) {
            self.render.overlay_previews.remove(record.id);
        }
        super::sync_keyboard_focus(self, SERIAL_COUNTER.next_serial());
        self.request_redraw();
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        if let Some(id) = self.nodes.id_for_surface(surface.wl_surface())
            && self.nodes.record(id).is_some_and(|record| record.collapsed)
        {
            crate::nodes::restore(self, id, SERIAL_COUNTER.next_serial());
        }
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

    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        if !pointer.has_grab(serial) {
            return;
        }
        let Some(start_data) = pointer.grab_start_data() else {
            return;
        };
        let Some((focused, _)) = start_data.focus else {
            return;
        };
        if wayland::compositor::root_surface(&focused) != *surface.wl_surface() {
            return;
        }
        let Some(window) = self
            .wayland
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == surface.wl_surface())
            })
            .cloned()
        else {
            return;
        };
        if super::begin_pointer_move(self, &window, serial) {
            self.request_redraw();
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // Ignore startup maximize hints. Honoring them during the first reveal
        // creates a monitor-sized configure/remap feedback loop in clients such
        // as Steam. A later decoration-button or title-bar request is deliberate
        // and maps to Halley's Field maximize presentation.
        if !surface.is_initial_configure_sent() {
            surface.with_pending_state(|state| {
                state.states.unset(
                    smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized,
                );
                wayland::decoration::apply_tiled_hint(state);
            });
            surface.send_configure();
            return;
        }
        if let Some(id) = self.nodes.id_for_surface(surface.wl_surface())
            && self.nodes.record(id).is_some_and(|record| record.collapsed)
        {
            crate::nodes::restore(self, id, SERIAL_COUNTER.next_serial());
        }
        super::cancel_grab_for_surface(self, surface.wl_surface());
        super::set_surface_field_maximized(self, surface.wl_surface(), true);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        super::set_surface_field_maximized(self, surface.wl_surface(), false);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.nodes.id_for_surface(surface.wl_surface()) {
            let _ = crate::nodes::collapse(self, id, SERIAL_COUNTER.next_serial());
        }
        // xdg-shell has no minimized state to configure. Sending the current
        // configure still acknowledges the client's state-changing request,
        // including the intentional no-op when the window is already a node.
        surface.send_configure();
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
            if let Some(id) = self.nodes.id_for_surface(&root)
                && self.nodes.record(id).is_some_and(|record| record.collapsed)
            {
                crate::nodes::restore(self, id, SERIAL_COUNTER.next_serial());
            }
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
        super::touch::cancel_surface(self, surface.wl_surface());
        super::gesture::cancel_surface(self, surface.wl_surface());
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

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        if let Some(previous) = self.cursor.set_image(image) {
            crate::cursor::surface::clear_outputs(&previous, &self.wayland.space);
        }
        crate::cursor::surface::refresh_outputs(
            &self.cursor,
            &self.wayland.space,
            self.pointer.position(),
        );
        self.request_redraw();
    }
}

impl<D: SessionDriver> TabletSeatHandler for Session<D> {}

impl<D: SessionDriver> IdleNotifierHandler for Session<D> {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
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

impl<D: SessionDriver> ExtDataControlHandler for Session<D> {
    fn data_control_state(&mut self) -> &mut ExtDataControlState {
        &mut self.wayland.ext_data_control_state
    }
}

impl<D: SessionDriver> smithay::wayland::background_effect::ExtBackgroundEffectHandler
    for Session<D>
{
    fn capabilities(&self) -> smithay::wayland::background_effect::Capability {
        smithay::wayland::background_effect::Capability::Blur
    }

    fn set_blur_region(
        &mut self,
        _surface: WlSurface,
        _region: smithay::wayland::compositor::RegionAttributes,
    ) {
        self.request_redraw();
    }

    fn unset_blur_region(&mut self, _surface: WlSurface) {
        self.request_redraw();
    }
}

delegate_compositor!(@<D: SessionDriver> Session<D>);
delegate_background_effect!(@<D: SessionDriver> Session<D>);
delegate_dmabuf!(@<D: SessionDriver> Session<D>);
delegate_drm_syncobj!(@<D: SessionDriver> Session<D>);
delegate_shm!(@<D: SessionDriver> Session<D>);
delegate_xdg_shell!(@<D: SessionDriver> Session<D>);
delegate_xdg_activation!(@<D: SessionDriver> Session<D>);
delegate_layer_shell!(@<D: SessionDriver> Session<D>);
delegate_xdg_decoration!(@<D: SessionDriver> Session<D>);
delegate_seat!(@<D: SessionDriver> Session<D>);
delegate_cursor_shape!(@<D: SessionDriver> Session<D>);
delegate_output!(@<D: SessionDriver> Session<D>);
delegate_viewporter!(@<D: SessionDriver> Session<D>);
delegate_fractional_scale!(@<D: SessionDriver> Session<D>);
delegate_idle_inhibit!(@<D: SessionDriver> Session<D>);
delegate_idle_notify!(@<D: SessionDriver> Session<D>);
delegate_presentation!(@<D: SessionDriver> Session<D>);
delegate_relative_pointer!(@<D: SessionDriver> Session<D>);
delegate_pointer_constraints!(@<D: SessionDriver> Session<D>);
delegate_pointer_gestures!(@<D: SessionDriver> Session<D>);
delegate_virtual_keyboard_manager!(@<D: SessionDriver> Session<D>);
delegate_keyboard_shortcuts_inhibit!(@<D: SessionDriver> Session<D>);
delegate_data_device!(@<D: SessionDriver> Session<D>);
delegate_primary_selection!(@<D: SessionDriver> Session<D>);
delegate_ext_data_control!(@<D: SessionDriver> Session<D>);

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
