use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::seat::WaylandFocus;

use crate::camera::OutputCameras;

/// The live mapping between a window's compositor-space geometry and its
/// current on-screen presentation.
///
/// Pointer routing and pointer constraints both consume this type. Keeping
/// the inverse pair here prevents either path from reconstructing a surface
/// origin from the pointer's last event location.
#[derive(Clone, Debug)]
pub struct WindowPresentation {
    source_geometry: Rectangle<i32, Logical>,
    visual_geometry: Rectangle<i32, Logical>,
    hit_geometry: Rectangle<i32, Logical>,
}

impl WindowPresentation {
    pub fn for_window(
        space: &Space<Window>,
        cameras: &OutputCameras,
        fullscreen: &crate::wayland::fullscreen::FullscreenManager,
        window: &Window,
        output: &Output,
        now: std::time::Duration,
    ) -> Option<Self> {
        let root = window.wl_surface()?.into_owned();
        let output_geometry = space.output_geometry(output)?;
        let source_geometry = space.element_geometry(window)?;
        let source_bbox = space.element_bbox(window)?;
        let view = cameras.view(&output.name())?;
        let camera_center = crate::camera::global_center(view.center, output_geometry);
        let output_size = output_geometry.size.to_physical(1);
        let camera_geometry = |geometry: Rectangle<i32, Logical>| {
            let local = crate::backend::camera_rect(
                geometry.to_physical(1),
                camera_center,
                output_size,
                view.scale,
            );
            Rectangle::new(
                output_geometry.loc + local.loc.to_logical(1),
                local.size.to_logical(1),
            )
        };

        let (visual_geometry, hit_geometry) = match fullscreen.presentation(&root, output, now) {
            Some(presentation) => {
                let windowed_geometry = presentation.windowed_geometry.unwrap_or(source_geometry);
                let windowed = crate::backend::camera_rect(
                    windowed_geometry.to_physical(1),
                    camera_center,
                    output_size,
                    view.scale,
                );
                let visual = presentation.client_rect(windowed, output_size);
                let visual = Rectangle::new(
                    output_geometry.loc + visual.loc.to_logical(1),
                    visual.size.to_logical(1),
                );
                (visual, visual)
            }
            None => (
                camera_geometry(source_geometry),
                camera_geometry(source_bbox),
            ),
        };

        Some(Self {
            source_geometry,
            visual_geometry,
            hit_geometry,
        })
    }

    pub fn visual_geometry(&self) -> Rectangle<i32, Logical> {
        self.visual_geometry
    }

    pub fn contains_screen(&self, screen: Point<f64, Logical>) -> bool {
        self.hit_geometry.to_f64().contains(screen)
    }

    pub fn source_from_screen(&self, screen: Point<f64, Logical>) -> Point<f64, Logical> {
        map_point(
            screen,
            self.visual_geometry.to_f64(),
            self.source_geometry.to_f64(),
        )
    }
}

fn map_point(
    point: Point<f64, Logical>,
    source: Rectangle<f64, Logical>,
    destination: Rectangle<f64, Logical>,
) -> Point<f64, Logical> {
    let scale_x = destination.size.w / source.size.w.max(1.0);
    let scale_y = destination.size.h / source.size.h.max(1.0);
    (
        destination.loc.x + (point.x - source.loc.x) * scale_x,
        destination.loc.y + (point.y - source.loc.y) * scale_y,
    )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transform(
        source: Rectangle<i32, Logical>,
        visual: Rectangle<i32, Logical>,
    ) -> (Rectangle<f64, Logical>, Rectangle<f64, Logical>) {
        (source.to_f64(), visual.to_f64())
    }

    #[test]
    fn screen_and_surface_mapping_are_exact_inverses() {
        let (source, visual) = transform(
            Rectangle::new((400, 200).into(), (1280, 720).into()),
            Rectangle::new((2560, 0).into(), (1920, 1080).into()),
        );
        let local = Point::from((720.0, 380.0));
        let screen = map_point(local, source, visual);

        assert_eq!(screen, Point::from((3040.0, 270.0)));
        assert_eq!(map_point(screen, visual, source), local);
    }

    #[test]
    fn camera_zoom_and_pan_mapping_round_trips() {
        let (source, visual) = transform(
            Rectangle::new((1700, -650).into(), (3840, 2400).into()),
            Rectangle::new((2560, 0).into(), (1920, 1200).into()),
        );
        for point in [
            Point::from((1700.0, -650.0)),
            Point::from((3620.0, 550.0)),
            Point::from((5539.0, 1749.0)),
        ] {
            let screen = map_point(point, source, visual);
            let round_trip = map_point(screen, visual, source);
            assert!((round_trip.x - point.x).abs() < f64::EPSILON);
            assert!((round_trip.y - point.y).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn fullscreen_letterbox_mapping_uses_live_client_rectangle() {
        let (source, visual) = transform(
            Rectangle::new((0, 0).into(), (1024, 768).into()),
            Rectangle::new((240, 0).into(), (1440, 1080).into()),
        );

        assert_eq!(
            map_point(Point::from((512.0, 384.0)), source, visual),
            Point::from((960.0, 540.0))
        );
        assert_eq!(
            map_point(Point::from((960.0, 540.0)), visual, source),
            Point::from((512.0, 384.0))
        );
    }
}
