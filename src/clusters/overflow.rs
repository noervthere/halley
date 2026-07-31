use halley_core::field::NodeId;
use smithay::utils::{Logical, Point, Rectangle};

use super::ClusterSystem;

const STRIP_WIDTH: i32 = 68;
const STRIP_PADDING: i32 = 10;
const ITEM_SIZE: i32 = 44;
const ITEM_GAP: i32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverflowItem {
    pub node_id: NodeId,
    pub rect: Rectangle<i32, Logical>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverflowLayout {
    pub strip: Rectangle<i32, Logical>,
    pub items: Vec<OverflowItem>,
}

impl ClusterSystem {
    /// Returns the output-local queue geometry shared by rendering and input.
    ///
    /// Keeping this layout here prevents the compositor overlay from owning
    /// cluster policy or reconstructing hit boxes independently.
    pub fn overflow_layout(
        &self,
        output: &str,
        work_area: Rectangle<i32, Logical>,
    ) -> Option<OverflowLayout> {
        let active = self.active_on(output)?;
        let layout = self.workspace_layout(active, work_area)?;
        if layout.queue_members.is_empty() {
            return None;
        }

        let available_height = (work_area.size.h - STRIP_PADDING * 2).max(ITEM_SIZE);
        let visible_count = ((available_height + ITEM_GAP) / (ITEM_SIZE + ITEM_GAP))
            .max(1)
            .min(layout.queue_members.len() as i32) as usize;
        let content_height =
            visible_count as i32 * ITEM_SIZE + visible_count.saturating_sub(1) as i32 * ITEM_GAP;
        let strip_height = content_height + STRIP_PADDING * 2;
        let strip = Rectangle::new(
            (
                work_area.loc.x + work_area.size.w - STRIP_WIDTH,
                work_area.loc.y + (work_area.size.h - strip_height) / 2,
            )
                .into(),
            (STRIP_WIDTH, strip_height).into(),
        );
        let item_x = strip.loc.x + (strip.size.w - ITEM_SIZE) / 2;
        let items = layout
            .queue_members
            .into_iter()
            .take(visible_count)
            .enumerate()
            .map(|(index, node_id)| OverflowItem {
                node_id,
                rect: Rectangle::new(
                    (
                        item_x,
                        strip.loc.y + STRIP_PADDING + index as i32 * (ITEM_SIZE + ITEM_GAP),
                    )
                        .into(),
                    (ITEM_SIZE, ITEM_SIZE).into(),
                ),
            })
            .collect();
        Some(OverflowLayout { strip, items })
    }

    pub fn overflow_hit_test(
        &self,
        output: &str,
        work_area: Rectangle<i32, Logical>,
        output_local: Point<f64, Logical>,
    ) -> Option<NodeId> {
        self.overflow_layout(output, work_area)?
            .items
            .into_iter()
            .find_map(|item| {
                item.rect
                    .to_f64()
                    .contains(output_local)
                    .then_some(item.node_id)
            })
    }

    /// Promotes a queued tile into the last visible stack position. The
    /// master remains stable, which avoids an unrelated workspace reshuffle.
    pub fn promote_overflow_member(
        &mut self,
        output: &str,
        member: NodeId,
        work_area: Rectangle<i32, Logical>,
        now: std::time::Duration,
    ) -> bool {
        let Some(active) = self.active_on(output) else {
            return false;
        };
        let Some(before) = self.workspace_layout(active, work_area) else {
            return false;
        };
        let Some(cluster) = self.registry.cluster(active) else {
            return false;
        };
        let visible_limit = self.config.tiling.max_stack.saturating_add(1);
        if self.config.tiling.max_stack == 0
            || cluster.members().len() <= visible_limit
            || !cluster.members()[visible_limit..].contains(&member)
        {
            return false;
        }
        let visible_member = cluster.members()[visible_limit - 1];
        let changed = self.registry.swap_cluster_overflow_member_with_visible(
            active,
            member,
            visible_member,
            self.config.tiling.max_stack,
        );
        if changed {
            self.begin_reflow(
                output,
                active,
                before,
                now,
                self.animations.tiling.reflow_duration_ms,
            );
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use halley_core::field::{Field, Vec2};

    use super::*;

    fn surface(field: &mut Field, label: &str) -> NodeId {
        field.spawn_surface(
            label,
            Vec2 { x: 20.0, y: 20.0 },
            Vec2 { x: 320.0, y: 200.0 },
        )
    }

    #[test]
    fn queue_geometry_and_promotion_share_one_member_order() {
        let mut field = Field::new();
        let members = (0..7)
            .map(|index| surface(&mut field, &format!("window-{index}")))
            .collect::<Vec<_>>();
        let mut config = halley_config::Clusters::default();
        config.default_layout = halley_config::ClusterLayout::Tiling;
        config.tiling.max_stack = 2;
        let mut system = ClusterSystem::new(config, halley_config::ClusterAnimation::default());
        system.begin_creation("DP-1".into());
        for member in &members {
            assert!(system.toggle_creation_member(*member, "DP-1"));
        }
        assert!(system.begin_naming());
        let cluster = system.finish_creation(&mut field).unwrap();
        assert!(system.activate("DP-1", cluster, Duration::ZERO));

        let work_area = Rectangle::new((0, 0).into(), (1280, 720).into());
        let queue = system.overflow_layout("DP-1", work_area).unwrap();
        assert_eq!(
            queue
                .items
                .iter()
                .map(|item| item.node_id)
                .collect::<Vec<_>>(),
            members[3..].to_vec()
        );
        let promoted = queue.items[1].node_id;
        assert!(system.promote_overflow_member("DP-1", promoted, work_area, Duration::ZERO,));
        assert_eq!(
            system.registry().cluster(cluster).unwrap().members()[2],
            promoted
        );
    }
}
