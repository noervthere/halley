use super::*;
use crate::render::scene::nodes::{contrast_text_rgb, node_fill_color, node_ring_color};

pub(super) struct ClusterElementContext<'a> {
    pub(super) output: &'a Output,
    pub(super) output_geometry: Rectangle<i32, Logical>,
    pub(super) clusters: &'a crate::clusters::ClusterSystem,
    pub(super) nodes: &'a crate::nodes::NodesState,
    pub(super) cameras: &'a crate::presentation::camera::OutputCameras,
    pub(super) decorations: &'a halley_config::Decorations,
    pub(super) shadow_config: halley_config::ShadowLayer,
    pub(super) shadow_renderer: &'a mut crate::render::effects::shadow::ShadowRenderer,
    pub(super) node_renderer: &'a mut crate::render::node::NodeRenderer,
    pub(super) ui_text: &'a mut crate::render::text::UiTextRenderer,
}

#[derive(Default)]
pub(super) struct ClusterScene {
    pub(super) overlay: Vec<SceneElement>,
    pub(super) groups: Vec<StackGroup>,
}

pub(super) fn cluster_elements(
    renderer: &mut GlesRenderer,
    cluster_renderer: &mut crate::clusters::render::ClusterRenderer,
    context: ClusterElementContext<'_>,
) -> Result<ClusterScene, Box<dyn Error>> {
    let ClusterElementContext {
        output,
        output_geometry,
        clusters,
        nodes,
        cameras,
        decorations,
        shadow_config,
        shadow_renderer,
        node_renderer,
        ui_text,
    } = context;
    let overlay = creation_elements(
        renderer,
        output,
        output_geometry,
        clusters,
        nodes,
        cameras,
        decorations,
        node_renderer,
        ui_text,
    )?;
    if clusters.active_on(&output.name()).is_some() {
        return Ok(ClusterScene {
            overlay,
            groups: Vec::new(),
        });
    }
    let Some(camera) = cameras.get(&output.name()) else {
        return Ok(ClusterScene {
            overlay,
            groups: Vec::new(),
        });
    };
    let focused_node = nodes.focused();
    let icon_colors = [
        rgba(decorations.border_color_unfocused),
        rgba(decorations.border_color_focused),
    ];
    let mut groups = Vec::new();
    for (_, id, metadata) in clusters.clusters_for_output(&output.name()) {
        let focused =
            focused_node.is_some_and(|node| clusters.cluster_for_member(node) == Some(id));
        let center =
            crate::nodes::screen_from_world(metadata.core_position, camera, output_geometry);
        let local = center - output_geometry.loc;
        let side = crate::nodes::NODE_DIAMETER_PX.round() as i32;
        let destination = Rectangle::<i32, Physical>::new(
            (local.x - side / 2, local.y - side / 2).into(),
            (side, side).into(),
        );
        let ring = node_ring_color(nodes.config, decorations, focused);
        let fill = node_fill_color(nodes.config, ring);
        let mut elements = Vec::new();
        if clusters.config().show_icons {
            let icon_side =
                ((side as f32 * nodes.config.icon_size * 0.98).round() as i32).clamp(16, 42);
            elements.push(SceneElement::ClusterIcon(cluster_renderer.icon(
                renderer,
                Rectangle::new(
                    (local.x - icon_side / 2, local.y - icon_side / 2).into(),
                    (icon_side, icon_side).into(),
                ),
                focused,
                icon_colors,
                nodes.config.opacity,
            )?));
        }
        elements.push(SceneElement::ClusterCore(cluster_renderer.core(
            renderer,
            destination,
            ring,
            fill,
            nodes.config.opacity,
        )?));
        if let Some(shadow) = shadow_renderer.element(
            renderer,
            format!("{}:cluster:{}", output.name(), id.as_u64()),
            destination,
            side as f32 / 2.0,
            nodes.config.opacity,
            shadow_config,
        )? {
            elements.push(SceneElement::Shadow(shadow));
        }
        groups.push(StackGroup {
            // Cluster cores are desktop objects. They remain below live
            // windows while sharing the same stack as nodes and closing
            // snapshots, instead of leaking into Halley's overlay plane.
            stack_index: 0,
            order: id.as_u64(),
            elements,
        });
    }
    Ok(ClusterScene { overlay, groups })
}

fn rgba(color: halley_config::BorderColor) -> [u8; 4] {
    [
        (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}

#[allow(clippy::too_many_arguments)]
fn creation_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    clusters: &crate::clusters::ClusterSystem,
    nodes: &crate::nodes::NodesState,
    cameras: &crate::presentation::camera::OutputCameras,
    decorations: &halley_config::Decorations,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let Some(creation) = clusters
        .creation()
        .filter(|creation| creation.output == output.name())
    else {
        return Ok(Vec::new());
    };
    let Some(camera) = cameras.get(&output.name()) else {
        return Ok(Vec::new());
    };
    let scale = crate::presentation::camera::scale(camera);
    let color = smithay::backend::renderer::Color32F::new(
        decorations.border_color_focused.r,
        decorations.border_color_focused.g,
        decorations.border_color_focused.b,
        0.96,
    );
    let mut foreground = Vec::new();
    for id in &creation.selected {
        let Some(node) = nodes.field.node(*id) else {
            continue;
        };
        let center = crate::nodes::screen_from_world(node.pos, camera, output_geometry)
            - output_geometry.loc;
        let size = (
            (node.intrinsic_size.x * scale).round().max(1.0) as i32,
            (node.intrinsic_size.y * scale).round().max(1.0) as i32,
        );
        let rect = Rectangle::<i32, Physical>::new(
            (center.x - size.0 / 2, center.y - size.1 / 2).into(),
            size.into(),
        );
        foreground.extend(
            crate::render::border_strips(rect, 4, color)
                .into_iter()
                .map(SceneElement::Border),
        );
    }

    let (width, height, message) = if creation.naming {
        let entered = if creation.name_buffer.is_empty() {
            "Type a cluster name…".to_string()
        } else {
            format!("Name: {}_", creation.name_buffer)
        };
        (
            520,
            86,
            format!("{entered}   •   Enter to save   •   Esc to cancel"),
        )
    } else {
        (
            580,
            58,
            format!(
                "Click windows to select   •   Enter to name   •   Esc to cancel   •   {} selected",
                creation.selected.len()
            ),
        )
    };
    let card = Rectangle::<i32, Physical>::new(
        (
            (output_geometry.size.w - width) / 2,
            if creation.naming {
                (output_geometry.size.h - height) / 2
            } else {
                output_geometry.size.h - height - 48
            },
        )
            .into(),
        (width, height).into(),
    );
    let fill = node_fill_color(
        nodes.config,
        (
            decorations.border_color_focused.r,
            decorations.border_color_focused.g,
            decorations.border_color_focused.b,
        ),
    );
    if let Some(text) = ui_text.element(
        renderer,
        (card.loc.x + 18, card.loc.y + (card.size.h - 18).max(0) / 2).into(),
        &message,
        2,
        contrast_text_rgb(fill),
        1.0,
    )? {
        foreground.push(SceneElement::UiText(text.element));
    }
    foreground.push(SceneElement::NodeLabel(node_renderer.label_element(
        renderer,
        card,
        halley_config::NodeShape::Squircle,
        fill,
        0.97,
    )?));
    Ok(foreground)
}
