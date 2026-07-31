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
    pub(super) node_grab_active: bool,
    pub(super) node_renderer: &'a mut crate::render::node::NodeRenderer,
    pub(super) ui_text: &'a mut crate::render::text::UiTextRenderer,
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
        node_grab_active,
        node_renderer,
        ui_text,
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
        let hovered = clusters.hovered_core() == Some(id);
        let highlighted = focused || hovered;
        let center =
            crate::nodes::screen_from_world(metadata.core_position, camera, output_geometry);
        let local = center - output_geometry.loc;
        let side = crate::clusters::CORE_DIAMETER_PX.round() as i32;
        let destination = Rectangle::<i32, Physical>::new(
            (local.x - side / 2, local.y - side / 2).into(),
            (side, side).into(),
        );
        let ring = node_ring_color(nodes.config, decorations, highlighted);
        let fill = node_fill_color(nodes.config, ring);
        let mut elements = Vec::new();
        let hover_mix = match (node_grab_active, nodes.config.show_labels) {
            (true, halley_config::NodeDisplayPolicy::Hover) => clusters.label_hover_mix(id, false),
            (true, _) | (_, halley_config::NodeDisplayPolicy::Off) => 0.0,
            (false, halley_config::NodeDisplayPolicy::Hover) => {
                clusters.label_hover_mix(id, hovered)
            }
            (false, halley_config::NodeDisplayPolicy::Always) => 1.0,
        };
        elements.extend(super::nodes::landmark_label_elements(
            renderer,
            node_renderer,
            ui_text,
            super::nodes::LandmarkLabel {
                center: (local.x, local.y),
                marker_side: side,
                output_size: (output_geometry.size.w, output_geometry.size.h),
                text: &metadata.name,
                shape: nodes.config.label_shape,
                fill,
                ring,
                hover_mix,
                alpha: 1.0,
            },
        )?);
        if clusters.config().show_icons {
            let icon_side =
                ((side as f32 * nodes.config.icon_size * 0.98).round() as i32).clamp(16, 42);
            elements.push(SceneElement::ClusterIcon(cluster_renderer.icon(
                renderer,
                Rectangle::new(
                    (local.x - icon_side / 2, local.y - icon_side / 2).into(),
                    (icon_side, icon_side).into(),
                ),
                highlighted,
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
