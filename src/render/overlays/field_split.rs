use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Rectangle};

use crate::input::grab::FieldSplitCandidate;
use crate::presentation::camera::OutputCameras;
use crate::render::node::{NodeRenderer, NodeSlot};
use crate::render::scene::SceneElement;

#[allow(clippy::too_many_arguments)]
pub fn elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    cameras: &OutputCameras,
    candidate: Option<&FieldSplitCandidate>,
    overlay_config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
    node_renderer: &mut NodeRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
) -> Vec<SceneElement> {
    let Some(candidate) = candidate.filter(|candidate| candidate.output == output.name()) else {
        return Vec::new();
    };
    let Some(view) = cameras.view(&output.name()) else {
        return Vec::new();
    };
    let visuals = super::shell::resolve_visuals(overlay_config);
    let screen_rect = |world: Rectangle<i32, Logical>| {
        field_split_screen_rect(world, output_geometry, view.center, view.scale)
    };
    let fill_color = Color32F::new(visuals.border.r, visuals.border.g, visuals.border.b, 0.12);
    let border_color = Color32F::new(visuals.border.r, visuals.border.g, visuals.border.b, 0.92);
    let radius = field_split_preview_radius(decorations.border_radius_px, view.scale);
    let world_rects = candidate
        .layout()
        .map(|layout| vec![layout.dragged_outer, layout.target_outer])
        .unwrap_or_else(|| vec![candidate.highlight()]);
    let mut elements = Vec::new();
    for (index, world) in world_rects.into_iter().enumerate() {
        let rect = screen_rect(world);
        if rect.size.w < 1 || rect.size.h < 1 {
            continue;
        }
        if !candidate.ready {
            elements.push(SceneElement::Border(crate::render::solid_color_element(
                node_renderer.slot_id(&output.name(), NodeSlot::FieldSplitFill(index as u8)),
                rect,
                fill_color,
            )));
            continue;
        }
        if let Some(border) = window_decoration_renderer.border_element(
            renderer,
            node_renderer.slot_id(
                &output.name(),
                NodeSlot::FieldSplitRoundedBorder(index as u8),
            ),
            rect,
            2,
            radius,
            border_color,
            1.0,
        ) {
            elements.push(SceneElement::WindowBorder(border));
        } else {
            elements.extend(
                crate::render::border_strips(
                    std::array::from_fn(|edge| {
                        node_renderer.slot_id(
                            &output.name(),
                            NodeSlot::FieldSplitBorder(index as u8, edge as u8),
                        )
                    }),
                    rect,
                    2,
                    border_color,
                )
                .into_iter()
                .map(SceneElement::Border),
            );
        }
    }
    elements
}

fn field_split_preview_radius(configured: i32, scale: f32) -> f32 {
    crate::render::window_decoration::scaled_metric(configured, scale) as f32
}

fn field_split_screen_rect(
    world: Rectangle<i32, Logical>,
    output_geometry: Rectangle<i32, Logical>,
    camera_center: smithay::utils::Point<f32, Physical>,
    scale: f32,
) -> Rectangle<i32, Physical> {
    let output_local = Rectangle::new(world.loc - output_geometry.loc, world.size);
    crate::render::camera_rect(
        output_local.to_physical(1),
        camera_center,
        output_geometry.size.to_physical(1),
        scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rounding_tracks_the_current_window_border_radius() {
        assert_eq!(field_split_preview_radius(12, 1.0), 12.0);
        assert_eq!(field_split_preview_radius(12, 0.5), 6.0);
        assert_eq!(field_split_preview_radius(0, 1.0), 0.0);
    }

    #[test]
    fn camera_preview_rect_stays_in_field_space() {
        let output = Rectangle::new((0, 0).into(), (800, 600).into());
        let world = Rectangle::new((100, 50).into(), (400, 300).into());
        assert_eq!(
            field_split_screen_rect(world, output, (300.0, 200.0).into(), 0.5),
            Rectangle::new((300, 225).into(), (200, 150).into())
        );
    }

    #[test]
    fn camera_preview_rebases_world_coordinates_on_secondary_outputs() {
        let output = Rectangle::new((2560, 0).into(), (1920, 1200).into());
        let world = Rectangle::new((2860, 200).into(), (800, 600).into());
        assert_eq!(
            field_split_screen_rect(world, output, (960.0, 600.0).into(), 1.0),
            Rectangle::new((300, 200).into(), (800, 600).into())
        );
    }
}
