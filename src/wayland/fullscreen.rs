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
use smithay::wayland::shell::xdg::ToplevelSurface;

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
    fullscreen_size: Size<i32, Logical>,
    transition: Option<MotionTimeline>,
    external_pending: Option<ExternalPending>,
    snapshot_serials: Vec<Serial>,
    origin: FullscreenOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullscreenOrigin {
    Client,
    Compositor,
    Maximize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalPresentationKind {
    Opening,
    Animated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaximizeToggleAction {
    Enter,
    Exit,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExternalPending {
    geometry: Rectangle<i32, Logical>,
    presentation: ExternalPresentationKind,
}

#[derive(Clone, Copy, Debug)]
pub struct FullscreenPresentation {
    pub progress: f64,
    pub transition_completion: f64,
    pub windowed_geometry: Option<Rectangle<i32, Logical>>,
    pub fullscreen_size: Size<i32, Logical>,
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
    ) {
        self.request_with_origin(wayland, toplevel, None, FullscreenOrigin::Compositor);
    }

    pub(crate) fn toggle_maximize(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
    ) -> bool {
        match maximize_toggle_action(self.windows.get(toplevel.wl_surface())) {
            MaximizeToggleAction::Enter => {
                self.request_with_origin(wayland, toplevel, None, FullscreenOrigin::Maximize);
                true
            }
            MaximizeToggleAction::Exit => {
                self.unrequest(wayland, toplevel);
                true
            }
            MaximizeToggleAction::Ignore => false,
        }
    }

    fn request_with_origin(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
        requested: Option<WlOutput>,
        origin: FullscreenOrigin,
    ) {
        let window = find_window(wayland, toplevel.wl_surface()).cloned();
        let requested = requested.filter(|resource| {
            Output::from_resource(resource)
                .is_some_and(|output| wayland.space.outputs().any(|known| known == &output))
        });
        let requested_output = requested.as_ref().and_then(Output::from_resource);
        let target = requested_output
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
                fullscreen_size: output_geometry.size,
                transition: None,
                external_pending: None,
                snapshot_serials: Vec::new(),
                origin,
            });
        let transition_requested = !entry.active;
        entry.desired = true;
        entry.target_output = target.name();
        entry.origin = origin;
        // The destination is the size we are about to configure, decided once
        // here, exactly like field maximize decides its target rect at toggle
        // time. `handle_commit` only re-reads the client's committed size once
        // the transition has settled, to letterbox a client that stays smaller.
        entry.fullscreen_size = output_geometry.size;

        toplevel.with_pending_state(|state| {
            state.states.set(State::Fullscreen);
            state.states.unset(State::Maximized);
            super::decoration::clear_tiled_hint(state);
            state.size = Some(output_geometry.size);
            state.bounds = Some(output_geometry.size);
            state.fullscreen_output = requested;
        });
        if let Some(serial) = send_required_configure(toplevel)
            && animations_enabled(self.animations)
            && transition_requested
        {
            entry.snapshot_serials.push(serial);
        }
    }

    pub(crate) fn unrequest_maximize(
        &mut self,
        wayland: &WaylandState,
        toplevel: &ToplevelSurface,
    ) -> bool {
        if !self
            .windows
            .get(toplevel.wl_surface())
            .is_some_and(|entry| entry.origin == FullscreenOrigin::Maximize)
        {
            return false;
        }
        self.unrequest(wayland, toplevel);
        true
    }

    pub fn unrequest(&mut self, wayland: &WaylandState, toplevel: &ToplevelSurface) {
        let (restore_size, transition_requested) = self
            .windows
            .get_mut(toplevel.wl_surface())
            .map(|entry| {
                entry.desired = false;
                if let Some(window) = find_window(wayland, toplevel.wl_surface()) {
                    entry.fullscreen_size = window.geometry().size;
                }
                entry.presentation_windowed =
                    entry.restore.as_ref().map(|restore| restore.geometry);
                (
                    entry.restore.as_ref().map(|restore| restore.geometry.size),
                    entry.active,
                )
            })
            .unwrap_or((None, false));
        let bounds = self
            .windows
            .get(toplevel.wl_surface())
            .and_then(|entry| output_by_name(wayland, &entry.target_output))
            .and_then(|output| wayland.space.output_geometry(&output))
            .map(|geometry| geometry.size);

        toplevel.with_pending_state(|state| {
            state.states.unset(State::Fullscreen);
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
        if self.windows.remove(toplevel.wl_surface()).is_none() {
            return;
        }
        toplevel.with_pending_state(|state| {
            state.states.unset(State::Fullscreen);
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
                fullscreen_size: output_geometry.size,
                transition: None,
                external_pending: None,
                snapshot_serials: Vec::new(),
                origin,
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
    ) -> Option<ExternalTransactionRequest> {
        self.request_external_transaction(
            wayland,
            window,
            ExternalPresentationKind::Animated,
            None,
            origin,
        )
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
                fullscreen_size: output_geometry.size,
                transition: None,
                external_pending: None,
                snapshot_serials: Vec::new(),
                origin,
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
    ) -> Option<ExternalTransactionRequest> {
        self.unrequest_external_transaction(window, ExternalPresentationKind::Animated)
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
        let committed = toplevel.with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(State::Fullscreen))
        });
        let Some(entry) = self.windows.get(surface) else {
            return false;
        };
        if committed != entry.desired {
            return false;
        }
        if committed == entry.active {
            if !committed {
                return false;
            }
            let target_output = entry.target_output.clone();
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

        let target_output = entry.target_output.clone();
        let restore = entry.restore.clone();
        let Some(output) = output_by_name(wayland, &target_output) else {
            return false;
        };
        let Some(output_geometry) = wayland.space.output_geometry(&output) else {
            return false;
        };

        if committed {
            super::set_window_output(&window, &output);
            let location = center_in_rect(
                window.geometry().size,
                output_geometry.loc,
                output_geometry.size,
            );
            wayland.space.map_element(window.clone(), location, true);
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
            wayland.space.map_element(window.clone(), location, true);
        }

        let entry = self.windows.get_mut(surface).expect("entry checked above");
        if !committed
            && let (Some(location), Some(geometry)) = (
                wayland.space.element_location(&window),
                wayland.space.element_geometry(&window),
            )
        {
            entry.restore = Some(WindowedPlacement {
                location,
                geometry,
                output: Some(output.name()),
            });
            entry.presentation_windowed = Some(geometry);
        }
        retarget_visual(entry, self.animations, now, committed);
        entry.active = committed;
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
        fullscreen_presentation_is_visible(progress, entry.transition.is_some()).then_some(
            FullscreenPresentation {
                progress,
                transition_completion,
                windowed_geometry: entry
                    .presentation_windowed
                    .or_else(|| entry.restore.as_ref().map(|restore| restore.geometry)),
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
        let entry = self.windows.values().find(|entry| {
            entry.target_output == output.name()
                && (entry.active
                    || entry.desired
                    || entry.presented
                    || entry.transition.is_some()
                    || entry.external_pending.is_some())
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
        Some(FullscreenCameraFrame { center, progress })
    }

    pub fn covers_top(&self, _focused: Option<&WlSurface>, output: &Output, now: Duration) -> bool {
        self.windows
            .values()
            .any(|entry| entry_covers_top(entry, &output.name(), now))
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
        self.windows.values().any(|entry| {
            entry.target_output == output.name()
                && entry
                    .transition
                    .is_some_and(|transition| !transition.is_finished_at(now))
        })
    }

    pub fn has_stable_fullscreen_on_output(&self, output: &Output, now: Duration) -> bool {
        self.windows.values().any(|entry| {
            entry.target_output == output.name()
                && entry.origin != FullscreenOrigin::Maximize
                && entry.desired
                && entry.active
                && (entry.presented
                    || entry
                        .transition
                        .is_some_and(|transition| transition.is_finished_at(now)))
                && entry.external_pending.is_none()
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

    pub fn is_fullscreen_or_pending(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_some_and(|entry| entry.active || entry.desired)
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
    ) {
        if let Some(entry) = self.windows.get_mut(surface) {
            entry.restore = Some(WindowedPlacement {
                location: restore_geometry.loc,
                geometry: restore_geometry,
                output: Some(restore_output),
            });
            entry.presentation_windowed = Some(field_geometry);
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
                toplevel.with_pending_state(|state| {
                    state.states.set(State::Fullscreen);
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

fn fullscreen_presentation_is_visible(progress: f64, transition_active: bool) -> bool {
    // A transition owns the handoff even at its exact zero endpoint. Dropping
    // presentation there exposes the client's newly configured live buffer for
    // one frame before the captured texture blend takes over (and once again
    // on exit immediately before cleanup), which reads as a fullscreen flash.
    transition_active || progress > 0.0
}

fn fullscreen_origin_allows_global_blur(origin: FullscreenOrigin) -> bool {
    origin != FullscreenOrigin::Client
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

fn maximize_toggle_action(entry: Option<&FullscreenWindow>) -> MaximizeToggleAction {
    match entry {
        Some(entry) if entry.origin == FullscreenOrigin::Maximize && entry.desired => {
            MaximizeToggleAction::Exit
        }
        Some(entry)
            if entry.origin != FullscreenOrigin::Maximize
                && (entry.active
                    || entry.desired
                    || entry.transition.is_some()
                    || entry.external_pending.is_some()) =>
        {
            MaximizeToggleAction::Ignore
        }
        _ => MaximizeToggleAction::Enter,
    }
}

fn prefer_seeded_restore(
    seeded: Option<WindowedPlacement>,
    current: Option<WindowedPlacement>,
) -> Option<WindowedPlacement> {
    seeded.or(current)
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
            retarget_visual(entry, animations, now, fullscreen);
            entry.active = fullscreen;
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
            fullscreen_size: (1920, 1080).into(),
            transition: None,
            external_pending: None,
            snapshot_serials: Vec::new(),
            origin: FullscreenOrigin::Client,
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
    fn maximize_button_toggles_only_its_own_fullscreen() {
        assert_eq!(maximize_toggle_action(None), MaximizeToggleAction::Enter);

        let mut maximize = test_entry(true);
        maximize.origin = FullscreenOrigin::Maximize;
        assert_eq!(
            maximize_toggle_action(Some(&maximize)),
            MaximizeToggleAction::Exit
        );

        maximize.desired = false;
        assert_eq!(
            maximize_toggle_action(Some(&maximize)),
            MaximizeToggleAction::Enter,
            "pressing again during exit reverses the toggle"
        );

        let client = test_entry(true);
        assert_eq!(
            maximize_toggle_action(Some(&client)),
            MaximizeToggleAction::Ignore,
            "maximize must not take ownership of client fullscreen"
        );
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
    fn fullscreen_transition_owns_the_zero_progress_handoff_frame() {
        assert!(fullscreen_presentation_is_visible(0.0, true));
        assert!(fullscreen_presentation_is_visible(0.5, true));
        assert!(fullscreen_presentation_is_visible(1.0, false));
        assert!(!fullscreen_presentation_is_visible(0.0, false));
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
    fn external_animation_waits_for_matching_geometry_and_settles_once() {
        let animations = Animations::default();
        let mut entry = test_entry(false);
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let intermediate = Rectangle::new((0, 0).into(), (1280, 720).into());

        assert_eq!(
            begin_external_transaction(
                &mut entry,
                true,
                target,
                ExternalPresentationKind::Animated,
            ),
            ExternalTransactionRequest::Configure(target)
        );
        assert_eq!(
            acknowledge_external_transaction(&mut entry, intermediate, animations, Duration::ZERO,),
            ExternalConfigureResult::Waiting
        );
        assert!(entry.transition.is_none());
        assert_eq!(
            acknowledge_external_transaction(
                &mut entry,
                target,
                animations,
                Duration::from_secs(1),
            ),
            ExternalConfigureResult::Settled {
                fullscreen: true,
                animated: true
            }
        );
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
    fn seeded_restore_geometry_wins_over_the_buffered_fullscreen_size() {
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
    fn settled_windowed_resize_can_replace_the_saved_restore() {
        let mut entry = test_entry(false);
        assert!(can_update_external_restore(&entry));

        entry.desired = true;
        assert!(!can_update_external_restore(&entry));

        entry.desired = false;
        entry.external_pending = Some(ExternalPending {
            geometry: Rectangle::new((320, 180).into(), (1280, 720).into()),
            presentation: ExternalPresentationKind::Animated,
        });
        assert!(!can_update_external_restore(&entry));
    }

    #[test]
    fn external_state_churn_keeps_only_the_latest_geometry() {
        let mut entry = test_entry(false);
        let fullscreen = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let restore = Rectangle::new((320, 180).into(), (1280, 720).into());

        assert_eq!(
            begin_external_transaction(
                &mut entry,
                true,
                fullscreen,
                ExternalPresentationKind::Opening,
            ),
            ExternalTransactionRequest::Configure(fullscreen)
        );
        assert_eq!(
            begin_external_transaction(
                &mut entry,
                false,
                restore,
                ExternalPresentationKind::Opening,
            ),
            ExternalTransactionRequest::Configure(restore)
        );
        assert_eq!(
            begin_external_transaction(
                &mut entry,
                true,
                fullscreen,
                ExternalPresentationKind::Opening,
            ),
            ExternalTransactionRequest::Configure(fullscreen)
        );
        assert_eq!(
            entry.external_pending,
            Some(ExternalPending {
                geometry: fullscreen,
                presentation: ExternalPresentationKind::Opening
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
                animated: false
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
}
