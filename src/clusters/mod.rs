use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use halley_core::cluster::layout::ClusterWorkspaceLayoutKind;
use halley_core::cluster::layout::{ClusterWorkspaceLayoutResult, layout_cluster_workspace};
use halley_core::cluster::tiling::Rect as LayoutRect;
use halley_core::cluster::{ClusterId, ClusterRegistry};
use halley_core::field::{Field, NodeId, Vec2};
use smithay::utils::{Logical, Point, Rectangle};

mod bloom;
mod creation;
mod ipc;
mod membership;
mod overflow;
pub mod render;
mod surfaces;
mod transition;

pub use creation::{CreationState, NameInput};
pub use ipc::handle_request;

pub const CORE_DIAMETER_PX: f32 = 68.0;

#[derive(Clone, Debug)]
pub struct ClusterMetadata {
    pub name: String,
    pub output: String,
    pub layout: ClusterWorkspaceLayoutKind,
    pub core: Option<NodeId>,
    pub core_position: Vec2,
}

#[derive(Clone, Debug)]
struct JoinCandidate {
    member: NodeId,
    cluster_id: ClusterId,
    output: String,
    started_at: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowPresentation {
    Field,
    Hidden,
    Workspace {
        rect: Rectangle<i32, Logical>,
        depth: usize,
        alpha: f32,
    },
}

/// Owns every cluster-specific state transition. Field and Nodes remain
/// unaware of membership, slots, workspace modes, naming, and presentation.
pub struct ClusterSystem {
    registry: ClusterRegistry,
    metadata: HashMap<ClusterId, ClusterMetadata>,
    slots: HashMap<String, Vec<ClusterId>>,
    active: HashMap<String, ClusterId>,
    transitions: HashMap<String, transition::WorkspaceTransition>,
    reflows: HashMap<String, transition::ReflowTransition>,
    floating: HashSet<NodeId>,
    join_candidate: Option<JoinCandidate>,
    hovered_core: Option<ClusterId>,
    bloom: bloom::BloomState,
    label_hover: RefCell<HashMap<ClusterId, f32>>,
    creation: Option<CreationState>,
    surfaces: surfaces::WorkspaceSurfaceState,
    config: halley_config::Clusters,
    animations: halley_config::ClusterAnimation,
}

impl ClusterSystem {
    pub fn new(
        config: halley_config::Clusters,
        animations: halley_config::ClusterAnimation,
    ) -> Self {
        Self {
            registry: ClusterRegistry::new(),
            metadata: HashMap::new(),
            slots: HashMap::new(),
            active: HashMap::new(),
            transitions: HashMap::new(),
            reflows: HashMap::new(),
            floating: HashSet::new(),
            join_candidate: None,
            hovered_core: None,
            bloom: bloom::BloomState::default(),
            label_hover: RefCell::new(HashMap::new()),
            creation: None,
            surfaces: surfaces::WorkspaceSurfaceState::default(),
            config,
            animations,
        }
    }

    pub fn reload(
        &mut self,
        config: halley_config::Clusters,
        animations: halley_config::ClusterAnimation,
    ) -> bool {
        let changed = self.config != config || self.animations != animations;
        self.config = config;
        self.animations = animations;
        changed
    }

    pub fn config(&self) -> halley_config::Clusters {
        self.config
    }

    pub fn registry(&self) -> &ClusterRegistry {
        &self.registry
    }

    pub fn metadata(&self, id: ClusterId) -> Option<&ClusterMetadata> {
        self.metadata.get(&id)
    }

    pub fn active_on(&self, output: &str) -> Option<ClusterId> {
        self.active.get(output).copied()
    }

    pub fn hovered_core(&self) -> Option<ClusterId> {
        self.hovered_core
    }

    pub fn set_hovered_core(&mut self, hovered: Option<ClusterId>, now: Duration) -> bool {
        if self.hovered_core == hovered {
            return false;
        }
        self.hovered_core = hovered;
        let metadata = &self.metadata;
        self.bloom.set_hovered(hovered, now, |id| {
            metadata.get(&id).map(|metadata| metadata.output.clone())
        });
        true
    }

    pub fn move_core(&mut self, id: ClusterId, output: &str, position: Vec2) -> bool {
        if self.active.values().any(|active| *active == id) {
            return false;
        }
        let Some(previous_output) = self
            .metadata
            .get(&id)
            .map(|metadata| metadata.output.clone())
        else {
            return false;
        };
        if previous_output != output {
            let destination = self.slots.entry(output.to_string()).or_default();
            if destination.len() >= 10 {
                return false;
            }
            destination.push(id);
            if let Some(previous) = self.slots.get_mut(&previous_output) {
                previous.retain(|candidate| *candidate != id);
                if previous.is_empty() {
                    self.slots.remove(&previous_output);
                }
            }
        }
        let Some(metadata) = self.metadata.get_mut(&id) else {
            return false;
        };
        metadata.output = output.to_string();
        metadata.core_position = position;
        true
    }

    pub fn label_hover_mix(&self, id: ClusterId, highlighted: bool) -> f32 {
        let mut states = self.label_hover.borrow_mut();
        let mix = states.entry(id).or_insert(0.0);
        let target = if highlighted { 1.0 } else { 0.0 };
        let rate = if highlighted { 0.06 } else { 0.10 };
        *mix += (target - *mix) * rate;
        if (*mix - target).abs() < 0.002 {
            *mix = target;
        }
        *mix
    }

    pub fn labels_animating_on_output(
        &self,
        output: &str,
        policy: halley_config::NodeDisplayPolicy,
    ) -> bool {
        if policy != halley_config::NodeDisplayPolicy::Hover {
            return false;
        }
        let states = self.label_hover.borrow();
        self.clusters_for_output(output).any(|(_, id, _)| {
            let mix = states.get(&id).copied().unwrap_or(0.0);
            let target = if self.hovered_core == Some(id) {
                1.0
            } else {
                0.0
            };
            (mix - target).abs() > 0.002
        })
    }

    pub fn cycle_active_layout(
        &mut self,
        output: &str,
        work_area: Rectangle<i32, Logical>,
        now: Duration,
    ) -> bool {
        let Some(id) = self.active_on(output) else {
            return false;
        };
        let Some(before) = self.workspace_layout(id, work_area) else {
            return false;
        };
        let Some(metadata) = self.metadata.get_mut(&id) else {
            return false;
        };
        metadata.layout = match metadata.layout {
            ClusterWorkspaceLayoutKind::Tiling => ClusterWorkspaceLayoutKind::Stacking,
            ClusterWorkspaceLayoutKind::Stacking => ClusterWorkspaceLayoutKind::Tiling,
        };
        let duration_ms = match metadata.layout {
            ClusterWorkspaceLayoutKind::Tiling => self.animations.tiling.reflow_duration_ms,
            ClusterWorkspaceLayoutKind::Stacking => self.animations.stacking.cycle_duration_ms,
        };
        self.begin_reflow(output, id, before, now, duration_ms);
        true
    }

    pub fn cycle_stack(
        &mut self,
        output: &str,
        direction: halley_config::FocusCycleDirection,
        work_area: Rectangle<i32, Logical>,
        now: Duration,
    ) -> Option<NodeId> {
        let id = self.active_on(output)?;
        if self.metadata(id)?.layout != ClusterWorkspaceLayoutKind::Stacking {
            return None;
        }
        let before = self.workspace_layout(id, work_area)?;
        let member = self.registry.cycle_cluster_stacking_members(
            id,
            match direction {
                halley_config::FocusCycleDirection::Forward => {
                    halley_core::cluster::layout::ClusterCycleDirection::Next
                }
                halley_config::FocusCycleDirection::Backward => {
                    halley_core::cluster::layout::ClusterCycleDirection::Prev
                }
            },
        )?;
        self.begin_reflow(
            output,
            id,
            before,
            now,
            self.animations.stacking.cycle_duration_ms,
        );
        Some(member)
    }

    pub fn activate_slot(&mut self, output: &str, slot: u8, now: Duration) -> bool {
        let Some(id) = self
            .slots
            .get(output)
            .and_then(|slots| slots.get(usize::from(slot.saturating_sub(1))))
            .copied()
        else {
            return false;
        };
        self.close_bloom(output);
        self.set_hovered_core(None, now);
        if self.active_on(output) == Some(id) {
            self.active.remove(output);
            self.registry.deactivate_cluster_workspace(id);
            self.begin_transition(output, id, transition::TransitionKind::Closing, now);
        } else {
            if let Some(previous) = self.active.insert(output.to_string(), id) {
                self.registry.deactivate_cluster_workspace(previous);
            }
            self.registry.activate_cluster_workspace(id);
            self.begin_transition(output, id, transition::TransitionKind::Opening, now);
        }
        true
    }

    pub fn activate(&mut self, output: &str, id: ClusterId, now: Duration) -> bool {
        if self
            .metadata
            .get(&id)
            .is_none_or(|metadata| metadata.output != output)
        {
            return false;
        }
        self.close_bloom(output);
        self.set_hovered_core(None, now);
        if self.active_on(output) == Some(id) {
            self.active.remove(output);
            self.registry.deactivate_cluster_workspace(id);
            self.begin_transition(output, id, transition::TransitionKind::Closing, now);
        } else {
            if let Some(previous) = self.active.insert(output.to_string(), id) {
                self.registry.deactivate_cluster_workspace(previous);
            }
            self.registry.activate_cluster_workspace(id);
            self.begin_transition(output, id, transition::TransitionKind::Opening, now);
        }
        true
    }

    pub fn is_member(&self, id: NodeId) -> bool {
        self.registry.is_cluster_member(id)
    }

    pub fn cluster_for_member(&self, id: NodeId) -> Option<ClusterId> {
        self.registry.cluster_id_for_member(id)
    }

    pub fn active_layout_for_member(&self, id: NodeId) -> Option<ClusterWorkspaceLayoutKind> {
        let cluster = self.cluster_for_member(id)?;
        let metadata = self.metadata(cluster)?;
        (self.active_on(&metadata.output) == Some(cluster)).then_some(metadata.layout)
    }

    pub fn admit_mapped_window(
        &mut self,
        field: &mut Field,
        output: &str,
        member: NodeId,
        participation: halley_config::WindowClusterParticipation,
        work_area: Rectangle<i32, Logical>,
        now: Duration,
    ) -> bool {
        let Some(active) = self.active_on(output) else {
            return false;
        };
        if self.registry.is_cluster_member(member) {
            return false;
        }
        match participation {
            halley_config::WindowClusterParticipation::Float => self.floating.insert(member),
            halley_config::WindowClusterParticipation::Layout => {
                self.floating.remove(&member);
                let layout = self
                    .metadata(active)
                    .map(|metadata| metadata.layout)
                    .unwrap_or(ClusterWorkspaceLayoutKind::Tiling);
                let before = self.workspace_layout(active, work_area);
                let result = match layout {
                    ClusterWorkspaceLayoutKind::Stacking => self
                        .registry
                        .add_member_to_cluster_front(field, active, member),
                    ClusterWorkspaceLayoutKind::Tiling if self.config.tiling.new_on_top => self
                        .registry
                        .add_member_to_cluster_front(field, active, member),
                    ClusterWorkspaceLayoutKind::Tiling => {
                        self.registry.add_member_to_cluster(field, active, member)
                    }
                };
                if result.is_err() {
                    return false;
                }
                if layout == ClusterWorkspaceLayoutKind::Stacking
                    && let Some(before) = before
                {
                    self.begin_stack_insert_reflow(output, active, before, member, work_area, now);
                }
                true
            }
        }
    }

    pub fn window_presentation(
        &self,
        id: NodeId,
        output: &str,
        work_area: Rectangle<i32, Logical>,
        core: Option<Point<i32, Logical>>,
        now: Duration,
    ) -> WindowPresentation {
        let member_cluster = self.cluster_for_member(id);
        let active = self.active_on(output);
        let closing = self
            .transitions
            .get(output)
            .filter(|transition| {
                transition.kind == transition::TransitionKind::Closing
                    && self.transition_cluster_on(output, now) == Some(transition.cluster_id)
            })
            .map(|transition| transition.cluster_id);
        let Some(active) = active.or(closing) else {
            return if member_cluster.is_some() {
                WindowPresentation::Hidden
            } else {
                WindowPresentation::Field
            };
        };
        if self.floating.contains(&id) {
            return WindowPresentation::Field;
        }
        if member_cluster != Some(active) {
            return WindowPresentation::Hidden;
        }
        let Some(layout) = self.workspace_layout(active, work_area) else {
            return WindowPresentation::Hidden;
        };
        layout
            .placements
            .into_iter()
            .find(|placement| placement.node_id == id)
            .map(|placement| {
                let target = Rectangle::new(
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
                let visual = self.transition_visual(output, active, id, target, core, now);
                let rect = visual.map_or_else(
                    || {
                        self.reflow_visual(output, active, id, target, now)
                            .unwrap_or(target)
                    },
                    |visual| visual.rect,
                );
                WindowPresentation::Workspace {
                    rect,
                    depth: placement.depth,
                    alpha: visual.map_or(1.0, |visual| visual.alpha),
                }
            })
            .unwrap_or(WindowPresentation::Hidden)
    }

    pub fn directional_tile_target(
        &self,
        output: &str,
        current: Option<NodeId>,
        direction: halley_config::ClusterDirection,
        work_area: Rectangle<i32, Logical>,
    ) -> Option<NodeId> {
        let id = self.active_on(output)?;
        if self.metadata(id)?.layout != ClusterWorkspaceLayoutKind::Tiling {
            return None;
        }
        let layout = self.workspace_layout(id, work_area)?;
        let current = current
            .filter(|current| {
                layout
                    .placements
                    .iter()
                    .any(|tile| tile.node_id == *current)
            })
            .or_else(|| layout.placements.first().map(|tile| tile.node_id))?;
        let source = layout
            .placements
            .iter()
            .find(|tile| tile.node_id == current)?;
        let (sx, sy) = rect_center(source.rect);
        layout
            .placements
            .iter()
            .filter(|tile| tile.node_id != current)
            .filter_map(|tile| {
                let (tx, ty) = rect_center(tile.rect);
                let (primary, secondary) = match direction {
                    halley_config::ClusterDirection::Left if tx < sx => (sx - tx, (sy - ty).abs()),
                    halley_config::ClusterDirection::Right if tx > sx => (tx - sx, (sy - ty).abs()),
                    halley_config::ClusterDirection::Up if ty < sy => (sy - ty, (sx - tx).abs()),
                    halley_config::ClusterDirection::Down if ty > sy => (ty - sy, (sx - tx).abs()),
                    _ => return None,
                };
                Some((primary + secondary * 0.35, tile.node_id))
            })
            .min_by(|(a, _), (b, _)| a.total_cmp(b))
            .map(|(_, id)| id)
    }

    pub fn swap_directional_tile(
        &mut self,
        output: &str,
        current: Option<NodeId>,
        direction: halley_config::ClusterDirection,
        work_area: Rectangle<i32, Logical>,
        now: Duration,
    ) -> Option<NodeId> {
        let id = self.active_on(output)?;
        let before = self.workspace_layout(id, work_area)?;
        let current = current.or_else(|| self.first_member(id))?;
        let target = self.directional_tile_target(output, Some(current), direction, work_area)?;
        let mut members = self.registry.cluster(id)?.members().to_vec();
        let current_index = members.iter().position(|member| *member == current)?;
        let target_index = members.iter().position(|member| *member == target)?;
        members.swap(current_index, target_index);
        self.registry.reorder_cluster_members(id, members).ok()?;
        self.begin_reflow(
            output,
            id,
            before,
            now,
            self.animations.tiling.reflow_duration_ms,
        );
        Some(current)
    }

    fn workspace_layout(
        &self,
        id: ClusterId,
        work_area: Rectangle<i32, Logical>,
    ) -> Option<ClusterWorkspaceLayoutResult> {
        let cluster = self.registry.cluster(id)?;
        let metadata = self.metadata(id)?;
        let outer = self.config.tiling.gaps_outer_px.max(0.0);
        let bounds = LayoutRect {
            x: work_area.loc.x as f32 + outer,
            y: work_area.loc.y as f32 + outer,
            w: (work_area.size.w as f32 - outer * 2.0).max(1.0),
            h: (work_area.size.h as f32 - outer * 2.0).max(1.0),
        };
        let limit = match metadata.layout {
            ClusterWorkspaceLayoutKind::Tiling => self.config.tiling.max_stack,
            ClusterWorkspaceLayoutKind::Stacking => self.config.stacking.max_visible,
        };
        Some(layout_cluster_workspace(
            metadata.layout,
            bounds,
            self.config.tiling.gaps_inner_px,
            self.config.tiling.gaps_inner_px,
            cluster.members(),
            limit,
        ))
    }

    pub fn clusters_for_output(
        &self,
        output: &str,
    ) -> impl Iterator<Item = (u8, ClusterId, &ClusterMetadata)> {
        self.slots
            .get(output)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, id)| {
                Some((u8::try_from(index + 1).ok()?, *id, self.metadata.get(id)?))
            })
    }

    pub fn core_hit_test(
        &self,
        output: &str,
        camera: &halley_core::camera::Camera,
        output_geometry: Rectangle<i32, Logical>,
        screen: Point<f64, Logical>,
    ) -> Option<ClusterId> {
        if self.active_on(output).is_some() {
            return None;
        }
        self.clusters_for_output(output)
            .filter_map(|(_, id, metadata)| {
                let center = crate::nodes::screen_from_world(
                    metadata.core_position,
                    camera,
                    output_geometry,
                );
                let diameter = CORE_DIAMETER_PX.round() as i32;
                Rectangle::new(
                    (center.x - diameter / 2, center.y - diameter / 2).into(),
                    (diameter, diameter).into(),
                )
                .to_f64()
                .contains(screen)
                .then_some(id)
            })
            .max_by_key(|id| id.as_u64())
    }

    pub fn first_member(&self, id: ClusterId) -> Option<NodeId> {
        self.registry.cluster(id)?.members().first().copied()
    }

    /// Removes one destroyed window from cluster bookkeeping before
    /// `NodesState` discards its Field node. A remapped/unmapped window is
    /// intentionally retained; only final surface destruction reaches here.
    pub fn forget_destroyed_member(&mut self, field: &mut Field, member: NodeId) -> bool {
        self.forget_surface_state(member);
        if self
            .join_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.member == member)
        {
            self.join_candidate = None;
        }
        let Some(id) = self.registry.cluster_id_for_member(member) else {
            self.floating.remove(&member);
            if let Some(creation) = self.creation.as_mut() {
                creation.selected.remove(&member);
            }
            return false;
        };
        let Some((_, effect)) = self.registry.remove_node_cluster_safe(field, member) else {
            return false;
        };
        if matches!(
            effect,
            Some(halley_core::cluster::RemoveNodeClusterEffect::DissolvedCluster(_))
        ) {
            self.remove_cluster_metadata(id);
        }
        self.floating.remove(&member);
        if let Some(creation) = self.creation.as_mut() {
            creation.selected.remove(&member);
        }
        true
    }

    fn remove_cluster_metadata(&mut self, id: ClusterId) {
        if self.hovered_core == Some(id) {
            self.hovered_core = None;
        }
        self.label_hover.borrow_mut().remove(&id);
        self.bloom.remove_cluster(id);
        let output = self.metadata.remove(&id).map(|metadata| metadata.output);
        if let Some(output) = output {
            if let Some(slots) = self.slots.get_mut(&output) {
                slots.retain(|candidate| *candidate != id);
                if slots.is_empty() {
                    self.slots.remove(&output);
                }
            }
            if self.active.get(&output) == Some(&id) {
                self.active.remove(&output);
            }
        }
    }

    fn slot_of(&self, output: &str, id: ClusterId) -> Option<u8> {
        self.slots
            .get(output)?
            .iter()
            .position(|candidate| *candidate == id)
            .and_then(|index| u8::try_from(index + 1).ok())
    }
}

fn rect_center(rect: LayoutRect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_is_single_modal_state() {
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        assert!(system.begin_creation("DP-1".into()));
        assert!(!system.begin_creation("DP-2".into()));
        assert_eq!(system.creation().unwrap().output, "DP-1");
        assert!(system.cancel_creation());
        assert!(system.creation().is_none());
    }

    #[test]
    fn cluster_core_uses_the_old_larger_hit_area_and_hover_animation() {
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        let id = ClusterId::new(1);
        system.metadata.insert(
            id,
            ClusterMetadata {
                name: "Cluster 1".into(),
                output: "DP-1".into(),
                layout: ClusterWorkspaceLayoutKind::Tiling,
                core: None,
                core_position: Vec2 { x: 0.0, y: 0.0 },
            },
        );
        system.slots.insert("DP-1".into(), vec![id]);
        let camera = halley_core::camera::Camera::new(
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 {
                x: 1920.0,
                y: 1080.0,
            },
        );
        let output_geometry = Rectangle::new((0, 0).into(), (1920, 1080).into());

        assert_eq!(
            system.core_hit_test(
                "DP-1",
                &camera,
                output_geometry,
                (960.0 + CORE_DIAMETER_PX as f64 / 2.0 - 1.0, 540.0).into(),
            ),
            Some(id)
        );
        assert_eq!(
            system.core_hit_test(
                "DP-1",
                &camera,
                output_geometry,
                (960.0 + CORE_DIAMETER_PX as f64 / 2.0 + 1.0, 540.0).into(),
            ),
            None
        );
        assert!(system.set_hovered_core(Some(id), Duration::ZERO));
        assert!(system.labels_animating_on_output("DP-1", halley_config::NodeDisplayPolicy::Hover));
        assert!(system.label_hover_mix(id, true) > 0.0);
    }

    #[test]
    fn collapsed_cluster_core_moves_as_one_cluster_owned_landmark() {
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        let id = ClusterId::new(1);
        system.metadata.insert(
            id,
            ClusterMetadata {
                name: "Cluster 1".into(),
                output: "DP-1".into(),
                layout: ClusterWorkspaceLayoutKind::Tiling,
                core: None,
                core_position: Vec2 { x: 10.0, y: 20.0 },
            },
        );
        system.slots.insert("DP-1".into(), vec![id]);

        assert!(system.move_core(id, "DP-1", Vec2 { x: 30.0, y: 40.0 }));
        assert_eq!(
            system.metadata(id).unwrap().core_position,
            Vec2 { x: 30.0, y: 40.0 }
        );

        assert!(system.move_core(id, "DP-2", Vec2 { x: 50.0, y: 60.0 }));
        assert_eq!(system.metadata(id).unwrap().output, "DP-2");
        assert_eq!(
            system
                .clusters_for_output("DP-1")
                .next()
                .map(|(_, id, _)| id),
            None
        );
        assert_eq!(
            system
                .clusters_for_output("DP-2")
                .next()
                .map(|(_, id, _)| id),
            Some(id)
        );

        system.active.insert("DP-2".into(), id);
        assert!(!system.move_core(id, "DP-1", Vec2 { x: 0.0, y: 0.0 }));
        assert_eq!(system.metadata(id).unwrap().output, "DP-2");
    }

    #[test]
    fn destroyed_members_reflow_then_retire_cluster_metadata() {
        let mut field = Field::new();
        let a = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 100.0, y: 80.0 });
        let b = field.spawn_surface("B", Vec2 { x: 200.0, y: 0.0 }, Vec2 { x: 100.0, y: 80.0 });
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        assert!(system.begin_creation("DP-1".into()));
        assert!(system.toggle_creation_member(a, "DP-1"));
        assert!(system.toggle_creation_member(b, "DP-1"));
        assert!(system.begin_naming());
        assert!(system.edit_name(NameInput::Character('W')));
        let id = system.finish_creation(&mut field).unwrap();
        assert!(system.activate_slot("DP-1", 1, Duration::ZERO));

        assert!(system.forget_destroyed_member(&mut field, a));
        assert_eq!(system.registry().cluster(id).unwrap().members(), &[b]);
        assert_eq!(system.active_on("DP-1"), Some(id));

        assert!(system.forget_destroyed_member(&mut field, b));
        assert!(system.registry().cluster(id).is_none());
        assert!(system.metadata(id).is_none());
        assert!(system.clusters_for_output("DP-1").next().is_none());
        assert!(system.active_on("DP-1").is_none());
    }

    #[test]
    fn directional_tiling_focus_and_swap_share_the_layout_snapshot() {
        let mut field = Field::new();
        let ids = (0..3)
            .map(|index| {
                field.spawn_surface(
                    format!("W{index}"),
                    Vec2 {
                        x: index as f32 * 120.0,
                        y: 0.0,
                    },
                    Vec2 { x: 100.0, y: 80.0 },
                )
            })
            .collect::<Vec<_>>();
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        assert!(system.begin_creation("DP-1".into()));
        for id in &ids {
            assert!(system.toggle_creation_member(*id, "DP-1"));
        }
        assert!(system.begin_naming());
        let cluster = system.finish_creation(&mut field).unwrap();
        system.metadata.get_mut(&cluster).unwrap().layout = ClusterWorkspaceLayoutKind::Tiling;
        assert!(system.activate_slot("DP-1", 1, Duration::ZERO));
        let work_area = Rectangle::new((0, 0).into(), (1_000, 700).into());

        assert_eq!(
            system.directional_tile_target(
                "DP-1",
                Some(ids[0]),
                halley_config::ClusterDirection::Right,
                work_area,
            ),
            Some(ids[1])
        );
        assert_eq!(
            system.swap_directional_tile(
                "DP-1",
                Some(ids[0]),
                halley_config::ClusterDirection::Right,
                work_area,
                Duration::ZERO,
            ),
            Some(ids[0])
        );
        assert_eq!(system.registry().cluster(cluster).unwrap().master(), ids[1]);

        let joined = field.spawn_surface(
            "joined",
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 100.0, y: 80.0 },
        );
        assert!(system.admit_mapped_window(
            &mut field,
            "DP-1",
            joined,
            halley_config::WindowClusterParticipation::Layout,
            work_area,
            Duration::from_secs(2),
        ));
        assert!(system.registry().cluster(cluster).unwrap().contains(joined));

        let floating = field.spawn_surface(
            "floating",
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 100.0, y: 80.0 },
        );
        assert!(system.admit_mapped_window(
            &mut field,
            "DP-1",
            floating,
            halley_config::WindowClusterParticipation::Float,
            work_area,
            Duration::from_secs(2),
        ));
        assert_eq!(
            system.window_presentation(floating, "DP-1", work_area, None, Duration::MAX),
            WindowPresentation::Field
        );
    }

    #[test]
    fn stacking_admission_pushes_the_front_back_and_enters_from_the_left() {
        let mut field = Field::new();
        let first =
            field.spawn_surface("first", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 100.0, y: 80.0 });
        let second = field.spawn_surface(
            "second",
            Vec2 { x: 120.0, y: 0.0 },
            Vec2 { x: 100.0, y: 80.0 },
        );
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        assert!(system.begin_creation("DP-1".into()));
        assert!(system.toggle_creation_member(first, "DP-1"));
        assert!(system.toggle_creation_member(second, "DP-1"));
        assert!(system.begin_naming());
        let cluster = system.finish_creation(&mut field).unwrap();
        assert!(system.activate("DP-1", cluster, Duration::ZERO));
        let work_area = Rectangle::new((0, 0).into(), (1_000, 700).into());
        let inserted = field.spawn_surface(
            "inserted",
            Vec2 { x: 240.0, y: 0.0 },
            Vec2 { x: 100.0, y: 80.0 },
        );
        let started = Duration::from_secs(2);

        assert!(system.admit_mapped_window(
            &mut field,
            "DP-1",
            inserted,
            halley_config::WindowClusterParticipation::Layout,
            work_area,
            started,
        ));
        assert_eq!(
            system.registry().cluster(cluster).unwrap().members()[0],
            inserted
        );
        let target = system
            .workspace_layout(cluster, work_area)
            .unwrap()
            .placements
            .into_iter()
            .find(|placement| placement.node_id == inserted)
            .unwrap()
            .rect;
        let WindowPresentation::Workspace { rect, depth, .. } =
            system.window_presentation(inserted, "DP-1", work_area, None, started)
        else {
            panic!("inserted stack member should be visible");
        };
        assert!(rect.loc.x < target.x.round() as i32);
        assert_eq!(depth, 2);
    }
}
