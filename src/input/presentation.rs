use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle};
use smithay::wayland::compositor::{SubsurfaceCachedState, get_parent, with_states};
use smithay::wayland::seat::WaylandFocus;

use crate::camera::OutputCameras;

/// The presentation geometry shared by scene construction and input routing.
///
/// Keeping this in the input-independent presentation module makes the
/// compositor render and invert the exact same camera, fullscreen, and
/// opening-animation rectangles.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowVisualState {
    pub(crate) source_geometry: Rectangle<i32, Logical>,
    pub(crate) camera_rect: Rectangle<i32, Physical>,
    pub(crate) presentation_rect: Rectangle<i32, Physical>,
    pub(crate) animated_rect: Rectangle<i32, Physical>,
    pub(crate) opening_alpha: f32,
    pub(crate) opening_is_animating: bool,
    pub(crate) fullscreen: Option<crate::wayland::fullscreen::FullscreenPresentation>,
    pub(crate) maximize: Option<crate::wayland::maximize::FieldMaximizePresentation>,
    pub(crate) camera_center: Point<f32, Physical>,
    pub(crate) zoom_scale: f32,
}

pub(crate) fn window_visual_state(
    space: &Space<Window>,
    cameras: &OutputCameras,
    window: &Window,
    output: &Output,
    window_open_animations: &crate::animation::WindowOpenAnimations,
    fullscreen: &crate::wayland::fullscreen::FullscreenManager,
    maximize: &crate::wayland::maximize::FieldMaximizeManager,
    now: std::time::Duration,
) -> Option<WindowVisualState> {
    let output_geometry = space.output_geometry(output)?;
    let output_size = output_geometry.size.to_physical(1);
    let view = cameras.view(&output.name())?;
    let camera_center = crate::camera::global_center(view.center, output_geometry);
    let source_geometry = space.element_geometry(window)?;
    let window_surface = window.wl_surface()?;
    let camera_rect = crate::backend::camera_rect(
        source_geometry.to_physical(1),
        camera_center,
        output_size,
        view.scale,
    );
    let opening_is_animating = window_open_animations.is_animating(window_surface.as_ref(), now);
    let opening_visual = window_open_animations
        .visual(window_surface.as_ref(), now, camera_rect)
        .unwrap_or_default();
    let fullscreen = fullscreen.presentation(window_surface.as_ref(), output, now);
    let maximize = fullscreen
        .is_none()
        .then(|| maximize.presentation(window_surface.as_ref(), output, output_geometry, now))
        .flatten();
    let presentation_rect = fullscreen
        .map(|presentation| {
            let windowed = presentation
                .windowed_geometry
                .map(|geometry| {
                    crate::backend::camera_rect(
                        geometry.to_physical(1),
                        camera_center,
                        output_size,
                        view.scale,
                    )
                })
                .unwrap_or_else(|| presentation.fullscreen_rect(output_size));
            presentation.client_rect(windowed, output_size)
        })
        .or_else(|| {
            maximize.map(|presentation| {
                let windowed = crate::backend::camera_rect(
                    presentation.windowed_rect.to_physical(1),
                    camera_center,
                    output_size,
                    view.scale,
                );
                presentation.client_rect(windowed)
            })
        })
        .unwrap_or(camera_rect);
    let animated_rect = opening_visual.transform_rect(presentation_rect, presentation_rect);

    Some(WindowVisualState {
        source_geometry,
        camera_rect,
        presentation_rect,
        animated_rect,
        opening_alpha: opening_visual.alpha(),
        opening_is_animating,
        fullscreen,
        maximize,
        camera_center,
        zoom_scale: view.scale,
    })
}

/// The live mapping between a window's compositor-space geometry and its
/// current on-screen presentation.
///
/// Pointer routing and pointer constraints both consume this type. Keeping
/// the inverse pair here prevents either path from reconstructing a surface
/// origin from the pointer's last event location.
#[derive(Clone, Debug)]
pub struct WindowPresentation {
    root: WlSurface,
    source_geometry: Rectangle<i32, Logical>,
    root_origin: Point<f64, Logical>,
    visual_geometry: Rectangle<i32, Logical>,
    hit_geometry: Rectangle<i32, Logical>,
}

impl WindowPresentation {
    pub fn for_window(
        space: &Space<Window>,
        cameras: &OutputCameras,
        window_open_animations: &crate::animation::WindowOpenAnimations,
        fullscreen: &crate::wayland::fullscreen::FullscreenManager,
        maximize: &crate::wayland::maximize::FieldMaximizeManager,
        window: &Window,
        output: &Output,
        now: std::time::Duration,
    ) -> Option<Self> {
        let root = window.wl_surface()?.into_owned();
        let output_geometry = space.output_geometry(output)?;
        let visual = window_visual_state(
            space,
            cameras,
            window,
            output,
            window_open_animations,
            fullscreen,
            maximize,
            now,
        )?;
        let source_geometry = visual.source_geometry;
        let source_bbox = space.element_bbox(window)?;
        let element_location = space.element_location(window)?;
        let root_origin = (element_location - window.geometry().loc).to_f64();
        let output_size = output_geometry.size.to_physical(1);
        let global_geometry = |local: Rectangle<i32, Physical>| {
            Rectangle::new(
                output_geometry.loc + local.loc.to_logical(1),
                local.size.to_logical(1),
            )
        };
        let hit_rect = if visual.fullscreen.is_some() || visual.maximize.is_some() {
            let presented = crate::animation::map_rect(
                source_bbox.to_physical(1),
                source_geometry.to_physical(1),
                visual.presentation_rect,
            );
            crate::animation::map_rect(presented, visual.presentation_rect, visual.animated_rect)
        } else {
            let camera_bbox = crate::backend::camera_rect(
                source_bbox.to_physical(1),
                visual.camera_center,
                output_size,
                visual.zoom_scale,
            );
            crate::animation::map_rect(camera_bbox, visual.camera_rect, visual.animated_rect)
        };
        let visual_geometry = global_geometry(visual.animated_rect);
        let hit_geometry = global_geometry(hit_rect);

        Some(Self {
            root,
            source_geometry,
            root_origin,
            visual_geometry,
            hit_geometry,
        })
    }

    pub fn for_surface(
        space: &Space<Window>,
        cameras: &OutputCameras,
        primary: &Output,
        window_open_animations: &crate::animation::WindowOpenAnimations,
        fullscreen: &crate::wayland::fullscreen::FullscreenManager,
        maximize: &crate::wayland::maximize::FieldMaximizeManager,
        surface: &WlSurface,
        now: std::time::Duration,
    ) -> Option<Self> {
        let root = crate::wayland::compositor::root_surface(surface);
        let window = space.elements().find(|window| {
            window
                .wl_surface()
                .is_some_and(|candidate| candidate.as_ref() == &root)
        })?;
        let output = space
            .outputs()
            .find(|output| crate::wayland::window_is_on_output(window, output, primary))?;
        Self::for_window(
            space,
            cameras,
            window_open_animations,
            fullscreen,
            maximize,
            window,
            output,
            now,
        )
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

    pub fn screen_from_source(&self, source: Point<f64, Logical>) -> Point<f64, Logical> {
        map_point(
            source,
            self.source_geometry.to_f64(),
            self.visual_geometry.to_f64(),
        )
    }

    pub fn surface_origin(&self, surface: &WlSurface) -> Option<Point<f64, Logical>> {
        let offset = subsurface_offset_from_root(surface, &self.root)?;
        Some(self.root_origin + offset.to_f64())
    }

    pub fn surface_from_screen(
        &self,
        surface: &WlSurface,
        screen: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        Some(self.source_from_screen(screen) - self.surface_origin(surface)?)
    }

    pub fn screen_from_surface(
        &self,
        surface: &WlSurface,
        local: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        Some(self.screen_from_source(self.surface_origin(surface)? + local))
    }
}

fn subsurface_offset_from_root(
    surface: &WlSurface,
    expected_root: &WlSurface,
) -> Option<Point<i32, Logical>> {
    let mut current = surface.clone();
    let mut offset = Point::from((0, 0));
    while &current != expected_root {
        let parent = get_parent(&current)?;
        let location = with_states(&current, |states| {
            states
                .cached_state
                .get::<SubsurfaceCachedState>()
                .current()
                .location
        });
        offset += location;
        current = parent;
    }
    Some(offset)
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

    #[test]
    fn subsurface_coordinates_share_the_window_transform() {
        let source = Rectangle::<f64, Logical>::new((400.0, 200.0).into(), (800.0, 600.0).into());
        let visual = Rectangle::<f64, Logical>::new((100.0, 50.0).into(), (1600.0, 1200.0).into());
        let root_origin = Point::from((380.0, 170.0));
        let subsurface_offset = Point::from((30.0, 40.0));
        let local = Point::from((25.0, 15.0));
        let source_point = root_origin + subsurface_offset + local;
        let screen = map_point(source_point, source, visual);
        let round_trip = map_point(screen, visual, source) - root_origin - subsurface_offset;

        assert_eq!(screen, Point::from((170.0, 100.0)));
        assert_eq!(round_trip, local);
    }
}
