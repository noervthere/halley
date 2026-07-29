use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Size};

use crate::camera::OutputCameras;
use crate::wayland::WaylandState;

pub(crate) struct InitialWindowPlacement {
    pub output: Output,
    pub location: Point<i32, Logical>,
}

/// Resolves the initial-placement policy shared by every normal toplevel protocol.
///
/// The most recently click-selected output is authoritative. The backend
/// primary is only a startup or output-removal fallback.
pub(crate) fn initial_window_placement(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    primary_output: &Output,
    window_size: Size<i32, Logical>,
) -> InitialWindowPlacement {
    let output = crate::wayland::focus::output_for_new_surface(wayland, None, primary_output);
    let location = centered_location_for_size(wayland, cameras, &output, window_size);
    InitialWindowPlacement { output, location }
}

pub(crate) fn centered_location_for_size(
    wayland: &WaylandState,
    cameras: &OutputCameras,
    output: &Output,
    window_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let Some(output_geometry) = wayland.space.output_geometry(output) else {
        return (0, 0).into();
    };
    let local_camera_center = cameras
        .view(&output.name())
        .map(|view| view.center)
        .unwrap_or_else(|| {
            Point::<f32, Physical>::from((
                output_geometry.size.w as f32 / 2.0,
                output_geometry.size.h as f32 / 2.0,
            ))
        });
    let center = crate::camera::global_center(local_camera_center, output_geometry);
    center_window(center, window_size)
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

#[cfg(test)]
mod tests {
    use super::center_window;
    use smithay::utils::{Point, Size};

    #[test]
    fn secondary_output_camera_center_produces_global_window_location() {
        assert_eq!(
            center_window(Point::from((3620.0, 550.0)), Size::from((1000, 700))),
            Point::from((3120, 200))
        );
    }

    #[test]
    fn client_opening_size_is_centered_independently_of_its_buffer() {
        assert_eq!(
            center_window(Point::from((1280.0, 720.0)), Size::from((640, 480))),
            Point::from((960, 480))
        );
    }
}
