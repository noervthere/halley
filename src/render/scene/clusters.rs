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
    let mut overlay = Vec::new();
    overlay.extend(overflow_elements(
        renderer,
        output,
        clusters,
        nodes,
        decorations,
        node_renderer,
        ui_text,
    )?);
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

#[allow(clippy::too_many_arguments)]
fn overflow_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    clusters: &crate::clusters::ClusterSystem,
    nodes: &crate::nodes::NodesState,
    decorations: &halley_config::Decorations,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let work_area = smithay::desktop::layer_map_for_output(output).non_exclusive_zone();
    let Some(layout) = clusters.overflow_layout(&output.name(), work_area) else {
        return Ok(Vec::new());
    };
    let fill = node_fill_color(
        nodes.config,
        (
            decorations.border_color_unfocused.r,
            decorations.border_color_unfocused.g,
            decorations.border_color_unfocused.b,
        ),
    );
    let mut elements = vec![SceneElement::NodeLabel(node_renderer.label_element(
        renderer,
        layout.strip.to_physical(1),
        halley_config::NodeShape::Squircle,
        fill,
        0.94,
    )?)];
    for item in layout.items {
        let rect = item.rect.to_physical(1);
        elements.push(SceneElement::NodeLabel(node_renderer.label_element(
            renderer,
            rect,
            halley_config::NodeShape::Squircle,
            fill,
            0.98,
        )?));
        let record = nodes.record(item.node_id);
        let icon = clusters
            .config()
            .tiling
            .overflow_show_icons
            .then(|| record.and_then(|record| record.app_id.as_deref()))
            .flatten()
            .and_then(|app_id| {
                node_renderer.request_app_icon(renderer, app_id);
                node_renderer.app_icon_element(
                    renderer,
                    app_id,
                    Rectangle::new((rect.loc.x + 5, rect.loc.y + 5).into(), (34, 34).into()),
                    1.0,
                )
            });
        if let Some(icon) = icon {
            elements.push(SceneElement::NodeTexture(icon));
            continue;
        }
        let glyph = record
            .and_then(|record| record.app_id.as_deref())
            .or_else(|| {
                nodes
                    .field
                    .node(item.node_id)
                    .map(|node| node.label.as_str())
            })
            .and_then(|label| label.chars().find(char::is_ascii_alphanumeric))
            .map(|ch| ch.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        let color = contrast_text_rgb(fill);
        if let Some(size) = ui_text.measure(renderer, &glyph, 2, color)?
            && let Some(text) = ui_text.element(
                renderer,
                (
                    rect.loc.x + (rect.size.w - size.w) / 2,
                    rect.loc.y + (rect.size.h - size.h) / 2,
                )
                    .into(),
                &glyph,
                2,
                color,
                1.0,
            )?
        {
            elements.push(SceneElement::UiText(text.element));
        }
    }
    Ok(elements)
}

fn rgba(color: halley_config::BorderColor) -> [u8; 4] {
    [
        (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}
