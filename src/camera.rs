use std::collections::HashMap;

use halley_core::camera::Camera;
use halley_core::field::Vec2;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputView {
    pub center: Point<f32, Physical>,
    pub scale: f32,
}

/// Independent camera state keyed by Smithay output name.
///
/// The collection owns no output/protocol objects and no rendering state;
/// sessions route input to it, while backends only read the resulting
/// `OutputView`. This avoids old Halley's active-monitor state swapping.
#[derive(Default)]
pub struct OutputCameras {
    cameras: HashMap<String, Camera>,
}

impl OutputCameras {
    pub fn insert(&mut self, output_name: String, output_size: Size<i32, Physical>) {
        self.cameras
            .insert(output_name, camera_at_rest(output_size));
    }

    pub fn reset(&mut self, output_name: String, output_size: Size<i32, Physical>) {
        self.insert(output_name, output_size);
    }

    pub fn get(&self, output_name: &str) -> Option<&Camera> {
        self.cameras.get(output_name)
    }

    pub fn get_mut(&mut self, output_name: &str) -> Option<&mut Camera> {
        self.cameras.get_mut(output_name)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Camera> {
        self.cameras.values_mut()
    }

    pub fn view(&self, output_name: &str) -> Option<OutputView> {
        self.get(output_name).map(|camera| OutputView {
            center: Point::from((camera.center.x, camera.center.y)),
            scale: scale(camera),
        })
    }
}

pub fn scale(camera: &Camera) -> f32 {
    (camera.base_size.x / camera.view_size.x.max(1.0)).min(1.0)
}

/// Rebases an output-local camera center into Halley's global world space.
///
/// Cameras stay local so panning one output cannot move another. Windows,
/// however, live in one global `Space`, so rendering and initial placement
/// must apply the same output-layout offset to the selected camera.
pub fn global_center(
    local_camera_center: Point<f32, Physical>,
    output_geometry: Rectangle<i32, Logical>,
) -> Point<f32, Physical> {
    Point::from((
        output_geometry.loc.x as f32 + local_camera_center.x,
        output_geometry.loc.y as f32 + local_camera_center.y,
    ))
}

/// Returns the portion of global world space currently visible through an
/// output camera. XDG popup constraints are expressed in the same world
/// coordinates as `Space`, so their target must follow this inverse of the
/// render transform rather than the output's fixed layout rectangle.
pub fn world_viewport(
    view: OutputView,
    output_geometry: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let center = global_center(view.center, output_geometry);
    let scale = view.scale.max(f32::EPSILON);
    let half_width = output_geometry.size.w as f32 / scale / 2.0;
    let half_height = output_geometry.size.h as f32 / scale / 2.0;
    let left = (center.x - half_width).floor() as i32;
    let top = (center.y - half_height).floor() as i32;
    let right = (center.x + half_width).ceil() as i32;
    let bottom = (center.y + half_height).ceil() as i32;

    Rectangle::new((left, top).into(), (right - left, bottom - top).into())
}

fn camera_at_rest(output_size: Size<i32, Physical>) -> Camera {
    Camera::new(
        Vec2 {
            x: output_size.w as f32 / 2.0,
            y: output_size.h as f32 / 2.0,
        },
        Vec2 {
            x: output_size.w as f32,
            y: output_size.h as f32,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_keep_independent_camera_state() {
        let mut cameras = OutputCameras::default();
        cameras.insert("DP-1".into(), Size::from((2560, 1440)));
        cameras.insert("DP-2".into(), Size::from((1920, 1200)));

        cameras.get_mut("DP-2").unwrap().view_size = Vec2 {
            x: 3840.0,
            y: 2400.0,
        };
        cameras.get_mut("DP-2").unwrap().center.x += 100.0;

        assert_eq!(cameras.view("DP-1").unwrap().scale, 1.0);
        assert_eq!(
            cameras.view("DP-1").unwrap().center,
            Point::from((1280.0, 720.0))
        );
        assert_eq!(cameras.view("DP-2").unwrap().scale, 0.5);
        assert_eq!(
            cameras.view("DP-2").unwrap().center,
            Point::from((1060.0, 600.0))
        );
    }

    #[test]
    fn output_local_camera_center_rebases_into_global_space() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());

        assert_eq!(
            global_center(Point::from((960.0, 600.0)), secondary),
            Point::from((3520.0, 600.0))
        );
        assert_eq!(
            global_center(Point::from((1060.0, 550.0)), secondary),
            Point::from((3620.0, 550.0))
        );
    }

    #[test]
    fn resting_camera_viewport_matches_output_layout() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());
        let view = OutputView {
            center: Point::from((960.0, 600.0)),
            scale: 1.0,
        };

        assert_eq!(world_viewport(view, secondary), secondary);
    }

    #[test]
    fn zoomed_camera_viewport_expands_around_global_center() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());
        let view = OutputView {
            center: Point::from((1060.0, 550.0)),
            scale: 0.5,
        };

        assert_eq!(
            world_viewport(view, secondary),
            Rectangle::new((1700, -650).into(), (3840, 2400).into())
        );
    }
}
