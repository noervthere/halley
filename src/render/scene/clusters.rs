use super::*;
use crate::render::scene::nodes::{node_fill_color, node_ring_color};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoreVisualFlags {
    identity_highlighted: bool,
    join_border_ready: bool,
}

fn core_visual_flags(focused: bool, hovered: bool, join_ready: bool) -> CoreVisualFlags {
    CoreVisualFlags {
        identity_highlighted: focused || hovered,
        join_border_ready: join_ready,
    }
}

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
    pub(super) now: std::time::Duration,
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
        now,
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
    let output_name = output.name();
    let join_readiness = clusters.join_readiness_on_output(&output_name);
    let icon_colors = [
        rgba(decorations.border_color_unfocused),
        rgba(decorations.border_color_focused),
    ];
    let mut groups = Vec::new();
    for (_, id, metadata) in clusters.clusters_for_output(&output.name()) {
        let focused = focused_node.is_some_and(|node| {
            clusters.cluster_for_member(node) == Some(id)
                || clusters.cluster_for_core(node) == Some(id)
        });
        let hovered = clusters.hovered_core() == Some(id);
        let bloom_open = clusters.bloom_open_on_output(&output.name()) == Some(id);
        let join_ready = join_readiness.is_some_and(|readiness| readiness.cluster_id == id);
        let visual_flags = core_visual_flags(focused, hovered, join_ready);
        let highlighted = visual_flags.identity_highlighted;
        let core_position = clusters
            .registry()
            .cluster(id)
            .and_then(|cluster| cluster.core_node())
            .map_or(metadata.core_position, |core| {
                nodes.landmark_position(core, metadata.core_position, now)
            });
        let center = crate::nodes::screen_from_world(core_position, camera, output_geometry);
        let local = center - output_geometry.loc;
        let side = crate::clusters::CORE_DIAMETER_PX.round() as i32;
        let destination = Rectangle::<i32, Physical>::new(
            (local.x - side / 2, local.y - side / 2).into(),
            (side, side).into(),
        );
        let ring = node_ring_color(decorations, highlighted);
        let core_border = if visual_flags.join_border_ready {
            let focused = decorations.border_color_focused;
            (focused.r, focused.g, focused.b)
        } else {
            ring
        };
        let fill = node_fill_color(nodes.config, ring);
        let mut elements = Vec::new();
        let hover_mix = if bloom_open {
            0.0
        } else {
            match (node_grab_active, nodes.config.show_labels) {
                (true, halley_config::NodeDisplayPolicy::Hover) => {
                    clusters.label_hover_mix(id, false)
                }
                (true, _) | (_, halley_config::NodeDisplayPolicy::Off) => 0.0,
                (false, halley_config::NodeDisplayPolicy::Hover) => {
                    clusters.label_hover_mix(id, hovered)
                }
                (false, halley_config::NodeDisplayPolicy::Always) => 1.0,
            }
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
            core_border,
            fill,
            nodes.config.opacity,
            visual_flags.join_border_ready,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_readiness_changes_only_the_core_border_highlight_state() {
        assert_eq!(
            core_visual_flags(false, false, true),
            CoreVisualFlags {
                identity_highlighted: false,
                join_border_ready: true,
            }
        );
    }
}
