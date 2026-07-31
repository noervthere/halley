use std::collections::HashMap;

use halley_core::field::NodeId;
use smithay::utils::{Logical, Rectangle};

use super::ClusterSystem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceSurfaceTarget {
    pub(crate) node_id: NodeId,
    pub(crate) geometry: Rectangle<i32, Logical>,
}

#[derive(Default)]
pub(super) struct WorkspaceSurfaceState {
    requested: HashMap<NodeId, Rectangle<i32, Logical>>,
    restore: HashMap<NodeId, Rectangle<i32, Logical>>,
}

impl WorkspaceSurfaceState {
    fn prepare(
        &mut self,
        node_id: NodeId,
        current: Rectangle<i32, Logical>,
        target: Rectangle<i32, Logical>,
    ) -> bool {
        self.restore.entry(node_id).or_insert(current);
        if self.requested.get(&node_id) == Some(&target) {
            return false;
        }
        self.requested.insert(node_id, target);
        true
    }

    fn forget(&mut self, node_id: NodeId) {
        self.requested.remove(&node_id);
        self.restore.remove(&node_id);
    }
}

impl ClusterSystem {
    pub(crate) fn workspace_surface_targets(
        &self,
        output: &str,
        work_area: Rectangle<i32, Logical>,
        output_geometry: Rectangle<i32, Logical>,
    ) -> Vec<WorkspaceSurfaceTarget> {
        let Some(cluster_id) = self.active_on(output) else {
            return Vec::new();
        };
        let Some(layout) = self.workspace_layout(cluster_id, work_area) else {
            return Vec::new();
        };
        layout
            .placements
            .into_iter()
            .filter(|placement| !self.floating.contains(&placement.node_id))
            .map(|placement| {
                let local = Rectangle::<i32, Logical>::new(
                    (
                        placement.rect.x.round() as i32,
                        placement.rect.y.round() as i32,
                    )
                        .into(),
                    (
                        placement.rect.w.round().max(1.0) as i32,
                        placement.rect.h.round().max(1.0) as i32,
                    )
                        .into(),
                );
                WorkspaceSurfaceTarget {
                    node_id: placement.node_id,
                    geometry: Rectangle::new(output_geometry.loc + local.loc, local.size),
                }
            })
            .collect()
    }

    pub(crate) fn prepare_surface_target(
        &mut self,
        node_id: NodeId,
        current: Rectangle<i32, Logical>,
        target: Rectangle<i32, Logical>,
    ) -> bool {
        self.surfaces.prepare(node_id, current, target)
    }

    pub(super) fn forget_surface_state(&mut self, node_id: NodeId) {
        self.surfaces.forget(node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use halley_core::field::{Field, Vec2};

    fn active_system(
        layout: halley_core::cluster::layout::ClusterWorkspaceLayoutKind,
    ) -> ClusterSystem {
        let mut field = Field::new();
        let first = field.spawn_surface(
            "first",
            Vec2 { x: 100.0, y: 100.0 },
            Vec2 { x: 640.0, y: 480.0 },
        );
        let second = field.spawn_surface(
            "second",
            Vec2 { x: 200.0, y: 100.0 },
            Vec2 { x: 640.0, y: 480.0 },
        );
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        system.begin_creation("DP-1".into());
        system.toggle_creation_member(first, "DP-1");
        system.toggle_creation_member(second, "DP-1");
        system.begin_naming();
        let cluster = system.finish_creation(&mut field).expect("cluster");
        system.metadata.get_mut(&cluster).expect("metadata").layout = layout;
        system.active.insert("DP-1".into(), cluster);
        system
    }

    #[test]
    fn unchanged_target_is_not_configured_twice() {
        let mut state = WorkspaceSurfaceState::default();
        let id = NodeId::new(4);
        let current = Rectangle::new((10, 20).into(), (800, 600).into());
        let target = Rectangle::new((0, 0).into(), (1_000, 700).into());

        assert!(state.prepare(id, current, target));
        assert!(!state.prepare(id, current, target));
        assert_eq!(state.restore.get(&id), Some(&current));
    }

    #[test]
    fn a_layout_change_emits_a_new_target_without_losing_restore_geometry() {
        let mut state = WorkspaceSurfaceState::default();
        let id = NodeId::new(9);
        let current = Rectangle::new((40, 80).into(), (640, 480).into());
        let first = Rectangle::new((0, 0).into(), (900, 700).into());
        let second = Rectangle::new((900, 0).into(), (500, 700).into());

        assert!(state.prepare(id, current, first));
        assert!(state.prepare(id, first, second));
        assert_eq!(state.restore.get(&id), Some(&current));
    }

    #[test]
    fn tiling_targets_are_output_global_and_keep_master_wider() {
        let system =
            active_system(halley_core::cluster::layout::ClusterWorkspaceLayoutKind::Tiling);
        let targets = system.workspace_surface_targets(
            "DP-1",
            Rectangle::new((0, 0).into(), (1_600, 900).into()),
            Rectangle::new((1_920, 0).into(), (1_600, 900).into()),
        );

        assert_eq!(targets.len(), 2);
        assert!(targets.iter().all(|target| target.geometry.loc.x >= 1_920));
        assert!(targets[0].geometry.size.w > targets[1].geometry.size.w);
    }

    #[test]
    fn stacking_targets_retain_distinct_native_client_sizes() {
        let system =
            active_system(halley_core::cluster::layout::ClusterWorkspaceLayoutKind::Stacking);
        let targets = system.workspace_surface_targets(
            "DP-1",
            Rectangle::new((0, 0).into(), (1_600, 900).into()),
            Rectangle::new((0, 0).into(), (1_600, 900).into()),
        );

        assert_eq!(targets.len(), 2);
        assert_ne!(targets[0].geometry.size, targets[1].geometry.size);
    }
}
