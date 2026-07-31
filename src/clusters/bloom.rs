use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::time::Duration;

use halley_core::camera::Camera;
use halley_core::cluster::ClusterId;
use halley_core::field::NodeId;
use smithay::utils::{Logical, Point, Rectangle};

use super::ClusterSystem;

pub const HOLD_DURATION: Duration = Duration::from_millis(1_700);
const OPEN_DURATION: Duration = Duration::from_millis(220);
pub const TOKEN_RADIUS_PX: i32 = 24;

#[derive(Clone, Copy, Debug)]
struct PendingHover {
    cluster_id: ClusterId,
    started_at: Duration,
}

#[derive(Clone, Copy, Debug)]
struct OpenBloom {
    cluster_id: ClusterId,
    opened_at: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TokenLayout {
    pub cluster_id: ClusterId,
    pub member_id: NodeId,
    pub center: Point<i32, Logical>,
    pub core_center: Point<i32, Logical>,
    pub radius: i32,
    pub alpha: f32,
}

#[derive(Default)]
pub(super) struct BloomState {
    pending_hover: Option<PendingHover>,
    open: HashMap<String, OpenBloom>,
}

impl BloomState {
    pub(super) fn set_hovered(
        &mut self,
        hovered: Option<ClusterId>,
        now: Duration,
        output_for: impl FnOnce(ClusterId) -> Option<String>,
    ) {
        let Some(cluster_id) = hovered else {
            self.pending_hover = None;
            return;
        };
        if self.open.values().any(|open| open.cluster_id == cluster_id) {
            self.pending_hover = None;
            return;
        }
        if self
            .pending_hover
            .is_some_and(|pending| pending.cluster_id == cluster_id)
        {
            return;
        }
        self.pending_hover = output_for(cluster_id).map(|_| PendingHover {
            cluster_id,
            started_at: now,
        });
    }

    pub(super) fn wakeup(
        &mut self,
        now: Duration,
        output_for: impl FnOnce(ClusterId) -> Option<String>,
        cluster_is_active: impl FnOnce(&str, ClusterId) -> bool,
    ) -> bool {
        let Some(pending) = self.pending_hover else {
            return false;
        };
        if now.saturating_sub(pending.started_at) < HOLD_DURATION {
            return false;
        }
        self.pending_hover = None;
        let Some(output) = output_for(pending.cluster_id) else {
            return false;
        };
        if cluster_is_active(&output, pending.cluster_id) {
            return false;
        }
        self.open.insert(
            output,
            OpenBloom {
                cluster_id: pending.cluster_id,
                opened_at: now,
            },
        );
        true
    }

    pub(super) fn close(&mut self, output: &str) -> bool {
        self.open.remove(output).is_some()
    }

    pub(super) fn remove_cluster(&mut self, cluster_id: ClusterId) {
        if self
            .pending_hover
            .is_some_and(|pending| pending.cluster_id == cluster_id)
        {
            self.pending_hover = None;
        }
        self.open.retain(|_, bloom| bloom.cluster_id != cluster_id);
    }

    fn snapshot(&self, output: &str, now: Duration) -> Option<(ClusterId, f32)> {
        let bloom = self.open.get(output)?;
        let progress = if OPEN_DURATION.is_zero() {
            1.0
        } else {
            now.saturating_sub(bloom.opened_at).as_secs_f32() / OPEN_DURATION.as_secs_f32()
        }
        .clamp(0.0, 1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        Some((bloom.cluster_id, eased))
    }

    pub(super) fn is_animating(&self, output: &str, now: Duration) -> bool {
        self.open
            .get(output)
            .is_some_and(|bloom| now.saturating_sub(bloom.opened_at) < OPEN_DURATION)
    }
}

impl ClusterSystem {
    pub fn bloom_wakeup(&mut self, now: Duration) -> bool {
        let metadata = &self.metadata;
        let active = &self.active;
        self.bloom.wakeup(
            now,
            |id| metadata.get(&id).map(|metadata| metadata.output.clone()),
            |output, id| active.get(output) == Some(&id),
        )
    }

    pub fn bloom_is_animating_on_output(&self, output: &str, now: Duration) -> bool {
        self.bloom.is_animating(output, now)
    }

    pub fn close_bloom(&mut self, output: &str) -> bool {
        self.bloom.close(output)
    }

    pub fn bloom_layout(
        &self,
        output: &str,
        camera: &Camera,
        output_geometry: Rectangle<i32, Logical>,
        now: Duration,
    ) -> Vec<TokenLayout> {
        let Some((cluster_id, mix)) = self.bloom.snapshot(output, now) else {
            return Vec::new();
        };
        if self.active_on(output).is_some() {
            return Vec::new();
        }
        let Some(metadata) = self.metadata(cluster_id) else {
            return Vec::new();
        };
        let Some(cluster) = self.registry.cluster(cluster_id) else {
            return Vec::new();
        };
        let core_center =
            crate::nodes::screen_from_world(metadata.core_position, camera, output_geometry);
        let mut members = cluster.members().to_vec();
        members.sort_by_key(|id| id.as_u64());
        let count = members.len().max(1);
        let slots = count.max(10);
        let angle_step = TAU / slots as f32;
        let min_chord = TOKEN_RADIUS_PX as f32 * 2.0 + 18.0;
        let radius = (min_chord / (2.0 * (angle_step * 0.5).sin()).max(0.20)).max(84.0)
            + (count as f32 - 1.0).min(5.0) * 3.0;
        let direction = match self.config.bloom_direction {
            halley_config::ClusterBloomDirection::Clockwise => 1.0,
            halley_config::ClusterBloomDirection::CounterClockwise => -1.0,
        };
        members
            .into_iter()
            .enumerate()
            .map(|(index, member_id)| {
                let angle = -PI * 0.5 + direction * angle_step * index as f32;
                TokenLayout {
                    cluster_id,
                    member_id,
                    center: (
                        core_center.x + (angle.cos() * radius * mix).round() as i32,
                        core_center.y + (angle.sin() * radius * mix).round() as i32,
                    )
                        .into(),
                    core_center,
                    radius: TOKEN_RADIUS_PX,
                    alpha: mix,
                }
            })
            .collect()
    }

    pub fn bloom_hit_test(
        &self,
        output: &str,
        camera: &Camera,
        output_geometry: Rectangle<i32, Logical>,
        point: Point<f64, Logical>,
        now: Duration,
    ) -> Option<TokenLayout> {
        self.bloom_layout(output, camera, output_geometry, now)
            .into_iter()
            .find(|token| {
                let dx = point.x - f64::from(token.center.x);
                let dy = point.y - f64::from(token.center.y);
                dx * dx + dy * dy <= f64::from(token.radius * token.radius)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use halley_core::field::{Field, Vec2};

    fn clustered_system() -> (ClusterSystem, ClusterId) {
        let mut field = Field::new();
        let first = field.spawn_surface(
            "first",
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 { x: 200.0, y: 120.0 },
        );
        let second = field.spawn_surface(
            "second",
            Vec2 { x: 220.0, y: 0.0 },
            Vec2 { x: 200.0, y: 120.0 },
        );
        let mut system = ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        );
        assert!(system.begin_creation("DP-1".into()));
        assert!(system.toggle_creation_member(first, "DP-1"));
        assert!(system.toggle_creation_member(second, "DP-1"));
        assert!(system.begin_naming());
        let id = system.finish_creation(&mut field).unwrap();
        (system, id)
    }

    #[test]
    fn bloom_opens_only_after_the_old_hover_dwell() {
        let (mut system, id) = clustered_system();
        system.set_hovered_core(Some(id), Duration::from_millis(100));
        assert!(!system.bloom_wakeup(Duration::from_millis(1_799)));
        assert!(system.bloom_wakeup(Duration::from_millis(1_800)));
        assert!(system.bloom_is_animating_on_output("DP-1", Duration::from_millis(1_801)));
    }

    #[test]
    fn clockwise_bloom_starts_above_the_core_and_advances_right() {
        let (mut system, id) = clustered_system();
        system.set_hovered_core(Some(id), Duration::ZERO);
        assert!(system.bloom_wakeup(HOLD_DURATION));
        let camera = Camera::new(
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 {
                x: 1_920.0,
                y: 1_080.0,
            },
        );
        let geometry = Rectangle::new((0, 0).into(), (1_920, 1_080).into());
        let layout = system.bloom_layout("DP-1", &camera, geometry, HOLD_DURATION + OPEN_DURATION);
        assert_eq!(layout.len(), 2);
        assert!(layout[0].center.y < layout[0].core_center.y);
        assert!(layout[1].center.x > layout[1].core_center.x);
    }

    #[test]
    fn counter_clockwise_bloom_advances_left() {
        let (mut system, id) = clustered_system();
        system.config.bloom_direction = halley_config::ClusterBloomDirection::CounterClockwise;
        system.set_hovered_core(Some(id), Duration::ZERO);
        assert!(system.bloom_wakeup(HOLD_DURATION));
        let camera = Camera::new(
            Vec2 { x: 0.0, y: 0.0 },
            Vec2 {
                x: 1_920.0,
                y: 1_080.0,
            },
        );
        let geometry = Rectangle::new((0, 0).into(), (1_920, 1_080).into());
        let layout = system.bloom_layout("DP-1", &camera, geometry, HOLD_DURATION + OPEN_DURATION);
        assert!(layout[1].center.x < layout[1].core_center.x);
    }
}
