use std::time::Duration;

use halley_core::cluster::ClusterId;
use halley_core::field::{Field, NodeId, Vec2};

use super::{ClusterSystem, JoinCandidate};

impl ClusterSystem {
    /// Tracks an ordinary Field window held near a collapsed cluster core.
    ///
    /// The candidate belongs to the cluster subsystem; pointer grabbing only
    /// supplies the current window center and never learns cluster policy.
    pub fn update_join_candidate(
        &mut self,
        output: &str,
        member: NodeId,
        center: Vec2,
        now: Duration,
    ) -> bool {
        if self.registry.is_cluster_member(member) || self.active_on(output).is_some() {
            return self.cancel_join_candidate();
        }
        let max_distance_sq = self.config.join_distance_px.max(0.0).powi(2);
        let target = self
            .clusters_for_output(output)
            .filter_map(|(_, id, metadata)| {
                let dx = center.x - metadata.core_position.x;
                let dy = center.y - metadata.core_position.y;
                let distance_sq = dx * dx + dy * dy;
                (distance_sq <= max_distance_sq).then_some((distance_sq, id))
            })
            .min_by(|(left, _), (right, _)| left.total_cmp(right))
            .map(|(_, id)| id);
        let Some(cluster_id) = target else {
            return self.cancel_join_candidate();
        };
        if self.join_candidate.as_ref().is_some_and(|candidate| {
            candidate.member == member
                && candidate.cluster_id == cluster_id
                && candidate.output == output
        }) {
            return false;
        }
        self.join_candidate = Some(JoinCandidate {
            member,
            cluster_id,
            output: output.to_string(),
            started_at: now,
        });
        true
    }

    pub fn cancel_join_candidate(&mut self) -> bool {
        self.join_candidate.take().is_some()
    }

    /// Completes a dwell join on button release. Releasing early cancels the
    /// candidate, so accidental passes over a core do not absorb a window.
    pub fn commit_join_candidate(
        &mut self,
        field: &mut Field,
        member: NodeId,
        now: Duration,
    ) -> Option<ClusterId> {
        let candidate = self.join_candidate.take()?;
        if candidate.member != member
            || now.saturating_sub(candidate.started_at)
                < Duration::from_millis(self.config.join_dwell_ms)
            || self.active_on(&candidate.output).is_some()
        {
            return None;
        }
        self.registry
            .add_member_to_cluster(field, candidate.cluster_id, member)
            .ok()?;
        Some(candidate.cluster_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(field: &mut Field, label: &str, x: f32) -> NodeId {
        field.spawn_surface(label, Vec2 { x, y: 100.0 }, Vec2 { x: 320.0, y: 200.0 })
    }

    #[test]
    fn dwell_join_requires_one_stable_candidate_and_release_after_deadline() {
        let mut field = Field::new();
        let first = surface(&mut field, "first", 80.0);
        let second = surface(&mut field, "second", 120.0);
        let joining = surface(&mut field, "joining", 500.0);
        let mut config = halley_config::Clusters::default();
        config.join_dwell_ms = 500;
        config.join_distance_px = 100.0;
        let mut system = ClusterSystem::new(config, halley_config::ClusterAnimation::default());
        assert!(system.begin_creation("DP-1".into()));
        assert!(system.toggle_creation_member(first, "DP-1"));
        assert!(system.toggle_creation_member(second, "DP-1"));
        assert!(system.begin_naming());
        let cluster = system.finish_creation(&mut field).unwrap();
        let core = system.metadata(cluster).unwrap().core_position;

        assert!(system.update_join_candidate("DP-1", joining, core, Duration::from_millis(100),));
        assert_eq!(
            system.commit_join_candidate(&mut field, joining, Duration::from_millis(599)),
            None
        );
        assert!(!system.registry().is_cluster_member(joining));

        assert!(system.update_join_candidate("DP-1", joining, core, Duration::from_millis(700),));
        assert_eq!(
            system.commit_join_candidate(&mut field, joining, Duration::from_millis(1_200)),
            Some(cluster)
        );
        assert!(system.registry().is_cluster_member(joining));
    }
}
