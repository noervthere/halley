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

    let mut elements: Vec<SceneElement> =
        super::layer_surface_elements(renderer, output, Layer::Overlay)
            .into_iter()
            .map(SceneElement::Layer)
            .collect();
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
        let scaled_bbox = super::camera_rect(
            geometry.to_physical(1),
            camera_center,
            output_size,
            zoom_scale,
        );
        let opening_visual = window
            .toplevel()
            .and_then(|toplevel| {
                request.window_open_animations.visual(
                    toplevel.wl_surface(),
                    request.target_presentation_time,
                    geometry.to_physical(1).size,
                )
            })
            .unwrap_or_default();
        let animated_bbox = opening_visual.transform_rect(scaled_bbox, scaled_bbox);
        let fullscreen = window.toplevel().and_then(|toplevel| {
            request.fullscreen.presentation(
                toplevel.wl_surface(),
                output,
                request.target_presentation_time,
            )
        });
        let animated_bbox = fullscreen
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
            .unwrap_or(animated_bbox);
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
                map_rect(native_geometry, geometry.to_physical(1), animated_bbox)
            } else {
                let final_destination =
                    super::camera_rect(native_geometry, camera_center, output_size, zoom_scale);
                opening_visual.transform_rect(final_destination, scaled_bbox)
            };
            SceneElement::Rescaled(super::rescale::RescaledElement::new(
                surface_element,
                destination,
                if fullscreen.is_some() {
                    1.0
                } else {
                    opening_visual.alpha()
                },
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
                    map_rect(native_geometry, geometry.to_physical(1), animated_bbox)
                } else {
                    let final_destination =
                        super::camera_rect(native_geometry, camera_center, output_size, zoom_scale);
                    opening_visual.transform_rect(final_destination, scaled_bbox)
                };
                let element = super::rescale::RescaledElement::new(
                    surface_element,
                    destination,
                    if fullscreen.is_some() {
                        1.0
                    } else {
                        opening_visual.alpha()
                    },
                );
                CropRenderElement::from_element(element, 1.0, animated_bbox)
                    .map(SceneElement::Cropped)
            }));
        }

        let is_focused = window
            .toplevel()
            .is_some_and(|toplevel| Some(toplevel.wl_surface()) == request.focused);
        let border_color = super::window_border_color(request.decorations, is_focused)
            * opening_visual.alpha()
            * fullscreen
                .map(|presentation| (1.0 - presentation.progress) as f32)
                .unwrap_or(1.0);
        let border_width =
            ((request.decorations.border_width_px as f64 * zoom_scale as f64).round() as i32)
                .max(1);
        elements.extend(
            super::border_strips(animated_bbox, border_width, border_color)
                .into_iter()
                .map(SceneElement::Border),
        );
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

    if let Some(position) = cursor_position_for_output(output_geometry, request.cursor_position) {
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
