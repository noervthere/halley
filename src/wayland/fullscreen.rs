use std::collections::HashMap;
use std::time::Duration;

use halley_config::Animations;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Serial, Size};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{ToplevelState, ToplevelSurface};

use crate::animation::MotionTimeline;
use crate::presentation::camera::{FullscreenCameraFrame, OutputCameras};

use super::WaylandState;

#[derive(Clone, Debug)]
struct WindowedPlacement {
    location: Point<i32, Logical>,
    geometry: Rectangle<i32, Logical>,
    output: Option<String>,
}

#[derive(Debug)]
struct FullscreenWindow {
    desired: bool,
    active: bool,
    presented: bool,
    target_output: String,
    restore: Option<WindowedPlacement>,
    presentation_windowed: Option<Rectangle<i32, Logical>>,
    /// Exact output-local source retained across a mode-to-mode handoff. This
    /// rectangle already includes the outgoing camera and must not be projected
    /// through the incoming camera again.
    presentation_output: Option<Rectangle<i32, Physical>>,
    fullscreen_size: Size<i32, Logical>,
    transition: Option<MotionTimeline>,
    external_pending: Option<ExternalPending>,
    snapshot_serials: Vec<Serial>,
    origin: FullscreenOrigin,
    native: Option<NativeFullscreenState>,
    preserve_stack: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullscreenOrigin {
    Client,
    Compositor,
    Maximize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NativeFullscreenState {
    client_requested: bool,
    compositor_requested: bool,
    protocol_desired: bool,
    protocol_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenCommitAction {
    Ignore,
    ProtocolOnly,
    Visual(bool),
}

impl NativeFullscreenState {
    fn request(&mut self, origin: FullscreenOrigin) {
        match origin {
            FullscreenOrigin::Client => {
                self.client_requested = true;
                self.protocol_desired = true;
            }
            FullscreenOrigin::Compositor => {
                self.compositor_requested = true;
                // Mod+F is a compositor presentation, not a client fullscreen
                // request. Keep the toplevel protocol-windowed so applications
                // such as Firefox still receive a real fullscreen state edge
                // when their own content later asks to enter fullscreen.
                self.protocol_desired = self.client_requested;
            }
            FullscreenOrigin::Maximize => return,
        }
    }

    fn release(&mut self, origin: FullscreenOrigin) {
        match origin {
            FullscreenOrigin::Client => {
                self.client_requested = false;
                // Honor the client's logical fullscreen edge even while Mod+F
                // retains the output presentation. Firefox uses this edge to
                // reflow HTML fullscreen content when the toplevel was already
                // fullscreen before the video entered fullscreen.
                self.protocol_desired = false;
            }
            FullscreenOrigin::Compositor => {
                self.compositor_requested = false;
                self.protocol_desired = self.client_requested;
            }
            FullscreenOrigin::Maximize => {}
        }
    }

    fn release_all(&mut self) {
        self.client_requested = false;
        self.compositor_requested = false;
        self.protocol_desired = false;
    }

    fn visual_desired(self) -> bool {
        self.client_requested || self.compositor_requested
    }

    fn presentation_origin(self) -> Option<FullscreenOrigin> {
        if self.compositor_requested {
            Some(FullscreenOrigin::Compositor)
        } else if self.client_requested {
            Some(FullscreenOrigin::Client)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalPresentationKind {
    Opening,
    Animated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExternalPending {
    geometry: Rectangle<i32, Logical>,
    presentation: ExternalPresentationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalTransactionRequest {
    NoChange,
    Configure(Rectangle<i32, Logical>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalConfigureResult {
    NotPending,
    Waiting,
    Settled { fullscreen: bool, animated: bool },
}

#[derive(Clone, Copy, Debug)]
pub struct FullscreenPresentation {
    pub progress: f64,
    pub transition_completion: f64,
    pub windowed_geometry: Option<Rectangle<i32, Logical>>,
    pub windowed_output_rect: Option<Rectangle<i32, Physical>>,
    pub fullscreen_size: Size<i32, Logical>,
}

impl FullscreenPresentation {
    pub fn fullscreen_rect(self, output_size: Size<i32, Physical>) -> Rectangle<i32, Physical> {
        let fullscreen_size = self.fullscreen_size.to_physical(1);
        Rectangle::new(
            (
                (output_size.w - fullscreen_size.w) / 2,
                (output_size.h - fullscreen_size.h) / 2,
            )
                .into(),
            fullscreen_size,
        )
    }

    pub fn client_rect(
        self,
        windowed: Rectangle<i32, Physical>,
        output_size: Size<i32, Physical>,
    ) -> Rectangle<i32, Physical> {
        let fullscreen = self.fullscreen_rect(output_size);
        interpolate_rect(windowed, fullscreen, self.progress)
    }
}

pub struct FullscreenManager {
    animations: Animations,
    windows: HashMap<WlSurface, FullscreenWindow>,
}

impl FullscreenManager {
    pub fn new(animations: Animations) -> Self {
        Self {
            animations,
            windows: HashMap::new(),
        }
    }

    pub fn reload(&mut self, animations: Animations) -> bool {
        self.animations = animations;
        if animations_enabled(animations) {
            return false;
        }
        self.windows.retain(|_, entry| {
            entry.transition = None;
            entry.snapshot_serials.clear();
            entry.presented = entry.desired;
            entry.active || entry.desired || entry.presented
        });
        true
    }

    pub fn request(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
        requested: Option<WlOutput>,
    ) {
        self.request_with_origin(wayland, toplevel, requested, FullscreenOrigin::Client);
    }

    pub(crate) fn request_compositor(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
        now: Duration,
    ) {
        self.request_with_origin(wayland, toplevel, None, FullscreenOrigin::Compositor);
        self.begin_compositor_visual(toplevel.wl_surface(), now);
    }

    fn request_with_origin(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
        requested: Option<WlOutput>,
        origin: FullscreenOrigin,
    ) {
        let window = find_window(wayland, toplevel.wl_surface()).cloned();
        // A client can enter its own fullscreen mode while the compositor is
        // already presenting the same toplevel through Mod+F. Keep the
        // compositor's output target in that case; client protocol ownership
        // is tracked independently below.
        let retained_compositor_target = self
            .windows
            .get(toplevel.wl_surface())
            .filter(|entry| {
                origin == FullscreenOrigin::Client
                    && entry
                        .native
                        .is_some_and(|native| native.compositor_requested)
            })
            .map(|entry| entry.target_output.clone());
        let requested = requested.filter(|resource| {
            Output::from_resource(resource)
                .is_some_and(|output| wayland.space.outputs().any(|known| known == &output))
        });
        let requested_output = requested.as_ref().and_then(Output::from_resource);
        let target = retained_compositor_target
            .as_deref()
            .and_then(|name| output_by_name(wayland, name))
            .or(requested_output)
            .or_else(|| {
                window
                    .as_ref()
                    .and_then(super::window_output_name)
                    .and_then(|name| output_by_name(wayland, &name))
            })
            .or_else(|| super::focus::selected_output(wayland).cloned());

        let Some(target) = target else {
            send_required_configure(toplevel);
            return;
        };
        let Some(output_geometry) = wayland.space.output_geometry(&target) else {
            send_required_configure(toplevel);
            return;
        };

        let entry = self
            .windows
            .entry(toplevel.wl_surface().clone())
            .or_insert_with(|| FullscreenWindow {
                desired: false,
                active: false,
                presented: false,
                target_output: target.name(),
                restore: window.as_ref().and_then(|window| {
                    Some(WindowedPlacement {
                        location: wayland.space.element_location(window)?,
                        geometry: wayland.space.element_geometry(window)?,
                        output: super::window_output_name(window),
                    })
                }),
                presentation_windowed: None,
                presentation_output: None,
                fullscreen_size: output_geometry.size,
                transition: None,
                external_pending: None,
                snapshot_serials: Vec::new(),
                origin,
                native: Some(NativeFullscreenState::default()),
                preserve_stack: false,
            });
        let transition_requested = !entry.active;
        request_native_owner(entry, origin);
        entry.target_output = target.name();
        // The destination is the size we are about to configure, decided once
        // here, exactly like field maximize decides its target rect at toggle
        // time. `handle_commit` only re-reads the client's committed size once
        // the transition has settled, to letterbox a client that stays smaller.
        entry.fullscreen_size = output_geometry.size;
        let protocol_origin = entry.origin;
        let protocol_desired = entry.native.map_or(true, |native| native.protocol_desired);

        toplevel.with_pending_state(|state| {
            apply_protocol_presentation_state(state, protocol_origin, protocol_desired);
            super::decoration::clear_tiled_hint(state);
            state.size = Some(output_geometry.size);
            state.bounds = Some(output_geometry.size);
            state.fullscreen_output = (protocol_origin != FullscreenOrigin::Maximize
                && protocol_desired)
                .then_some(requested)
                .flatten();
        });
        if let Some(serial) = send_required_configure(toplevel)
            && animations_enabled(self.animations)
            && transition_requested
        {
            entry.snapshot_serials.push(serial);
        }
    }

    /// Applies an xdg_toplevel `unset_fullscreen` request without allowing it
    /// to release a presentation which was explicitly selected by Mod+F.
    pub fn unrequest_client(&mut self, wayland: &WaylandState, toplevel: &ToplevelSurface) {
        let nested = self
            .windows
            .get_mut(toplevel.wl_surface())
            .and_then(|entry| {
                release_native_owner(entry, FullscreenOrigin::Client);
                entry.native?.compositor_requested.then_some((
                    entry.target_output.clone(),
                    entry.fullscreen_size,
                    entry.origin,
                ))
            });
        if let Some((target_output, fullscreen_size, origin)) = nested {
            let bounds = output_by_name(wayland, &target_output)
                .and_then(|output| wayland.space.output_geometry(&output))
                .map_or(fullscreen_size, |geometry| geometry.size);
            toplevel.with_pending_state(|state| {
                apply_protocol_presentation_state(state, origin, false);
                // The protocol state is windowed for this configure edge, but
                // Mod+F still owns the visual output presentation. Keep the
                // configured size pinned so the edge cannot expose or restore
                // the field-sized window underneath it.
                state.size = Some(bounds);
                state.bounds = Some(bounds);
                state.fullscreen_output = None;
                super::decoration::clear_tiled_hint(state);
            });
            send_required_configure(toplevel);
            return;
        }
        self.unrequest(wayland, toplevel);
    }

    /// Releases only Mod+F ownership. A client which is independently
    /// fullscreen remains fullscreen and becomes the presentation owner.
    pub(crate) fn unrequest_compositor(
        &mut self,
        wayland: &WaylandState,
        toplevel: &ToplevelSurface,
        now: Duration,
    ) {
        let retained_client = self
            .windows
            .get_mut(toplevel.wl_surface())
            .and_then(|entry| {
                let had_compositor = entry.native?.compositor_requested;
                release_native_owner(entry, FullscreenOrigin::Compositor);
                (had_compositor && entry.native?.client_requested).then_some((
                    entry.target_output.clone(),
                    entry.fullscreen_size,
                    entry.origin,
                ))
            });
        if let Some((target_output, fullscreen_size, origin)) = retained_client {
            let bounds = output_by_name(wayland, &target_output)
                .and_then(|output| wayland.space.output_geometry(&output))
                .map_or(fullscreen_size, |geometry| geometry.size);
            toplevel.with_pending_state(|state| {
                apply_protocol_presentation_state(state, origin, true);
                state.size = Some(bounds);
                state.bounds = Some(bounds);
                super::decoration::clear_tiled_hint(state);
            });
            send_required_configure(toplevel);
            return;
        }
        self.unrequest(wayland, toplevel);
        self.begin_compositor_visual(toplevel.wl_surface(), now);
    }

    /// Starts a Mod+F presentation at input time instead of waiting for the
    /// client to acknowledge and commit its new size. Geometry remains
    /// commit-gated in `handle_commit`; this only makes the visual timeline as
    /// responsive and reversible as field maximize.
    fn begin_compositor_visual(&mut self, surface: &WlSurface, now: Duration) {
        let Some(entry) = self.windows.get_mut(surface) else {
            return;
        };
        begin_compositor_visual(entry, self.animations, now);
    }

    pub fn unrequest(&mut self, wayland: &WaylandState, toplevel: &ToplevelSurface) {
        let (restore_size, transition_requested, origin) = self
            .windows
            .get_mut(toplevel.wl_surface())
            .map(|entry| {
                release_all_native_owners(entry);
                if let Some(window) = find_window(wayland, toplevel.wl_surface()) {
                    entry.fullscreen_size = window.geometry().size;
                }
                entry.presentation_windowed =
                    entry.restore.as_ref().map(|restore| restore.geometry);
                if !entry.preserve_stack {
                    entry.presentation_output = None;
                }
                (
                    entry.restore.as_ref().map(|restore| restore.geometry.size),
                    entry.active,
                    entry.origin,
                )
            })
            .unwrap_or((None, false, FullscreenOrigin::Client));
        let bounds = self
            .windows
            .get(toplevel.wl_surface())
            .and_then(|entry| output_by_name(wayland, &entry.target_output))
            .and_then(|output| wayland.space.output_geometry(&output))
            .map(|geometry| geometry.size);

        toplevel.with_pending_state(|state| {
            apply_protocol_presentation_state(state, origin, false);
            state.size = restore_size;
            state.bounds = bounds;
            state.fullscreen_output = None;
            super::decoration::apply_tiled_hint(state);
        });
        if let Some(serial) = send_required_configure(toplevel)
            && animations_enabled(self.animations)
            && transition_requested
            && let Some(entry) = self.windows.get_mut(toplevel.wl_surface())
        {
            entry.snapshot_serials.push(serial);
        }
    }

    /// Drops fullscreen state without arming the exit transition.
    ///
    /// Used only when another presentation immediately takes ownership of the
    /// same window - today the field-maximize handoff. The caller's animation
    /// covers the whole travel from the on-screen fullscreen rect, so a second
    /// shrink transition here would fight it (fullscreen wins in
    /// `window_visual_state`, so the maximize grow would only become visible
    /// once the shrink retired, mid-flight). Sending no configure and pushing
    /// no snapshot serial leaves the single configure and the captured
    /// crossfade texture to the caller.
    pub(crate) fn retire_for_handoff(&mut self, toplevel: &ToplevelSurface) {
        let Some(entry) = self.windows.remove(toplevel.wl_surface()) else {
            return;
        };
        toplevel.with_pending_state(|state| {
            apply_protocol_presentation_state(state, entry.origin, false);
            state.fullscreen_output = None;
            super::decoration::apply_tiled_hint(state);
        });
    }

    pub(crate) fn request_external(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
        origin: FullscreenOrigin,
    ) -> Option<Rectangle<i32, Logical>> {
        let wl_surface = window.wl_surface().map(|surface| surface.into_owned())?;
        let window = find_window(wayland, &wl_surface).cloned()?;
        let target = super::window_output_name(&window)
            .and_then(|name| output_by_name(wayland, &name))
            .or_else(|| super::focus::selected_output(wayland).cloned());
        let target = target?;
        let output_geometry = wayland.space.output_geometry(&target)?;
        let target_name = target.name();
        self.windows
            .entry(wl_surface)
            .and_modify(|entry| {
                entry.origin = origin;
                settle_external_fullscreen(entry, &target_name, output_geometry.size);
            })
            .or_insert_with(|| FullscreenWindow {
                desired: true,
                active: true,
                presented: true,
                target_output: target_name,
                restore: Some(WindowedPlacement {
                    location: wayland
                        .space
                        .element_location(&window)
                        .unwrap_or(output_geometry.loc),
                    geometry: wayland
                        .space
                        .element_geometry(&window)
                        .unwrap_or_else(|| window.geometry()),
                    output: super::window_output_name(&window),
                }),
                presentation_windowed: None,
                presentation_output: None,
                fullscreen_size: output_geometry.size,
                transition: None,
                external_pending: None,
                snapshot_serials: Vec::new(),
                origin,
                native: None,
                preserve_stack: false,
            });
        super::set_window_output(&window, &target);
        // X11 fullscreen changes presentation geometry, not stacking. Using
        // `map_element` here would silently move an already-mapped window to
        // the top and cover windows the user had deliberately kept above it.
        wayland.space.relocate_element(&window, output_geometry.loc);
        Some(output_geometry)
    }

    pub fn unrequest_external(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
    ) -> Option<Rectangle<i32, Logical>> {
        let wl_surface = window.wl_surface().map(|surface| surface.into_owned())?;
        let restore = self
            .windows
            .remove(&wl_surface)
            .and_then(|entry| entry.restore)?;
        if let Some(output) = restore
            .output
            .as_deref()
            .and_then(|name| output_by_name(wayland, name))
        {
            super::set_window_output(window, &output);
        }
        // Leaving X11 fullscreen preserves the same stack slot as entry.
        wayland.space.relocate_element(window, restore.location);
        Some(restore.geometry)
    }

    pub(crate) fn request_external_animated(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
        origin: FullscreenOrigin,
        now: Duration,
    ) -> Option<ExternalTransactionRequest> {
        let request = self.request_external_transaction(
            wayland,
            window,
            ExternalPresentationKind::Animated,
            None,
            origin,
        )?;
        if origin == FullscreenOrigin::Compositor
            && request != ExternalTransactionRequest::NoChange
            && let Some(surface) = window.wl_surface()
        {
            self.begin_compositor_visual(surface.as_ref(), now);
        }
        Some(request)
    }

    pub(crate) fn request_external_opening(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
        restore_geometry: Option<Rectangle<i32, Logical>>,
        origin: FullscreenOrigin,
    ) -> Option<ExternalTransactionRequest> {
        self.request_external_transaction(
            wayland,
            window,
            ExternalPresentationKind::Opening,
            restore_geometry,
            origin,
        )
    }

    fn request_external_transaction(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
        presentation: ExternalPresentationKind,
        restore_geometry: Option<Rectangle<i32, Logical>>,
        origin: FullscreenOrigin,
    ) -> Option<ExternalTransactionRequest> {
        let wl_surface = window.wl_surface().map(|surface| surface.into_owned())?;
        let window = find_window(wayland, &wl_surface).cloned()?;
        let target = super::window_output_name(&window)
            .and_then(|name| output_by_name(wayland, &name))
            .or_else(|| super::focus::selected_output(wayland).cloned())?;
        let output_geometry = wayland.space.output_geometry(&target)?;
        let target_name = target.name();
        let current_restore =
            wayland
                .space
                .element_location(&window)
                .map(|location| WindowedPlacement {
                    location,
                    geometry: wayland
                        .space
                        .element_geometry(&window)
                        .unwrap_or_else(|| Rectangle::new(location, window.geometry().size)),
                    output: super::window_output_name(&window),
                });
        let restore = prefer_seeded_restore(
            restore_geometry.map(|geometry| WindowedPlacement {
                location: geometry.loc,
                geometry,
                output: super::window_output_name(&window),
            }),
            current_restore,
        );

        let entry = self
            .windows
            .entry(wl_surface)
            .or_insert_with(|| FullscreenWindow {
                desired: false,
                active: false,
                presented: false,
                target_output: target_name.clone(),
                restore: restore.clone(),
                presentation_windowed: None,
                presentation_output: None,
                fullscreen_size: output_geometry.size,
                transition: None,
                external_pending: None,
                snapshot_serials: Vec::new(),
                origin,
                native: None,
                preserve_stack: false,
            });
        entry.origin = origin;
        entry.target_output = target_name;
        entry.fullscreen_size = output_geometry.size;
        if entry.restore.is_none() {
            entry.restore = restore;
        }
        super::set_window_output(&window, &target);
        Some(begin_external_transaction(
            entry,
            true,
            output_geometry,
            presentation,
        ))
    }

    pub(crate) fn unrequest_external_animated(
        &mut self,
        window: &Window,
        origin: FullscreenOrigin,
        now: Duration,
    ) -> Option<ExternalTransactionRequest> {
        let request =
            self.unrequest_external_transaction(window, ExternalPresentationKind::Animated)?;
        if origin == FullscreenOrigin::Compositor
            && request != ExternalTransactionRequest::NoChange
            && let Some(surface) = window.wl_surface()
        {
            self.begin_compositor_visual(surface.as_ref(), now);
        }
        Some(request)
    }

    pub(crate) fn unrequest_external_opening(
        &mut self,
        window: &Window,
    ) -> Option<ExternalTransactionRequest> {
        self.unrequest_external_transaction(window, ExternalPresentationKind::Opening)
    }

    fn unrequest_external_transaction(
        &mut self,
        window: &Window,
        presentation: ExternalPresentationKind,
    ) -> Option<ExternalTransactionRequest> {
        let wl_surface = window.wl_surface().map(|surface| surface.into_owned())?;
        let entry = self.windows.get_mut(&wl_surface)?;
        let geometry = entry.restore.as_ref()?.geometry;
        entry.presentation_windowed = Some(geometry);
        if !entry.preserve_stack {
            entry.presentation_output = None;
        }
        Some(begin_external_transaction(
            entry,
            false,
            geometry,
            presentation,
        ))
    }

    pub(crate) fn settle_external_configure(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
        observed: Rectangle<i32, Logical>,
        now: Duration,
    ) -> ExternalConfigureResult {
        let Some(wl_surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
            return ExternalConfigureResult::NotPending;
        };
        let Some(entry) = self.windows.get_mut(&wl_surface) else {
            return ExternalConfigureResult::NotPending;
        };
        let result = acknowledge_external_transaction(entry, observed, self.animations, now);
        let ExternalConfigureResult::Settled {
            fullscreen,
            animated,
        } = result
        else {
            return result;
        };
        if !relocate_external_window(wayland, window, entry) {
            entry.transition = None;
            entry.presented = fullscreen;
            return ExternalConfigureResult::Settled {
                fullscreen,
                animated: false,
            };
        }
        ExternalConfigureResult::Settled {
            fullscreen,
            animated,
        }
    }

    pub(crate) fn finish_external_presentation(
        &mut self,
        wayland: &mut WaylandState,
        window: &Window,
    ) -> bool {
        let Some(wl_surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
            return false;
        };
        let Some(entry) = self.windows.get_mut(&wl_surface) else {
            return false;
        };
        if entry.external_pending.is_none() && entry.transition.is_none() {
            return false;
        }
        finish_external_transition(entry);
        relocate_external_window(wayland, window, entry)
    }

    pub(crate) fn external_desired_matches(&self, window: &Window, fullscreen: bool) -> bool {
        window
            .wl_surface()
            .is_some_and(|surface| desired_matches(self.windows.get(surface.as_ref()), fullscreen))
    }

    pub(crate) fn update_external_windowed_placement(
        &mut self,
        wayland: &WaylandState,
        window: &Window,
    ) {
        let Some(surface) = window.wl_surface() else {
            return;
        };
        let Some(entry) = self.windows.get_mut(surface.as_ref()) else {
            return;
        };
        if !can_update_external_restore(entry) {
            return;
        }
        let (Some(location), Some(geometry)) = (
            wayland.space.element_location(window),
            wayland.space.element_geometry(window),
        ) else {
            return;
        };
        entry.restore = Some(WindowedPlacement {
            location,
            geometry,
            output: super::window_output_name(window),
        });
    }

    pub fn handle_commit(
        &mut self,
        wayland: &mut WaylandState,
        cameras: &OutputCameras,
        surface: &WlSurface,
        now: Duration,
    ) -> bool {
        let Some(window) = find_window(wayland, surface).cloned() else {
            return false;
        };
        let Some(toplevel) = window.toplevel() else {
            return false;
        };
        let Some(entry) = self.windows.get(surface) else {
            return false;
        };
        let origin = entry.origin;
        let visual_desired = entry.desired;
        let target_output = entry.target_output.clone();
        let restore = entry.restore.clone();
        let preserve_stack = entry.preserve_stack;
        let committed = toplevel.with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(protocol_presentation_state(origin)))
        });
        let commit_action = fullscreen_commit_action(entry, committed);
        if commit_action == FullscreenCommitAction::Ignore {
            return false;
        }
        if let Some(native) = self
            .windows
            .get_mut(surface)
            .and_then(|entry| entry.native.as_mut())
        {
            native.protocol_active = committed;
        }
        if commit_action == FullscreenCommitAction::ProtocolOnly {
            if !visual_desired {
                return false;
            }
            let Some(output) = output_by_name(wayland, &target_output) else {
                return false;
            };
            let Some(output_geometry) = wayland.space.output_geometry(&output) else {
                return false;
            };
            let size = window.geometry().size;
            let location = center_in_rect(size, output_geometry.loc, output_geometry.size);
            if wayland.space.element_location(&window) != Some(location) {
                wayland.space.relocate_element(&window, location);
            }
            let entry = self.windows.get_mut(surface).expect("entry checked above");
            if may_adopt_client_size(entry, now) {
                entry.fullscreen_size = size;
            }
            return false;
        }

        let Some(output) = output_by_name(wayland, &target_output) else {
            return false;
        };
        let Some(output_geometry) = wayland.space.output_geometry(&output) else {
            return false;
        };

        if visual_desired {
            super::set_window_output(&window, &output);
            let location = center_in_rect(
                window.geometry().size,
                output_geometry.loc,
                output_geometry.size,
            );
            if preserve_stack {
                wayland.space.relocate_element(&window, location);
            } else {
                wayland.space.map_element(window.clone(), location, true);
            }
        } else {
            let location = match restore.as_ref() {
                Some(restore) if restore.output.as_deref() == Some(target_output.as_str()) => {
                    restore.location
                }
                _ => crate::window::routing::centered_location_for_size(
                    wayland,
                    cameras,
                    &output,
                    window.geometry().size,
                ),
            };
            super::set_window_output(&window, &output);
            if preserve_stack {
                wayland.space.relocate_element(&window, location);
            } else {
                wayland.space.map_element(window.clone(), location, true);
            }
        }

        let entry = self.windows.get_mut(surface).expect("entry checked above");
        if !visual_desired
            && let (Some(location), Some(geometry)) = (
                wayland.space.element_location(&window),
                wayland.space.element_geometry(&window),
            )
        {
            record_native_exit_placement(
                entry,
                restore,
                WindowedPlacement {
                    location,
                    geometry,
                    output: Some(output.name()),
                },
            );
        }
        settle_visual_commit(entry, self.animations, now, visual_desired);
        true
    }

    pub fn presentation(
        &self,
        surface: &WlSurface,
        output: &Output,
        now: Duration,
    ) -> Option<FullscreenPresentation> {
        let entry = self.windows.get(surface)?;
        if entry.target_output != output.name() {
            return None;
        }
        let progress = entry
            .transition
            .map(|transition| transition.value_at(now))
            .unwrap_or_else(|| if entry.presented { 1.0 } else { 0.0 })
            .clamp(0.0, 1.0);
        let transition_completion = entry
            .transition
            .map(|transition| transition.completion_at(now))
            .unwrap_or(1.0);
        fullscreen_presentation_is_visible(progress, entry_owns_presentation(entry)).then_some(
            FullscreenPresentation {
                progress,
                transition_completion,
                windowed_geometry: entry
                    .presentation_windowed
                    .or_else(|| entry.restore.as_ref().map(|restore| restore.geometry)),
                windowed_output_rect: entry.presentation_output,
                fullscreen_size: entry.fullscreen_size,
            },
        )
    }

    /// Returns the monitor-wide camera track for the fullscreen transaction.
    ///
    /// The endpoint is the original window center at native output zoom, as
    /// in old Halley. Unlike the surface presentation, this deliberately
    /// remains present at progress zero while a request/exit transaction owns
    /// the output, allowing input mutation to stay locked until cleanup.
    pub fn camera_frame(
        &self,
        output: &Output,
        output_geometry: Rectangle<i32, Logical>,
        now: Duration,
    ) -> Option<FullscreenCameraFrame> {
        self.camera_frame_matching(output, output_geometry, now, |_| true)
    }

    pub(crate) fn camera_frame_matching(
        &self,
        output: &Output,
        output_geometry: Rectangle<i32, Logical>,
        now: Duration,
        matches: impl Fn(&WlSurface) -> bool,
    ) -> Option<FullscreenCameraFrame> {
        let entry = self.windows.iter().find_map(|(surface, entry)| {
            (matches(surface)
                && entry.target_output == output.name()
                && (entry.active
                    || entry.desired
                    || entry.presented
                    || entry.transition.is_some()
                    || entry.external_pending.is_some()))
            .then_some(entry)
        })?;
        let progress = entry
            .transition
            .map(|transition| transition.value_at(now))
            .unwrap_or_else(|| if entry.presented { 1.0 } else { 0.0 })
            .clamp(0.0, 1.0) as f32;
        let center = entry
            .restore
            .as_ref()
            .map(|restore| {
                let geometry = restore.geometry;
                Point::<f32, Physical>::from((
                    geometry.loc.x as f32 + geometry.size.w as f32 / 2.0
                        - output_geometry.loc.x as f32,
                    geometry.loc.y as f32 + geometry.size.h as f32 / 2.0
                        - output_geometry.loc.y as f32,
                ))
            })
            .unwrap_or_else(|| {
                Point::from((
                    output_geometry.size.w as f32 / 2.0,
                    output_geometry.size.h as f32 / 2.0,
                ))
            });
        Some(FullscreenCameraFrame {
            center,
            progress,
            desired: entry.desired,
            transition_active: entry.transition.is_some(),
        })
    }

    pub fn covers_top(&self, focused: Option<&WlSurface>, output: &Output, now: Duration) -> bool {
        self.covers_top_matching(focused, output, now, |_| true)
    }

    pub(crate) fn covers_top_matching(
        &self,
        _focused: Option<&WlSurface>,
        output: &Output,
        now: Duration,
        matches: impl Fn(&WlSurface) -> bool,
    ) -> bool {
        self.windows.iter().any(|(surface, entry)| {
            matches(surface) && entry_covers_top(entry, &output.name(), now)
        })
    }

    pub fn covers_any_top(
        &self,
        wayland: &WaylandState,
        focused: Option<&WlSurface>,
        now: Duration,
    ) -> bool {
        wayland
            .space
            .outputs()
            .any(|output| self.covers_top(focused, output, now))
    }

    pub fn is_animating_on_output(&self, output: &Output, now: Duration) -> bool {
        self.is_animating_on_output_matching(output, now, |_| true)
    }

    pub(crate) fn is_animating_on_output_matching(
        &self,
        output: &Output,
        now: Duration,
        matches: impl Fn(&WlSurface) -> bool,
    ) -> bool {
        self.windows.iter().any(|(surface, entry)| {
            matches(surface)
                && entry.target_output == output.name()
                && entry
                    .transition
                    .is_some_and(|transition| !transition.is_finished_at(now))
        })
    }

    /// Whether a real fullscreen request owns, or is in the process of
    /// taking/releasing, this output. Unlike `covers_top`, this includes the
    /// configure and transition handoffs so input-side desktop maintenance
    /// cannot leak into a fullscreen path between frames.
    pub(crate) fn has_fullscreen_activity_on_output_matching(
        &self,
        output: &Output,
        matches: impl Fn(&WlSurface) -> bool,
    ) -> bool {
        self.windows.iter().any(|(surface, entry)| {
            matches(surface)
                && entry.origin != FullscreenOrigin::Maximize
                && entry_occupies_output(entry, &output.name())
        })
    }

    pub fn has_stable_fullscreen_on_output(&self, output: &Output, now: Duration) -> bool {
        self.stable_fullscreen_surface_on_output(output, now)
            .is_some()
    }

    pub fn stable_fullscreen_surface_on_output(
        &self,
        output: &Output,
        now: Duration,
    ) -> Option<&WlSurface> {
        self.stable_fullscreen_surface_on_output_matching(output, now, |_| true)
    }

    pub(crate) fn stable_fullscreen_surface_on_output_matching(
        &self,
        output: &Output,
        now: Duration,
        matches: impl Fn(&WlSurface) -> bool,
    ) -> Option<&WlSurface> {
        self.windows.iter().find_map(|(surface, entry)| {
            (matches(surface)
                && entry.target_output == output.name()
                && entry.origin != FullscreenOrigin::Maximize
                && entry.desired
                && entry.active
                && (entry.presented
                    || entry
                        .transition
                        .is_some_and(|transition| transition.is_finished_at(now)))
                && entry.external_pending.is_none())
            .then_some(surface)
        })
    }

    /// Whether compositor policy may add blur behind this surface.
    ///
    /// A client-owned fullscreen is an immersive presentation request and
    /// keeps the composition-saving fast path. Compositor-owned fullscreen
    /// (`Mod+F`) and maximize presentations remain ordinary managed-window
    /// views, so configured window effects continue through their transition.
    pub(crate) fn allows_global_blur(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_none_or(|entry| fullscreen_origin_allows_global_blur(entry.origin))
    }

    /// Whether an external (X11) configure handoff is still outstanding.
    ///
    /// The transaction owns the window's geometry until the client acks, so
    /// compositor-side position resync must not race it.
    pub(crate) fn awaits_external_configure(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_some_and(|entry| entry.external_pending.is_some())
    }

    pub fn is_fullscreen_or_pending(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_some_and(|entry| entry.active || entry.desired)
    }

    /// Whether compositor-owned window chrome should be omitted.
    ///
    /// Chrome follows logical fullscreen state rather than presentation
    /// progress: entering fullscreen removes it before the first animated
    /// frame, and leaving fullscreen restores it while the window animates
    /// back. Maximize presentations remain decorated.
    pub(crate) fn suppresses_chrome(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_some_and(fullscreen_entry_suppresses_chrome)
    }

    pub(crate) fn occupants_on_output(
        &self,
        output: &str,
        except: &WlSurface,
    ) -> Vec<(WlSurface, FullscreenOrigin)> {
        self.windows
            .iter()
            .filter(|(surface, entry)| *surface != except && entry_occupies_output(entry, output))
            .map(|(surface, entry)| (surface.clone(), entry.origin))
            .collect()
    }

    pub(crate) fn restore_placement(
        &self,
        surface: &WlSurface,
    ) -> Option<(Rectangle<i32, Logical>, Option<String>)> {
        let restore = self.windows.get(surface)?.restore.as_ref()?;
        Some((restore.geometry, restore.output.clone()))
    }

    pub(crate) fn restore_location(
        &self,
        surface: &WlSurface,
    ) -> Option<(Point<i32, Logical>, Option<String>)> {
        let restore = self.windows.get(surface)?.restore.as_ref()?;
        Some((restore.location, restore.output.clone()))
    }

    pub(crate) fn override_restore_from_field(
        &mut self,
        surface: &WlSurface,
        restore_geometry: Rectangle<i32, Logical>,
        restore_output: String,
        field_geometry: Rectangle<i32, Logical>,
        field_output_rect: Option<Rectangle<i32, Physical>>,
    ) {
        if let Some(entry) = self.windows.get_mut(surface) {
            entry.restore = Some(WindowedPlacement {
                location: restore_geometry.loc,
                geometry: restore_geometry,
                output: Some(restore_output),
            });
            entry.presentation_windowed = Some(field_geometry);
            entry.presentation_output = field_output_rect;
        }
    }

    pub(crate) fn override_restore_from_cluster(
        &mut self,
        surface: &WlSurface,
        restore_geometry: Rectangle<i32, Logical>,
        restore_output: String,
        presentation_output: Option<Rectangle<i32, Physical>>,
    ) {
        if let Some(entry) = self.windows.get_mut(surface) {
            entry.restore = Some(WindowedPlacement {
                location: restore_geometry.loc,
                geometry: restore_geometry,
                output: Some(restore_output),
            });
            entry.presentation_windowed = Some(restore_geometry);
            entry.presentation_output = presentation_output;
            entry.preserve_stack = true;
        }
    }

    /// Match a client commit to the fullscreen configure which requested it.
    /// Clients may skip intermediate configures, so all serials no newer than
    /// the acknowledged commit are consumed, as in Niri's resize path.
    pub fn should_capture_snapshot(&mut self, surface: &WlSurface, commit_serial: Serial) -> bool {
        let Some(entry) = self.windows.get_mut(surface) else {
            return false;
        };
        let mut capture = false;
        entry.snapshot_serials.retain(|serial| {
            if commit_serial.is_no_older_than(serial) {
                capture = true;
                false
            } else {
                true
            }
        });
        capture
    }

    pub(crate) fn should_capture_external_snapshot(
        &self,
        surface: &WlSurface,
        fullscreen: bool,
    ) -> bool {
        animations_enabled(self.animations)
            && self
                .windows
                .get(surface)
                .is_none_or(|entry| entry.desired != fullscreen)
    }

    pub(crate) fn animations_enabled(&self) -> bool {
        animations_enabled(self.animations)
    }

    pub fn reconfigure_output(
        &mut self,
        wayland: &WaylandState,
        output: &Output,
    ) -> Vec<(Window, Rectangle<i32, Logical>)> {
        let Some(geometry) = wayland.space.output_geometry(output) else {
            return Vec::new();
        };
        let mut external = Vec::new();
        for (surface, entry) in &mut self.windows {
            if entry.target_output != output.name() || !(entry.active || entry.desired) {
                continue;
            }
            let Some(window) = find_window(wayland, surface) else {
                continue;
            };
            if let Some(toplevel) = window.toplevel() {
                let origin = entry.origin;
                let protocol_desired = entry
                    .native
                    .map_or(entry.desired, |native| native.protocol_desired);
                entry.fullscreen_size = geometry.size;
                toplevel.with_pending_state(|state| {
                    apply_protocol_presentation_state(state, origin, protocol_desired);
                    state.size = Some(geometry.size);
                    state.bounds = Some(geometry.size);
                    super::decoration::clear_tiled_hint(state);
                });
                toplevel.send_configure();
            } else {
                entry.fullscreen_size = geometry.size;
                external.push((window.clone(), geometry));
            }
        }
        external
    }

    pub fn cleanup(&mut self, now: Duration) -> FullscreenCleanup {
        let mut finished = false;
        let mut finished_surfaces = Vec::new();
        self.windows.retain(|surface, entry| {
            if entry
                .transition
                .is_some_and(|transition| transition.is_finished_at(now))
                && entry.active == entry.desired
                && entry.external_pending.is_none()
            {
                entry.transition = None;
                entry.presented = entry.desired;
                finished = true;
                finished_surfaces.push(surface.clone());
            }
            entry.active
                || entry.desired
                || entry.presented
                || entry.transition.is_some()
                || entry.external_pending.is_some()
        });
        FullscreenCleanup {
            visual_finished: finished,
            finished_surfaces,
        }
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
    }
}

pub struct FullscreenCleanup {
    pub visual_finished: bool,
    pub finished_surfaces: Vec<WlSurface>,
}

fn animations_enabled(animations: Animations) -> bool {
    animations.enabled && animations.fullscreen.enabled
}

fn protocol_presentation_state(origin: FullscreenOrigin) -> State {
    match origin {
        FullscreenOrigin::Maximize => State::Maximized,
        FullscreenOrigin::Client | FullscreenOrigin::Compositor => State::Fullscreen,
    }
}

fn apply_protocol_presentation_state(
    state: &mut ToplevelState,
    origin: FullscreenOrigin,
    active: bool,
) {
    state.states.unset(State::Fullscreen);
    state.states.unset(State::Maximized);
    if active {
        state.states.set(protocol_presentation_state(origin));
    }
}

fn fullscreen_presentation_is_visible(progress: f64, transition_active: bool) -> bool {
    // A transition owns the handoff even at its exact zero endpoint. Dropping
    // presentation there exposes the client's newly configured live buffer for
    // one frame before the captured texture blend takes over (and once again
    // on exit immediately before cleanup), which reads as a fullscreen flash.
    transition_active || progress > 0.0
}

fn entry_owns_presentation(entry: &FullscreenWindow) -> bool {
    entry.transition.is_some() || entry.desired != entry.active || entry.external_pending.is_some()
}

fn fullscreen_origin_allows_global_blur(origin: FullscreenOrigin) -> bool {
    origin != FullscreenOrigin::Client
}

fn fullscreen_entry_suppresses_chrome(entry: &FullscreenWindow) -> bool {
    entry.desired && entry.origin != FullscreenOrigin::Maximize
}

fn request_native_owner(entry: &mut FullscreenWindow, origin: FullscreenOrigin) {
    let native = entry
        .native
        .get_or_insert_with(NativeFullscreenState::default);
    native.request(origin);
    entry.desired = native.visual_desired();
    entry.origin = native.presentation_origin().unwrap_or(origin);
}

fn release_native_owner(entry: &mut FullscreenWindow, origin: FullscreenOrigin) {
    let Some(native) = entry.native.as_mut() else {
        return;
    };
    native.release(origin);
    entry.desired = native.visual_desired();
    entry.origin = native.presentation_origin().unwrap_or(entry.origin);
}

fn release_all_native_owners(entry: &mut FullscreenWindow) {
    if let Some(native) = entry.native.as_mut() {
        native.release_all();
    }
    entry.desired = false;
}

fn fullscreen_commit_action(
    entry: &FullscreenWindow,
    protocol_committed: bool,
) -> FullscreenCommitAction {
    let protocol_desired = entry
        .native
        .map_or(entry.desired, |native| native.protocol_desired);
    if protocol_committed != protocol_desired {
        FullscreenCommitAction::Ignore
    } else if entry.desired == entry.active {
        FullscreenCommitAction::ProtocolOnly
    } else {
        FullscreenCommitAction::Visual(entry.desired)
    }
}

/// Whether a commit may retarget the fullscreen rect to the client's own size.
///
/// Adopting the committed size is how a client that stays smaller than the
/// output gets letterboxed, but the fullscreen rect is also the transition's
/// destination. Clients routinely ack the fullscreen configure a commit or two
/// before their buffer catches up, so sampling mid-flight aims the grow at the
/// old windowed size and then snaps outward when the real buffer lands. Field
/// maximize never had this because its target rect is fixed at toggle time.
fn may_adopt_client_size(entry: &FullscreenWindow, now: Duration) -> bool {
    entry
        .transition
        .is_none_or(|transition| transition.is_finished_at(now))
}

fn entry_covers_top(entry: &FullscreenWindow, output: &str, now: Duration) -> bool {
    entry.target_output == output
        && entry.active
        && entry
            .transition
            .is_none_or(|transition| transition.is_finished_at(now))
}

fn desired_matches(entry: Option<&FullscreenWindow>, desired: bool) -> bool {
    entry.is_some_and(|entry| entry.desired == desired)
}

fn prefer_seeded_restore(
    seeded: Option<WindowedPlacement>,
    current: Option<WindowedPlacement>,
) -> Option<WindowedPlacement> {
    seeded.or(current)
}

fn record_native_exit_placement(
    entry: &mut FullscreenWindow,
    seeded: Option<WindowedPlacement>,
    observed: WindowedPlacement,
) {
    // Native clients may acknowledge the windowed configure one commit before
    // attaching the resized buffer. At that point Space still reports the
    // fullscreen geometry. The pre-fullscreen placement is the configure's
    // authoritative endpoint; replacing it with the stale observed geometry
    // turns the exit motion into a same-size move followed by a cleanup snap.
    let placement = prefer_seeded_restore(seeded, Some(observed))
        .expect("the observed native placement is always available");
    entry.presentation_windowed = Some(placement.geometry);
    entry.restore = Some(placement);
    if !entry.preserve_stack {
        entry.presentation_output = None;
    }
}

fn can_update_external_restore(entry: &FullscreenWindow) -> bool {
    !entry.desired && entry.external_pending.is_none()
}

fn entry_occupies_output(entry: &FullscreenWindow, output: &str) -> bool {
    entry.target_output == output
        && (entry.active
            || entry.desired
            || entry.presented
            || entry.transition.is_some()
            || entry.external_pending.is_some())
}

fn settle_external_fullscreen(
    entry: &mut FullscreenWindow,
    target_output: &str,
    fullscreen_size: Size<i32, Logical>,
) {
    entry.desired = true;
    entry.active = true;
    entry.presented = true;
    entry.target_output = target_output.to_string();
    entry.fullscreen_size = fullscreen_size;
    entry.transition = None;
    entry.external_pending = None;
}

fn begin_external_transaction(
    entry: &mut FullscreenWindow,
    desired: bool,
    geometry: Rectangle<i32, Logical>,
    presentation: ExternalPresentationKind,
) -> ExternalTransactionRequest {
    if entry.desired == desired {
        return ExternalTransactionRequest::NoChange;
    }
    entry.desired = desired;
    entry.external_pending = Some(ExternalPending {
        geometry,
        presentation,
    });
    ExternalTransactionRequest::Configure(geometry)
}

fn acknowledge_external_transaction(
    entry: &mut FullscreenWindow,
    observed: Rectangle<i32, Logical>,
    animations: Animations,
    now: Duration,
) -> ExternalConfigureResult {
    let Some(pending) = entry.external_pending else {
        return ExternalConfigureResult::NotPending;
    };
    if observed != pending.geometry {
        return ExternalConfigureResult::Waiting;
    }
    entry.external_pending = None;
    let fullscreen = entry.desired;
    match pending.presentation {
        ExternalPresentationKind::Opening => {
            entry.active = fullscreen;
            entry.presented = fullscreen;
            entry.transition = None;
        }
        ExternalPresentationKind::Animated => {
            settle_visual_commit(entry, animations, now, fullscreen);
        }
    }
    ExternalConfigureResult::Settled {
        fullscreen,
        animated: entry.transition.is_some(),
    }
}

fn finish_external_transition(entry: &mut FullscreenWindow) {
    entry.external_pending = None;
    entry.active = entry.desired;
    entry.presented = entry.desired;
    entry.transition = None;
}

fn relocate_external_window(
    wayland: &mut WaylandState,
    window: &Window,
    entry: &FullscreenWindow,
) -> bool {
    let target = if entry.desired {
        let Some(output) = output_by_name(wayland, &entry.target_output) else {
            return false;
        };
        let Some(geometry) = wayland.space.output_geometry(&output) else {
            return false;
        };
        (output, geometry.loc)
    } else {
        let Some(restore) = entry.restore.as_ref() else {
            return false;
        };
        let Some(output) = restore
            .output
            .as_deref()
            .and_then(|name| output_by_name(wayland, name))
            .or_else(|| output_by_name(wayland, &entry.target_output))
        else {
            return false;
        };
        (output, restore.location)
    };
    super::set_window_output(window, &target.0);
    wayland.space.relocate_element(window, target.1);
    true
}

fn retarget_visual(
    entry: &mut FullscreenWindow,
    animations: Animations,
    now: Duration,
    presented: bool,
) {
    let (current, velocity) = entry
        .transition
        .map(|transition| (transition.value_at(now), transition.velocity_at(now)))
        .unwrap_or_else(|| (if entry.presented { 1.0 } else { 0.0 }, 0.0));
    if animations_enabled(animations) {
        entry.transition = Some(MotionTimeline::between(
            animations.fullscreen.motion,
            now,
            current,
            if presented { 1.0 } else { 0.0 },
            velocity,
        ));
    } else {
        entry.presented = presented;
        entry.transition = None;
    }
}

/// Retarget compositor-owned fullscreen immediately while retaining the
/// committed `active` state until the matching client buffer arrives. Rapid
/// reversals can therefore sample the in-flight position and velocity even
/// when the client has not acknowledged either configure yet.
fn begin_compositor_visual(entry: &mut FullscreenWindow, animations: Animations, now: Duration) {
    entry.snapshot_serials.clear();
    let target = if entry.desired { 1.0 } else { 0.0 };
    if (entry.desired != entry.active || entry.transition.is_some())
        && entry
            .transition
            .is_none_or(|transition| (transition.target() - target).abs() > f64::EPSILON)
    {
        retarget_visual(entry, animations, now, entry.desired);
    }
}

/// A compositor-owned timeline may already be running by the time the client
/// commits its configured size. In that case the commit settles geometry and
/// logical state without restarting the motion from the current point.
fn settle_visual_commit(
    entry: &mut FullscreenWindow,
    animations: Animations,
    now: Duration,
    desired: bool,
) {
    let target = if desired { 1.0 } else { 0.0 };
    if entry
        .transition
        .is_none_or(|transition| (transition.target() - target).abs() > f64::EPSILON)
    {
        retarget_visual(entry, animations, now, desired);
    }
    entry.active = desired;
}

fn send_required_configure(toplevel: &ToplevelSurface) -> Option<Serial> {
    if toplevel.is_initial_configure_sent() {
        return Some(toplevel.send_configure());
    }
    None
}

fn find_window<'a>(wayland: &'a WaylandState, surface: &WlSurface) -> Option<&'a Window> {
    wayland
        .space
        .elements()
        .find(|window| {
            window
                .wl_surface()
                .is_some_and(|candidate| candidate.as_ref() == surface)
        })
        .or_else(|| wayland.unmapped.get(surface))
}

fn output_by_name(wayland: &WaylandState, name: &str) -> Option<Output> {
    wayland
        .space
        .outputs()
        .find(|output| output.name() == name)
        .cloned()
}

fn center_in_rect(
    size: Size<i32, Logical>,
    location: Point<i32, Logical>,
    bounds: Size<i32, Logical>,
) -> Point<i32, Logical> {
    (
        location.x + (bounds.w - size.w) / 2,
        location.y + (bounds.h - size.h) / 2,
    )
        .into()
}

fn interpolate_rect(
    from: Rectangle<i32, Physical>,
    to: Rectangle<i32, Physical>,
    progress: f64,
) -> Rectangle<i32, Physical> {
    let interpolate =
        |from: i32, to: i32| (f64::from(from) + f64::from(to - from) * progress).round() as i32;
    Rectangle::new(
        (
            interpolate(from.loc.x, to.loc.x),
            interpolate(from.loc.y, to.loc.y),
        )
            .into(),
        (
            interpolate(from.size.w, to.size.w).max(0),
            interpolate(from.size.h, to.size.h).max(0),
        )
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use halley_config::{AnimationCurve, AnimationMotion, EasingMotion};

    use super::*;

    fn test_entry(active: bool) -> FullscreenWindow {
        FullscreenWindow {
            desired: active,
            active,
            presented: active,
            target_output: "DP-1".to_string(),
            restore: None,
            presentation_windowed: None,
            presentation_output: None,
            fullscreen_size: (1920, 1080).into(),
            transition: None,
            external_pending: None,
            snapshot_serials: Vec::new(),
            origin: FullscreenOrigin::Client,
            native: Some(NativeFullscreenState {
                client_requested: active,
                compositor_requested: false,
                protocol_desired: active,
                protocol_active: active,
            }),
            preserve_stack: false,
        }
    }

    #[test]
    fn centers_undersized_client_in_output() {
        assert_eq!(
            center_in_rect((1280, 720).into(), (1920, 0).into(), (2560, 1440).into()),
            (2560, 360).into()
        );
    }

    #[test]
    fn client_size_cannot_retarget_a_running_transition() {
        let motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let started = Duration::from_secs(1);
        let mut entry = test_entry(true);

        assert!(
            may_adopt_client_size(&entry, started),
            "a settled fullscreen still letterboxes an undersized client"
        );

        entry.transition = Some(MotionTimeline::between(motion, started, 0.0, 1.0, 0.0));
        assert!(!may_adopt_client_size(
            &entry,
            started + Duration::from_millis(200)
        ));
        assert!(may_adopt_client_size(
            &entry,
            started + Duration::from_millis(400)
        ));
    }

    #[test]
    fn global_blur_distinguishes_managed_and_client_fullscreen() {
        assert!(fullscreen_origin_allows_global_blur(
            FullscreenOrigin::Compositor
        ));
        assert!(fullscreen_origin_allows_global_blur(
            FullscreenOrigin::Maximize
        ));
        assert!(!fullscreen_origin_allows_global_blur(
            FullscreenOrigin::Client
        ));
    }

    #[test]
    fn maximize_presentation_reports_maximized_not_fullscreen() {
        let mut state = ToplevelState::default();
        apply_protocol_presentation_state(&mut state, FullscreenOrigin::Maximize, true);

        assert!(state.states.contains(State::Maximized));
        assert!(!state.states.contains(State::Fullscreen));

        apply_protocol_presentation_state(&mut state, FullscreenOrigin::Maximize, false);
        assert!(!state.states.contains(State::Maximized));
        assert!(!state.states.contains(State::Fullscreen));
    }

    #[test]
    fn explicit_fullscreen_still_reports_fullscreen() {
        let mut state = ToplevelState::default();
        apply_protocol_presentation_state(&mut state, FullscreenOrigin::Client, true);

        assert!(state.states.contains(State::Fullscreen));
        assert!(!state.states.contains(State::Maximized));
    }

    #[test]
    fn compositor_fullscreen_is_visual_only_until_the_client_requests_fullscreen() {
        let mut entry = test_entry(false);
        let target = entry.target_output.clone();
        let fullscreen_size = entry.fullscreen_size;

        request_native_owner(&mut entry, FullscreenOrigin::Compositor);
        let compositor_only = entry.native.expect("native state");
        assert!(compositor_only.compositor_requested);
        assert!(!compositor_only.client_requested);
        assert!(!compositor_only.protocol_desired);
        assert!(entry.desired);
        assert_eq!(entry.origin, FullscreenOrigin::Compositor);
        assert_eq!(
            fullscreen_commit_action(&entry, false),
            FullscreenCommitAction::Visual(true)
        );
        let mut pending = ToplevelState::default();
        apply_protocol_presentation_state(
            &mut pending,
            entry.origin,
            compositor_only.protocol_desired,
        );
        assert!(!pending.states.contains(State::Fullscreen));
        assert!(!pending.states.contains(State::Maximized));

        entry.active = true;
        entry.presented = true;

        request_native_owner(&mut entry, FullscreenOrigin::Client);
        let nested_set = entry.native.expect("native state");
        assert!(nested_set.compositor_requested);
        assert!(nested_set.client_requested);
        assert!(nested_set.protocol_desired);
        assert!(entry.desired);
        assert_eq!(entry.origin, FullscreenOrigin::Compositor);
        assert_eq!(
            fullscreen_commit_action(&entry, true),
            FullscreenCommitAction::ProtocolOnly
        );
        apply_protocol_presentation_state(&mut pending, entry.origin, nested_set.protocol_desired);
        assert!(pending.states.contains(State::Fullscreen));
        assert!(!pending.states.contains(State::Maximized));

        entry.native.as_mut().expect("native state").protocol_active = true;

        release_native_owner(&mut entry, FullscreenOrigin::Client);
        let nested_unset = entry.native.expect("native state");
        assert!(nested_unset.compositor_requested);
        assert!(!nested_unset.client_requested);
        assert!(!nested_unset.protocol_desired);
        assert!(entry.desired);
        assert!(entry.active);
        assert!(entry.presented);
        assert_eq!(entry.origin, FullscreenOrigin::Compositor);
        assert_eq!(entry.target_output, target);
        assert_eq!(entry.fullscreen_size, fullscreen_size);
        assert_eq!(
            fullscreen_commit_action(&entry, false),
            FullscreenCommitAction::ProtocolOnly
        );
    }

    #[test]
    fn nested_owners_can_be_released_in_either_order() {
        let mut compositor_first = test_entry(false);
        request_native_owner(&mut compositor_first, FullscreenOrigin::Compositor);
        request_native_owner(&mut compositor_first, FullscreenOrigin::Client);
        compositor_first.active = true;

        release_native_owner(&mut compositor_first, FullscreenOrigin::Compositor);
        let retained_client = compositor_first.native.expect("native state");
        assert!(!retained_client.compositor_requested);
        assert!(retained_client.client_requested);
        assert!(retained_client.protocol_desired);
        assert!(compositor_first.desired);
        assert_eq!(compositor_first.origin, FullscreenOrigin::Client);
        assert_eq!(
            fullscreen_commit_action(&compositor_first, true),
            FullscreenCommitAction::ProtocolOnly
        );

        release_native_owner(&mut compositor_first, FullscreenOrigin::Client);
        assert!(!compositor_first.desired);
        assert_eq!(
            fullscreen_commit_action(&compositor_first, false),
            FullscreenCommitAction::Visual(false)
        );

        let mut client_first = test_entry(false);
        request_native_owner(&mut client_first, FullscreenOrigin::Compositor);
        request_native_owner(&mut client_first, FullscreenOrigin::Client);
        client_first.active = true;

        release_native_owner(&mut client_first, FullscreenOrigin::Client);
        assert!(client_first.desired);
        assert_eq!(client_first.origin, FullscreenOrigin::Compositor);
        release_native_owner(&mut client_first, FullscreenOrigin::Compositor);
        assert!(!client_first.desired);
        assert_eq!(
            fullscreen_commit_action(&client_first, false),
            FullscreenCommitAction::Visual(false)
        );
    }

    #[test]
    fn client_owned_fullscreen_can_still_be_released_by_the_client() {
        let mut entry = test_entry(true);

        release_native_owner(&mut entry, FullscreenOrigin::Client);
        assert!(!entry.desired);
        assert_eq!(
            fullscreen_commit_action(&entry, false),
            FullscreenCommitAction::Visual(false)
        );
    }

    #[test]
    fn fullscreen_hides_top_layer_by_output_without_focus_dependency() {
        let entry = test_entry(true);
        assert!(entry_covers_top(&entry, "DP-1", Duration::ZERO));
        assert!(!entry_covers_top(&entry, "DP-2", Duration::ZERO));
    }

    #[test]
    fn output_occupancy_includes_transitional_fullscreen_windows() {
        let motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let mut entry = test_entry(false);
        entry.transition = Some(MotionTimeline::between(
            motion,
            Duration::ZERO,
            1.0,
            0.0,
            0.0,
        ));

        assert!(entry_occupies_output(&entry, "DP-1"));
        assert!(!entry_occupies_output(&entry, "DP-2"));

        entry.transition = None;
        assert!(!entry_occupies_output(&entry, "DP-1"));
    }

    #[test]
    fn local_killswitch_disables_visual_motion() {
        let mut animations = Animations::default();
        animations.fullscreen.enabled = false;
        assert!(!animations_enabled(animations));
    }

    #[test]
    fn fullscreen_motion_retargets_without_discontinuity() {
        let mut animations = Animations::default();
        animations.fullscreen.motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let mut entry = test_entry(false);
        let started = Duration::from_secs(1);

        retarget_visual(&mut entry, animations, started, true);
        let forward = entry.transition.expect("forward transition");
        let reversed_at = started + Duration::from_millis(100);
        let value_before_reverse = forward.value_at(reversed_at);

        retarget_visual(&mut entry, animations, reversed_at, false);
        let reverse = entry.transition.expect("reverse transition");

        assert!(!entry.active);
        assert!((reverse.value_at(reversed_at) - value_before_reverse).abs() < f64::EPSILON);
        assert_eq!(
            reverse.value_at(reversed_at + Duration::from_millis(400)),
            0.0
        );
    }

    #[test]
    fn compositor_fullscreen_starts_before_the_configure_commit() {
        let mut animations = Animations::default();
        animations.fullscreen.motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let mut entry = test_entry(false);
        entry.snapshot_serials.push(Serial::from(7));
        let started = Duration::from_secs(1);

        request_native_owner(&mut entry, FullscreenOrigin::Compositor);
        begin_compositor_visual(&mut entry, animations, started);

        assert!(entry.desired);
        assert!(!entry.active, "geometry remains commit-gated");
        assert!(entry.snapshot_serials.is_empty());
        assert_eq!(
            entry
                .transition
                .expect("visual starts at input time")
                .value_at(started + Duration::from_millis(100)),
            0.25
        );
    }

    #[test]
    fn rapid_compositor_fullscreen_reversal_keeps_motion_continuous() {
        let mut animations = Animations::default();
        animations.fullscreen.motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let mut entry = test_entry(false);
        let started = Duration::from_secs(1);

        request_native_owner(&mut entry, FullscreenOrigin::Compositor);
        begin_compositor_visual(&mut entry, animations, started);
        let reversed_at = started + Duration::from_millis(100);
        let before = entry
            .transition
            .expect("forward transition")
            .value_at(reversed_at);

        release_native_owner(&mut entry, FullscreenOrigin::Compositor);
        begin_compositor_visual(&mut entry, animations, reversed_at);
        let reverse = entry.transition.expect("reverse transition");

        assert!(!entry.desired);
        assert!(!entry.active);
        assert!((reverse.value_at(reversed_at) - before).abs() < f64::EPSILON);
        assert_eq!(
            reverse.value_at(reversed_at + Duration::from_millis(400)),
            0.0
        );
    }

    #[test]
    fn compositor_configure_commit_does_not_restart_running_motion() {
        let mut animations = Animations::default();
        animations.fullscreen.motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let mut entry = test_entry(false);
        let started = Duration::from_secs(1);

        request_native_owner(&mut entry, FullscreenOrigin::Compositor);
        begin_compositor_visual(&mut entry, animations, started);
        let committed_at = started + Duration::from_millis(100);
        let expected = entry
            .transition
            .expect("input-time transition")
            .value_at(started + Duration::from_millis(200));

        settle_visual_commit(&mut entry, animations, committed_at, true);

        assert!(entry.active);
        assert_eq!(
            entry
                .transition
                .expect("commit retains input-time transition")
                .value_at(started + Duration::from_millis(200)),
            expected
        );
    }

    #[test]
    fn client_fullscreen_remains_commit_gated() {
        let animations = Animations::default();
        let mut entry = test_entry(false);

        request_native_owner(&mut entry, FullscreenOrigin::Client);
        assert!(entry.transition.is_none());
        assert!(!entry.active);

        settle_visual_commit(&mut entry, animations, Duration::ZERO, true);
        assert!(entry.active);
        assert!(entry.transition.is_some());
    }

    #[test]
    fn fullscreen_transition_owns_the_zero_progress_handoff_frame() {
        assert!(fullscreen_presentation_is_visible(0.0, true));
        assert!(fullscreen_presentation_is_visible(0.5, true));
        assert!(fullscreen_presentation_is_visible(1.0, false));
        assert!(!fullscreen_presentation_is_visible(0.0, false));
    }

    #[test]
    fn pending_configure_keeps_the_outgoing_source_presented() {
        let mut entry = test_entry(false);
        entry.desired = true;

        assert!(
            entry_owns_presentation(&entry),
            "the handoff source must survive until the client acknowledges fullscreen"
        );

        entry.active = true;
        assert!(!entry_owns_presentation(&entry));
    }

    #[test]
    fn fullscreen_motion_killswitch_still_applies_state() {
        let mut animations = Animations::default();
        animations.fullscreen.enabled = false;
        let mut entry = test_entry(false);

        retarget_visual(&mut entry, animations, Duration::ZERO, true);

        assert!(!entry.active);
        assert!(entry.presented);
        assert!(entry.transition.is_none());
    }

    #[test]
    fn external_fullscreen_is_logically_settled_without_animation() {
        let animations = Animations::default();
        let mut entry = test_entry(false);
        retarget_visual(&mut entry, animations, Duration::from_secs(1), true);

        settle_external_fullscreen(&mut entry, "HDMI-A-1", (2560, 1440).into());

        assert!(entry.desired);
        assert!(entry.active);
        assert_eq!(entry.target_output, "HDMI-A-1");
        assert_eq!(entry.fullscreen_size, Size::from((2560, 1440)));
        assert!(entry.transition.is_none());
    }

    #[test]
    fn external_mod_f_starts_before_x11_geometry_and_does_not_restart() {
        let mut animations = Animations::default();
        animations.fullscreen.motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let mut entry = test_entry(false);
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let intermediate = Rectangle::new((0, 0).into(), (1280, 720).into());
        let started = Duration::from_secs(1);

        assert_eq!(
            begin_external_transaction(
                &mut entry,
                true,
                target,
                ExternalPresentationKind::Animated,
            ),
            ExternalTransactionRequest::Configure(target)
        );
        begin_compositor_visual(&mut entry, animations, started);
        let expected = entry
            .transition
            .expect("input-time transition")
            .value_at(started + Duration::from_millis(200));
        assert_eq!(
            acknowledge_external_transaction(&mut entry, intermediate, animations, started),
            ExternalConfigureResult::Waiting
        );
        assert!(!entry.active);

        assert_eq!(
            acknowledge_external_transaction(
                &mut entry,
                target,
                animations,
                started + Duration::from_millis(100),
            ),
            ExternalConfigureResult::Settled {
                fullscreen: true,
                animated: true,
            }
        );
        assert!(entry.active);
        assert_eq!(
            entry
                .transition
                .expect("configure retains input-time transition")
                .value_at(started + Duration::from_millis(200)),
            expected
        );
        assert!(entry.external_pending.is_none());
    }

    #[test]
    fn client_external_fullscreen_remains_geometry_gated() {
        let animations = Animations::default();
        let mut entry = test_entry(false);
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());

        begin_external_transaction(&mut entry, true, target, ExternalPresentationKind::Animated);

        assert!(entry.transition.is_none());
        assert!(!entry.active);
        assert_eq!(
            acknowledge_external_transaction(&mut entry, target, animations, Duration::ZERO),
            ExternalConfigureResult::Settled {
                fullscreen: true,
                animated: true,
            }
        );
        assert!(entry.active);
        assert!(entry.transition.is_some());
    }

    #[test]
    fn duplicate_external_request_preserves_the_active_transaction() {
        let mut entry = test_entry(false);
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        begin_external_transaction(&mut entry, true, target, ExternalPresentationKind::Animated);
        let pending = entry.external_pending;

        assert!(desired_matches(Some(&entry), true));
        assert!(!desired_matches(Some(&entry), false));
        assert!(!desired_matches(None, true));
        assert_eq!(entry.external_pending, pending);
    }

    #[test]
    fn chrome_follows_logical_fullscreen_state_instead_of_visual_progress() {
        let mut entry = test_entry(false);
        assert!(!fullscreen_entry_suppresses_chrome(&entry));

        entry.desired = true;
        assert!(fullscreen_entry_suppresses_chrome(&entry));

        entry.origin = FullscreenOrigin::Compositor;
        assert!(fullscreen_entry_suppresses_chrome(&entry));

        entry.origin = FullscreenOrigin::Maximize;
        assert!(!fullscreen_entry_suppresses_chrome(&entry));

        entry.origin = FullscreenOrigin::Client;
        entry.desired = false;
        entry.active = true;
        entry.presented = true;
        assert!(!fullscreen_entry_suppresses_chrome(&entry));
    }

    #[test]
    fn settled_windowed_resize_can_replace_the_saved_restore() {
        let mut entry = test_entry(false);
        assert!(can_update_external_restore(&entry));

        entry.desired = true;
        assert!(!can_update_external_restore(&entry));

        entry.desired = false;
        entry.external_pending = Some(ExternalPending {
            geometry: Rectangle::new((0, 0).into(), (1920, 1080).into()),
            presentation: ExternalPresentationKind::Animated,
        });
        assert!(!can_update_external_restore(&entry));
    }

    #[test]
    fn external_state_churn_keeps_only_the_latest_geometry() {
        let mut entry = test_entry(false);
        let fullscreen = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let restore = Rectangle::new((320, 180).into(), (1280, 720).into());

        begin_external_transaction(
            &mut entry,
            true,
            fullscreen,
            ExternalPresentationKind::Opening,
        );
        begin_external_transaction(
            &mut entry,
            false,
            restore,
            ExternalPresentationKind::Opening,
        );
        begin_external_transaction(
            &mut entry,
            true,
            fullscreen,
            ExternalPresentationKind::Opening,
        );

        assert_eq!(
            entry.external_pending,
            Some(ExternalPending {
                geometry: fullscreen,
                presentation: ExternalPresentationKind::Opening,
            })
        );
        assert!(entry.desired);
        assert!(entry.transition.is_none());
    }

    #[test]
    fn opening_transaction_settles_without_a_second_animation() {
        let animations = Animations::default();
        let mut entry = test_entry(false);
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());

        begin_external_transaction(&mut entry, true, target, ExternalPresentationKind::Opening);

        assert_eq!(
            acknowledge_external_transaction(&mut entry, target, animations, Duration::ZERO),
            ExternalConfigureResult::Settled {
                fullscreen: true,
                animated: false,
            }
        );
        assert!(entry.active);
        assert!(entry.transition.is_none());
    }

    #[test]
    fn finishing_external_animation_snaps_to_desired_state() {
        let animations = Animations::default();
        let mut entry = test_entry(false);
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        begin_external_transaction(&mut entry, true, target, ExternalPresentationKind::Animated);
        acknowledge_external_transaction(&mut entry, target, animations, Duration::from_secs(1));
        assert!(entry.transition.is_some());

        finish_external_transition(&mut entry);

        assert!(entry.active);
        assert!(entry.desired);
        assert!(entry.external_pending.is_none());
        assert!(entry.transition.is_none());
    }

    #[test]
    fn seeded_restore_geometry_wins_over_buffered_fullscreen_geometry() {
        let seeded_geometry = Rectangle::new((960, 480).into(), (640, 480).into());
        let buffered_geometry = Rectangle::new((0, 0).into(), (2560, 1440).into());
        let seeded = WindowedPlacement {
            location: seeded_geometry.loc,
            geometry: seeded_geometry,
            output: Some("DP-1".to_string()),
        };
        let buffered = WindowedPlacement {
            location: buffered_geometry.loc,
            geometry: buffered_geometry,
            output: Some("DP-1".to_string()),
        };

        let fallback =
            prefer_seeded_restore(None, Some(buffered.clone())).expect("buffered fallback");
        let restore = prefer_seeded_restore(Some(seeded), Some(buffered)).expect("seeded restore");

        assert_eq!(fallback.geometry, buffered_geometry);
        assert_eq!(restore.geometry, seeded_geometry);
        assert_eq!(restore.location, seeded_geometry.loc);
    }

    #[test]
    fn native_exit_ack_keeps_windowed_endpoint_when_buffer_is_still_fullscreen() {
        let windowed_geometry = Rectangle::new((960, 480).into(), (640, 480).into());
        let fullscreen_geometry = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let windowed = WindowedPlacement {
            location: windowed_geometry.loc,
            geometry: windowed_geometry,
            output: Some("DP-1".to_string()),
        };
        let observed = WindowedPlacement {
            location: windowed_geometry.loc,
            geometry: fullscreen_geometry,
            output: Some("DP-1".to_string()),
        };
        let mut entry = test_entry(true);
        entry.desired = false;

        record_native_exit_placement(&mut entry, Some(windowed), observed);

        assert_eq!(entry.restore.as_ref().unwrap().geometry, windowed_geometry);
        assert_eq!(entry.presentation_windowed, Some(windowed_geometry));
    }
}
