use super::*;
use crate::render::scene::nodes::{node_fill_color, node_ring_color};

pub(super) struct ClusterElementContext<'a> {
    pub(super) output: &'a Output,
    pub(super) output_geometry: Rectangle<i32, Logical>,
    pub(super) clusters: &'a crate::clusters::ClusterSystem,
    pub(super) nodes: &'a crate::nodes::NodesState,
    pub(super) cameras: &'a crate::presentation::camera::OutputCameras,
    pub(super) decorations: &'a halley_config::Decorations,
    pub(super) shadow_config: halley_config::ShadowLayer,
    pub(super) shadow_renderer: &'a mut crate::render::effects::shadow::ShadowRenderer,
}

pub(super) fn cluster_elements(
    renderer: &mut GlesRenderer,
    cluster_renderer: &mut crate::clusters::render::ClusterRenderer,
    context: ClusterElementContext<'_>,
) -> Result<Vec<StackGroup>, Box<dyn Error>> {
    let ClusterElementContext {
        output,
        output_geometry,
        clusters,
        nodes,
        cameras,
        decorations,
        shadow_config,
        shadow_renderer,
    } = context;
    if clusters.active_on(&output.name()).is_some() {
        return Ok(Vec::new());
    }
    let Some(camera) = cameras.get(&output.name()) else {
        return Ok(Vec::new());
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
    Ok(groups)
}

fn rgba(color: halley_config::BorderColor) -> [u8; 4] {
    [
        (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}
