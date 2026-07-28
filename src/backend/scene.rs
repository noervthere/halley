use std::error::Error;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer;

use super::RenderRequest;

render_elements! {
    /// The complete front-to-back scene consumed by both presentation
    /// backends. Keeping one element type and one builder prevents nested and
    /// real-hardware sessions from drifting in z-order or visual policy.
    pub SceneElement<=GlesRenderer>;
    Cursor=MemoryRenderBufferRenderElement<GlesRenderer>,
    Rescaled=super::rescale::RescaledElement,
    Cropped=CropRenderElement<super::rescale::RescaledElement>,
    FullscreenBlend=super::fullscreen_texture::FullscreenBlendElement,
    CaptureOverlay=super::capture_overlay::CaptureOverlayElement,
    Border=SolidColorRenderElement,
    Layer=WaylandSurfaceRenderElement<GlesRenderer>,
}

pub fn build(
    renderer: &mut GlesRenderer,
    output: &Output,
    primary_output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    request: RenderRequest<'_>,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let output_size = output_geometry.size.to_physical(1);
    let view = request
        .cameras
        .view(&output.name())
        .ok_or_else(|| format!("output {:?} has no camera", output.name()))?;
    let camera_center = crate::camera::global_center(view.center, output_geometry);
    let zoom_scale = view.scale;

    let mut elements = capture_overlay_elements(
        renderer,
        output,
        output_geometry,
        request.capture_overlay,
        request.decorations,
    )?;
    elements.extend(
        super::layer_surface_elements(renderer, output, Layer::Overlay)
            .into_iter()
            .map(SceneElement::Layer),
    );
    if !request
        .fullscreen
        .covers_top(request.focused, output, request.target_presentation_time)
    {
        elements.extend(
            super::layer_surface_elements(renderer, output, Layer::Top)
                .into_iter()
                .map(SceneElement::Layer),
        );
    }

    // Space iterates bottom-to-top while render element lists are
    // front-to-back. Reversing here preserves the compositor's window order.
    for window in request.space.elements().rev() {
        if !crate::wayland::window_is_on_output(window, output, primary_output) {
            continue;
        }
        let Some(geometry) = request.space.element_geometry(window) else {
            continue;
        };
        let Some(location) = request.space.element_location(window) else {
            continue;
        };
        let Some(window_surface) = window.wl_surface() else {
            continue;
        };
        let scaled_bbox = super::camera_rect(
            geometry.to_physical(1),
            camera_center,
            output_size,
            zoom_scale,
        );
        let opening_visual = request
            .window_open_animations
            .visual(
                window_surface.as_ref(),
                request.target_presentation_time,
                scaled_bbox,
            )
            .unwrap_or_default();
        let fullscreen = request.fullscreen.presentation(
            window_surface.as_ref(),
            output,
            request.target_presentation_time,
        );
        let presentation_bbox = fullscreen
            .map(|presentation| {
                let windowed_bbox = presentation
                    .windowed_geometry
                    .map(|geometry| {
                        super::camera_rect(
                            geometry.to_physical(1),
                            camera_center,
                            output_size,
                            zoom_scale,
                        )
                    })
                    .unwrap_or_else(|| presentation.fullscreen_rect(output_size));
                presentation.client_rect(windowed_bbox, output_size)
            })
            .unwrap_or(scaled_bbox);
        let animated_bbox = opening_visual.transform_rect(presentation_bbox, presentation_bbox);
        if animated_bbox.size.w == 0 || animated_bbox.size.h == 0 {
            continue;
        }

        // Space locations refer to window geometry while surface trees begin
        // at the underlying surface origin. Popups remain uncropped because
        // they may legitimately extend beyond the toplevel geometry.
        let surface_location = super::window_surface_location(location, window.geometry());
        let (popup_elements, surface_elements) =
            super::window_surface_elements(renderer, window, surface_location);
        elements.extend(popup_elements.into_iter().map(|surface_element| {
            let native_geometry = surface_element.geometry(Scale::from(1.0));
            let destination = if fullscreen.is_some() {
                let destination =
                    map_rect(native_geometry, geometry.to_physical(1), presentation_bbox);
                opening_visual.transform_rect(destination, presentation_bbox)
            } else {
                let final_destination =
                    super::camera_rect(native_geometry, camera_center, output_size, zoom_scale);
                opening_visual.transform_rect(final_destination, scaled_bbox)
            };
            SceneElement::Rescaled(super::rescale::RescaledElement::new(
                surface_element,
                destination,
                opening_visual.alpha(),
            ))
        }));
        let fullscreen_blend = if let Some(presentation) = fullscreen {
            match request.fullscreen_textures.blend_element(
                renderer,
                window,
                animated_bbox,
                presentation.transition_completion,
            ) {
                Ok(blend) => blend,
                Err(err) => {
                    eventline::warn!("fullscreen: failed to blend window textures: {err}");
                    None
                }
            }
        } else {
            None
        };
        if let Some(blend) = fullscreen_blend {
            elements.push(SceneElement::FullscreenBlend(blend));
        } else {
            elements.extend(surface_elements.into_iter().filter_map(|surface_element| {
                let native_geometry = surface_element.geometry(Scale::from(1.0));
                let destination = if fullscreen.is_some() {
                    let destination =
                        map_rect(native_geometry, geometry.to_physical(1), presentation_bbox);
                    opening_visual.transform_rect(destination, presentation_bbox)
                } else {
                    let final_destination =
                        super::camera_rect(native_geometry, camera_center, output_size, zoom_scale);
                    opening_visual.transform_rect(final_destination, scaled_bbox)
                };
                let element = super::rescale::RescaledElement::new(
                    surface_element,
                    destination,
                    opening_visual.alpha(),
                );
                CropRenderElement::from_element(element, 1.0, animated_bbox)
                    .map(SceneElement::Cropped)
            }));
        }

        let is_focused = Some(window_surface.as_ref()) == request.focused;
        let border_color = super::window_border_color(request.decorations, is_focused)
            * opening_visual.alpha()
            * fullscreen
                .map(|presentation| (1.0 - presentation.progress) as f32)
                .unwrap_or(1.0);
        let border_width =
            ((request.decorations.border_width_px as f64 * zoom_scale as f64).round() as i32)
                .max(1);
        if !crate::xwayland::is_override_redirect(window) {
            elements.extend(
                super::border_strips(animated_bbox, border_width, border_color)
                    .into_iter()
                    .map(SceneElement::Border),
            );
        }
        if let Some(fullscreen) = fullscreen {
            elements.push(SceneElement::Border(SolidColorRenderElement::new(
                Id::new(),
                Rectangle::new((0, 0).into(), output_size),
                CommitCounter::default(),
                smithay::backend::renderer::Color32F::new(
                    0.0,
                    0.0,
                    0.0,
                    fullscreen.progress as f32,
                ),
                Kind::Unspecified,
            )));
        }
    }

    elements.extend(
        super::layer_surface_elements(renderer, output, Layer::Bottom)
            .into_iter()
            .map(SceneElement::Layer),
    );
    elements.extend(
        super::layer_surface_elements(renderer, output, Layer::Background)
            .into_iter()
            .map(SceneElement::Layer),
    );

    if request.show_cursor
        && let Some(position) = cursor_position_for_output(output_geometry, request.cursor_position)
    {
        let cursor = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            position,
            &request.cursor.buffer,
            None,
            None,
            None,
            Kind::Cursor,
        )?;
        // Element lists are front-to-back, so the cursor belongs at index 0.
        elements.insert(0, SceneElement::Cursor(cursor));
    }

    Ok(elements)
}

fn capture_overlay_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    overlay: crate::capture::CaptureOverlay<'_>,
    decorations: &halley_config::Decorations,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    match overlay {
        crate::capture::CaptureOverlay::None => Ok(Vec::new()),
        crate::capture::CaptureOverlay::Region(region) => {
            Ok(capture_picker_elements(output_geometry, region, true)
                .into_iter()
                .rev()
                .map(SceneElement::Border)
                .collect())
        }
        crate::capture::CaptureOverlay::Highlight(region) => {
            Ok(capture_picker_elements(output_geometry, region, false)
                .into_iter()
                .rev()
                .map(SceneElement::Border)
                .collect())
        }
        crate::capture::CaptureOverlay::Menu {
            output_name,
            selected,
            hovered,
            window_available,
        } if output.name() == output_name => Ok(super::capture_overlay::menu_elements(
            renderer,
            output_geometry,
            selected,
            hovered,
            window_available,
            super::window_border_color(decorations, true),
        )?
        .into_iter()
        .rev()
        .map(SceneElement::CaptureOverlay)
        .collect()),
        crate::capture::CaptureOverlay::Menu { .. } => Ok(Vec::new()),
    }
}

fn capture_picker_elements(
    output: Rectangle<i32, Logical>,
    selection: Rectangle<i32, Logical>,
    region_style: bool,
) -> Vec<SolidColorRenderElement> {
    let output_local = Rectangle::<i32, Physical>::from_size(output.size.to_physical(1));
    let selected = output.intersection(selection).map(|intersection| {
        Rectangle::<i32, Physical>::new(
            (intersection.loc - output.loc).to_physical(1),
            intersection.size.to_physical(1),
        )
    });
    let dim = smithay::backend::renderer::Color32F::new(0.0, 0.0, 0.0, 0.48);
    let white = smithay::backend::renderer::Color32F::new(1.0, 1.0, 1.0, 1.0);
    let make = |geometry, color| {
        SolidColorRenderElement::new(
            Id::new(),
            geometry,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
    };

    let Some(selected) = selected else {
        return vec![make(output_local, dim)];
    };
    let mut elements = Vec::with_capacity(12);
    let right = selected.loc.x + selected.size.w;
    let bottom = selected.loc.y + selected.size.h;
    for rect in [
        Rectangle::new(
            (0, 0).into(),
            (output_local.size.w, selected.loc.y.max(0)).into(),
        ),
        Rectangle::new(
            (0, bottom).into(),
            (output_local.size.w, (output_local.size.h - bottom).max(0)).into(),
        ),
        Rectangle::new(
            (0, selected.loc.y).into(),
            (selected.loc.x.max(0), selected.size.h).into(),
        ),
        Rectangle::new(
            (right, selected.loc.y).into(),
            ((output_local.size.w - right).max(0), selected.size.h).into(),
        ),
    ] {
        if rect.size.w > 0 && rect.size.h > 0 {
            elements.push(make(rect, dim));
        }
    }
    if region_style {
        elements.extend(
            dashed_border_rects(selected)
                .into_iter()
                .map(|rect| make(rect, white)),
        );
        let handle_size = 12;
        for point in [
            selected.loc,
            (right, selected.loc.y).into(),
            (selected.loc.x, bottom).into(),
            (right, bottom).into(),
        ] {
            elements.push(make(
                Rectangle::new(
                    (point.x - handle_size / 2, point.y - handle_size / 2).into(),
                    (handle_size, handle_size).into(),
                ),
                white,
            ));
        }
    } else {
        elements.extend(
            inner_border_rects(selected, 2)
                .into_iter()
                .map(|rect| make(rect, white)),
        );
    }
    elements
}

fn inner_border_rects(rect: Rectangle<i32, Physical>, width: i32) -> [Rectangle<i32, Physical>; 4] {
    let width = width.max(0).min(rect.size.w).min(rect.size.h);
    let right = rect.loc.x + rect.size.w;
    let bottom = rect.loc.y + rect.size.h;
    [
        Rectangle::new(rect.loc, (rect.size.w, width).into()),
        Rectangle::new(
            (rect.loc.x, bottom - width).into(),
            (rect.size.w, width).into(),
        ),
        Rectangle::new(rect.loc, (width, rect.size.h).into()),
        Rectangle::new(
            (right - width, rect.loc.y).into(),
            (width, rect.size.h).into(),
        ),
    ]
}

fn dashed_border_rects(rect: Rectangle<i32, Physical>) -> Vec<Rectangle<i32, Physical>> {
    const THICKNESS: i32 = 2;
    const DASH_LENGTH: i32 = 10;
    const GAP_LENGTH: i32 = 6;

    let right = rect.loc.x + rect.size.w;
    let bottom = rect.loc.y + rect.size.h;
    let mut strips = Vec::new();

    let mut x = rect.loc.x;
    while x < right {
        let length = (right - x).min(DASH_LENGTH);
        strips.push(Rectangle::new(
            (x, rect.loc.y).into(),
            (length, THICKNESS).into(),
        ));
        strips.push(Rectangle::new(
            (x, bottom - THICKNESS).into(),
            (length, THICKNESS).into(),
        ));
        x += DASH_LENGTH + GAP_LENGTH;
    }

    let mut y = rect.loc.y;
    while y < bottom {
        let length = (bottom - y).min(DASH_LENGTH);
        strips.push(Rectangle::new(
            (rect.loc.x, y).into(),
            (THICKNESS, length).into(),
        ));
        strips.push(Rectangle::new(
            (right - THICKNESS, y).into(),
            (THICKNESS, length).into(),
        ));
        y += DASH_LENGTH + GAP_LENGTH;
    }

    strips
}

fn map_rect(
    rect: Rectangle<i32, Physical>,
    source: Rectangle<i32, Physical>,
    destination: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    if source.size.w == 0 || source.size.h == 0 {
        return destination;
    }
    let scale_x = f64::from(destination.size.w) / f64::from(source.size.w);
    let scale_y = f64::from(destination.size.h) / f64::from(source.size.h);
    let left = f64::from(destination.loc.x) + f64::from(rect.loc.x - source.loc.x) * scale_x;
    let top = f64::from(destination.loc.y) + f64::from(rect.loc.y - source.loc.y) * scale_y;
    let right = left + f64::from(rect.size.w) * scale_x;
    let bottom = top + f64::from(rect.size.h) * scale_y;
    Rectangle::new(
        (left.round() as i32, top.round() as i32).into(),
        (
            (right - left).round().max(0.0) as i32,
            (bottom - top).round().max(0.0) as i32,
        )
            .into(),
    )
}

pub fn cursor_position_for_output(
    output_geometry: Rectangle<i32, Logical>,
    cursor_position: (f64, f64),
) -> Option<Point<f64, Physical>> {
    let cursor_position = Point::<f64, Logical>::from(cursor_position);
    output_geometry
        .to_f64()
        .contains(cursor_position)
        .then(|| (cursor_position - output_geometry.loc.to_f64()).to_physical(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Physical, Rectangle};

    #[test]
    fn cursor_is_localized_to_the_containing_output() {
        let output = Rectangle::new((2560, 0).into(), (1920, 1200).into());

        assert_eq!(
            cursor_position_for_output(output, (3000.0, 500.0)),
            Some(Point::from((440.0, 500.0)))
        );
        assert_eq!(cursor_position_for_output(output, (2559.0, 500.0)), None);
    }

    #[test]
    fn output_edges_are_half_open() {
        let output = Rectangle::new((0, 0).into(), (1920, 1080).into());

        assert!(cursor_position_for_output(output, (0.0, 0.0)).is_some());
        assert!(cursor_position_for_output(output, (1919.0, 1079.0)).is_some());
        assert!(cursor_position_for_output(output, (1920.0, 500.0)).is_none());
    }

    #[test]
    fn resize_handles_are_reserved_for_region_selection() {
        let output = Rectangle::<i32, Logical>::new((0, 0).into(), (1920, 1080).into());
        let selection = Rectangle::<i32, Logical>::new((320, 180).into(), (1280, 720).into());

        let region = capture_picker_elements(output, selection, true);
        let highlight = capture_picker_elements(output, selection, false);

        let handles = |elements: &[SolidColorRenderElement]| {
            elements
                .iter()
                .filter(|element| element.geometry(Scale::from(1.0)).size == (12, 12).into())
                .count()
        };
        assert_eq!(handles(&region), 4);
        assert_eq!(handles(&highlight), 0);
    }

    #[test]
    fn region_border_uses_ten_pixel_dashes_with_six_pixel_gaps() {
        let selection = Rectangle::<i32, Physical>::new((320, 180).into(), (100, 80).into());
        let strips = dashed_border_rects(selection);

        assert!(strips.contains(&Rectangle::new((320, 180).into(), (10, 2).into())));
        assert!(strips.contains(&Rectangle::new((336, 180).into(), (10, 2).into())));
        assert!(!strips.iter().any(|strip| {
            strip.loc.y == 180 && strip.loc.x < 336 && strip.loc.x + strip.size.w > 330
        }));
    }

    #[test]
    fn full_screen_highlight_border_stays_inside_the_output() {
        let output = Rectangle::<i32, Physical>::new((0, 0).into(), (1920, 1080).into());
        let border = inner_border_rects(output, 2);

        assert!(border.into_iter().all(|strip| output.contains_rect(strip)));
        assert_eq!(border[0], Rectangle::new((0, 0).into(), (1920, 2).into()));
        assert_eq!(
            border[1],
            Rectangle::new((0, 1078).into(), (1920, 2).into())
        );
    }

    #[test]
    fn secondary_window_geometry_becomes_output_local() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());
        let camera_center = crate::camera::global_center(Point::from((1060.0, 550.0)), secondary);
        let world_rect = Rectangle::<i32, Physical>::new((3520, 600).into(), (200, 100).into());

        assert_eq!(
            super::super::camera_rect(
                world_rect,
                camera_center,
                secondary.size.to_physical(1),
                0.5,
            ),
            Rectangle::new((910, 625).into(), (100, 50).into())
        );
    }

    #[test]
    fn fullscreen_rect_interpolates_position_and_size() {
        let windowed = Rectangle::new((100, 50).into(), (800, 600).into());
        let start = crate::wayland::fullscreen::FullscreenPresentation {
            progress: 0.0,
            transition_completion: 0.0,
            windowed_geometry: None,
            fullscreen_size: (1920, 1080).into(),
        };
        let end = crate::wayland::fullscreen::FullscreenPresentation {
            progress: 1.0,
            ..start
        };
        let middle = crate::wayland::fullscreen::FullscreenPresentation {
            progress: 0.5,
            ..start
        };

        assert_eq!(start.client_rect(windowed, (1920, 1080).into()), windowed);
        assert_eq!(
            end.client_rect(windowed, (1920, 1080).into()),
            Rectangle::new((0, 0).into(), (1920, 1080).into())
        );
        assert_eq!(
            middle.client_rect(windowed, (1920, 1080).into()),
            Rectangle::new((50, 25).into(), (1360, 840).into())
        );
    }

    #[test]
    fn surface_rects_map_into_animated_client_bounds() {
        assert_eq!(
            map_rect(
                Rectangle::new((120, 70).into(), (200, 100).into()),
                Rectangle::new((100, 50).into(), (800, 600).into()),
                Rectangle::new((0, 0).into(), (1600, 1200).into()),
            ),
            Rectangle::new((40, 40).into(), (400, 200).into())
        );
    }
}
