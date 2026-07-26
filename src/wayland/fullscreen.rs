use std::collections::HashMap;

use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::camera::OutputCameras;

use super::WaylandState;

#[derive(Clone, Debug)]
struct WindowedPlacement {
    location: Point<i32, Logical>,
    size: Size<i32, Logical>,
    output: Option<String>,
}

#[derive(Debug)]
struct FullscreenWindow {
    desired: bool,
    active: bool,
    target_output: String,
    restore: Option<WindowedPlacement>,
}

pub struct FullscreenManager {
    windows: HashMap<WlSurface, FullscreenWindow>,
}

impl FullscreenManager {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    pub fn request(
        &mut self,
        wayland: &mut WaylandState,
        toplevel: &ToplevelSurface,
        requested: Option<WlOutput>,
    ) {
        let window = find_window(wayland, toplevel.wl_surface()).cloned();
        let requested_output = requested
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|output| wayland.space.outputs().any(|known| known == output));
        let target = requested_output
            .or_else(|| {
                window
                    .as_ref()
                    .and_then(super::window_output_name)
                    .and_then(|name| output_by_name(wayland, &name))
            })
            .or_else(|| super::focus::selected_output(wayland).cloned());

        let Some(target) = target else {
            if toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
            return;
        };
        let Some(output_geometry) = wayland.space.output_geometry(&target) else {
            if toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
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
                    wayland
                        .space
                        .element_location(window)
                        .map(|location| WindowedPlacement {
                            location,
                            size: window.geometry().size,
                            output: super::window_output_name(window),
                        })
                }),
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
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }

    pub fn unrequest(&mut self, wayland: &WaylandState, toplevel: &ToplevelSurface) {
        let restore_size = self
            .windows
            .get_mut(toplevel.wl_surface())
            .and_then(|entry| {
                entry.desired = false;
                entry.restore.as_ref().map(|restore| restore.size)
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
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }

    pub fn handle_commit(
        &mut self,
        wayland: &mut WaylandState,
        cameras: &OutputCameras,
        surface: &WlSurface,
    ) -> bool {
        let Some(entry) = self.windows.get_mut(surface) else {
            return false;
        };
        let Some(window) = find_window(wayland, surface).cloned() else {
            return false;
        };
        let Some(toplevel) = window.toplevel() else {
            return false;
        };
        let committed = toplevel.with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(State::Fullscreen))
        });
        if committed != entry.desired || committed == entry.active {
            return false;
        }

        let Some(output) = output_by_name(wayland, &entry.target_output) else {
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
            wayland.space.map_element(window, location, true);
        } else {
            let location = match entry.restore.as_ref() {
                Some(restore)
                    if restore.output.as_deref() == Some(entry.target_output.as_str()) =>
                {
                    restore.location
                }
                _ => super::xdg_shell::centered_location(wayland, cameras, &output, &window),
            };
            super::set_window_output(&window, &output);
            wayland.space.map_element(window, location, true);
        }
        entry.active = committed;
        true
    }

    pub fn is_fullscreen_or_pending(&self, surface: &WlSurface) -> bool {
        self.windows
            .get(surface)
            .is_some_and(|entry| entry.active || entry.desired)
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
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
}
