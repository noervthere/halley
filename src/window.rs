use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Size};
use smithay::wayland::seat::WaylandFocus;

use crate::camera::OutputCameras;
use crate::wayland::WaylandState;
use crate::window::lifecycle::{MapTransition, Placement};

pub(crate) mod lifecycle;

pub fn focus_and_raise(wayland: &mut WaylandState, window: &Window) {
    wayland.focused_layer = None;
    for mapped in wayland.space.elements() {
        if mapped.set_activated(mapped == window)
            && let Some(toplevel) = mapped.toplevel()
            && toplevel.is_initial_configure_sent()
        {
            toplevel.send_pending_configure();
        }
    }
    if let Some(location) = wayland.space.element_location(window) {
        wayland.space.map_element(window.clone(), location, true);
    }
    wayland.focused_window = window.wl_surface().map(|surface| surface.into_owned());
}

pub(crate) fn place_mapping(
    wayland: &mut WaylandState,
    cameras: &OutputCameras,
    transition: &MapTransition,
) {
    let restored_output = transition
        .placement
        .as_ref()
        .and_then(|placement| placement.output.as_deref())
        .and_then(|name| wayland.space.outputs().find(|output| output.name() == name))
        .cloned();
    let restoring = restored_output.is_some();
    let output =
        restored_output.or_else(|| crate::wayland::focus::selected_output(wayland).cloned());
    let location = if restoring {
        transition
            .placement
            .as_ref()
            .map(|placement| placement.location)
            .unwrap_or_else(|| Point::from((0, 0)))
    } else {
        output
            .as_ref()
            .map(|output| centered_location(wayland, cameras, output, &transition.window))
            .unwrap_or_else(|| Point::from((0, 0)))
    };
    if let Some(output) = output.as_ref() {
        crate::wayland::set_window_output(&transition.window, output);
    }
    wayland
        .space
        .map_element(transition.window.clone(), location, false);
    wayland.windows.update_placement(
        &transition.key,
        Placement {
            location,
            output: crate::wayland::window_output_name(&transition.window),
        },
    );
}

/// Centers a newly-mapped window on the selected output's live camera.
/// Existing freeform windows stay where they are when the camera later moves.
pub(crate) fn centered_location(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    output: &Output,
    window: &Window,
) -> Point<i32, Logical> {
    let Some(output_geo) = wayland.space.output_geometry(output) else {
        return (0, 0).into();
    };
    let local_camera_center = cameras
        .view(&output.name())
        .map(|view| view.center)
        .unwrap_or_else(|| {
            Point::<f32, Physical>::from((
                output_geo.size.w as f32 / 2.0,
                output_geo.size.h as f32 / 2.0,
            ))
        });
    let center = crate::camera::global_center(local_camera_center, output_geo);
    center_window(center, window.geometry().size)
}

fn center_window(
    center: Point<f32, Physical>,
    window_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    Point::from((
        (center.x - window_size.w as f32 / 2.0).round() as i32,
        (center.y - window_size.h as f32 / 2.0).round() as i32,
    ))
}

pub fn close_focused(wayland: &WaylandState) {
    let Some(focused) = wayland.focused_window.as_ref() else {
        return;
    };
    let Some(window) = wayland.space.elements().find(|window| {
        window
            .wl_surface()
            .is_some_and(|surface| surface.as_ref() == focused)
    }) else {
        return;
    };
    if let Some(toplevel) = window.toplevel() {
        toplevel.send_close();
    } else {
        crate::xwayland::close_window(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_location_is_centered_on_global_camera_position() {
        assert_eq!(
            center_window(Point::from((3620.0, 550.0)), Size::from((1000, 700)),),
            Point::from((3120, 200))
        );
    }
}
