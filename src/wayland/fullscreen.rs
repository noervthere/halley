use std::collections::HashMap;
use std::time::Duration;

use halley_config::Animations;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::animation::MotionTimeline;
use crate::camera::OutputCameras;

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
    target_output: String,
    restore: Option<WindowedPlacement>,
    fullscreen_size: Size<i32, Logical>,
    transition: Option<MotionTimeline>,
}

#[derive(Clone, Copy, Debug)]
pub struct FullscreenPresentation {
    pub progress: f64,
    pub windowed_geometry: Option<Rectangle<i32, Logical>>,
    pub fullscreen_size: Size<i32, Logical>,
}

impl FullscreenPresentation {
    pub fn fullscreen_rect(
        self,
        output_size: Size<i32, Physical>,
    ) -> Rectangle<i32, Physical> {
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

    pub fn reload(&mut self, animations: Animations) {
        self.animations = animations;
    }

    pub fn request(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
        requested: Option<WlOutput>,
    ) {
        let window = find_window(wayland, toplevel.wl_surface()).cloned();
        let requested = requested.filter(|resource| {
            Output::from_resource(resource)
                .is_some_and(|output| wayland.space.outputs().any(|known| known == &output))
        });
        let requested_output = requested
            .as_ref()
            .and_then(Output::from_resource);
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
                desired: true,
                active: false,
                target_output: target.name(),
                restore: window.as_ref().and_then(|window| {
                    Some(WindowedPlacement {
                        location: wayland.space.element_location(window)?,
                        geometry: wayland.space.element_geometry(window)?,
                        output: super::window_output_name(window),
                    })
                }),
                fullscreen_size: output_geometry.size,
                transition: None,
            });
        entry.desired = true;
        entry.target_output = target.name();

        toplevel.with_pending_state(|state| {
            state.states.set(State::Fullscreen);
            state.states.unset(State::Maximized);
            super::decoration::clear_tiled_hint(state);
            state.size = Some(output_geometry.size);
            state.bounds = Some(output_geometry.size);
            state.fullscreen_output = requested;
        });
        send_required_configure(toplevel);
    }

    pub fn unrequest(&mut self, wayland: &WaylandState, toplevel: &ToplevelSurface) {
        let restore_size = self
            .windows
            .get_mut(toplevel.wl_surface())
            .and_then(|entry| {
                entry.desired = false;
                if let Some(window) = find_window(wayland, toplevel.wl_surface()) {
                    entry.fullscreen_size = window.geometry().size;
                }
                entry.restore.as_ref().map(|restore| restore.geometry.size)
            });
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
        send_required_configure(toplevel);
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
            self.windows
                .get_mut(surface)
                .expect("entry checked above")
                .fullscreen_size = size;
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
                _ => super::xdg_shell::centered_location(wayland, cameras, &output, &window),
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
        }
        let (current, velocity) = entry
            .transition
            .map(|transition| (transition.value_at(now), transition.velocity_at(now)))
            .unwrap_or_else(|| (if entry.active { 1.0 } else { 0.0 }, 0.0));
        entry.active = committed;
        if committed {
            entry.fullscreen_size = window.geometry().size;
        }
        entry.transition = animations_enabled(self.animations).then(|| {
            MotionTimeline::between(
                self.animations.fullscreen.motion,
                now,
                current,
                if committed { 1.0 } else { 0.0 },
                velocity,
            )
        });
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
            .unwrap_or_else(|| if entry.active { 1.0 } else { 0.0 })
            .clamp(0.0, 1.0);
        (progress > 0.0).then_some(FullscreenPresentation {
            progress,
            windowed_geometry: entry.restore.as_ref().map(|restore| restore.geometry),
            fullscreen_size: entry.fullscreen_size,
        })
    }

    pub fn covers_top(
        &self,
        focused: Option<&WlSurface>,
        output: &Output,
        now: Duration,
    ) -> bool {
        focused
            .and_then(|surface| self.windows.get(surface))
            .is_some_and(|entry| {
                entry.target_output == output.name()
                    && entry.active
                    && entry
                        .transition
                        .is_none_or(|transition| transition.is_finished_at(now))
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
        self.windows.values().any(|entry| {
            entry.target_output == output.name()
                && entry
                    .transition
                    .is_some_and(|transition| !transition.is_finished_at(now))
        })
    }

    pub fn is_fullscreen_or_pending(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_some_and(|entry| entry.active || entry.desired)
    }

    pub fn reconfigure_output(&self, wayland: &WaylandState, output: &Output) {
        let Some(geometry) = wayland.space.output_geometry(output) else {
            return;
        };
        for (surface, entry) in &self.windows {
            if entry.target_output != output.name() || !(entry.active || entry.desired) {
                continue;
            }
            let Some(window) = find_window(wayland, surface) else {
                continue;
            };
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            toplevel.with_pending_state(|state| {
                state.states.set(State::Fullscreen);
                state.size = Some(geometry.size);
                state.bounds = Some(geometry.size);
                super::decoration::clear_tiled_hint(state);
            });
            toplevel.send_configure();
        }
    }

    pub fn cleanup(&mut self, now: Duration) -> bool {
        let mut finished = false;
        self.windows.retain(|_, entry| {
            if entry
                .transition
                .is_some_and(|transition| transition.is_finished_at(now))
            {
                entry.transition = None;
                finished = true;
            }
            entry.active || entry.desired || entry.transition.is_some()
        });
        finished
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
    }
}

fn animations_enabled(animations: Animations) -> bool {
    animations.enabled && animations.fullscreen.enabled
}

fn send_required_configure(toplevel: &ToplevelSurface) {
    if toplevel.is_initial_configure_sent() {
        toplevel.send_configure();
    }
}

fn find_window<'a>(wayland: &'a WaylandState, surface: &WlSurface) -> Option<&'a Window> {
    wayland
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface)
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
    let interpolate = |from: i32, to: i32| {
        (f64::from(from) + f64::from(to - from) * progress).round() as i32
    };
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
    use super::*;

    #[test]
    fn centers_undersized_client_in_output() {
        assert_eq!(
            center_in_rect((1280, 720).into(), (1920, 0).into(), (2560, 1440).into()),
            (2560, 360).into()
        );
    }

    #[test]
    fn local_killswitch_disables_visual_motion() {
        let mut animations = Animations::default();
        animations.fullscreen.enabled = false;
        assert!(!animations_enabled(animations));
    }
}
