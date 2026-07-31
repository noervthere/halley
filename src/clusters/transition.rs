use std::collections::HashMap;
use std::time::Duration;

use halley_core::cluster::ClusterId;
use halley_core::cluster::layout::ClusterWorkspaceLayoutKind;
use halley_core::field::NodeId;
use smithay::utils::{Logical, Point, Rectangle};

use super::ClusterSystem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TransitionKind {
    Opening,
    Closing,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct WorkspaceTransition {
    pub(super) cluster_id: ClusterId,
    pub(super) kind: TransitionKind,
    pub(super) layout: ClusterWorkspaceLayoutKind,
    pub(super) started_at: Duration,
    pub(super) duration: Duration,
    pub(super) stagger: Duration,
    pub(super) visible_members: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TransitionVisual {
    pub(super) rect: Rectangle<i32, Logical>,
    pub(super) alpha: f32,
}

#[derive(Clone, Debug)]
pub(super) struct ReflowTransition {
    cluster_id: ClusterId,
    started_at: Duration,
    duration: Duration,
    from: HashMap<NodeId, Rectangle<i32, Logical>>,
}

impl ClusterSystem {
    pub(super) fn begin_transition(
        &mut self,
        output: &str,
        cluster_id: ClusterId,
        kind: TransitionKind,
        now: Duration,
    ) {
        let Some(layout) = self.metadata(cluster_id).map(|metadata| metadata.layout) else {
            return;
        };
        if !self.animations.enabled {
            self.transitions.remove(output);
            return;
        }
        let duration_ms = match (layout, kind) {
            (ClusterWorkspaceLayoutKind::Tiling, TransitionKind::Opening) => {
                self.animations.tiling.open_duration_ms
            }
            (ClusterWorkspaceLayoutKind::Tiling, TransitionKind::Closing) => {
                self.animations.tiling.close_duration_ms
            }
            (ClusterWorkspaceLayoutKind::Stacking, TransitionKind::Opening) => {
                self.animations.stacking.open_duration_ms
            }
            (ClusterWorkspaceLayoutKind::Stacking, TransitionKind::Closing) => {
                self.animations.stacking.close_duration_ms
            }
        };
        let visible_members = self
            .registry
            .cluster(cluster_id)
            .map_or(0, |cluster| match layout {
                ClusterWorkspaceLayoutKind::Tiling if self.config.tiling.max_stack > 0 => cluster
                    .members()
                    .len()
                    .min(self.config.tiling.max_stack.saturating_add(1)),
                _ => cluster.members().len(),
            });
        let stagger =
            if kind == TransitionKind::Opening && layout == ClusterWorkspaceLayoutKind::Tiling {
                Duration::from_millis(u64::from(self.animations.tiling.stagger_ms))
            } else {
                Duration::ZERO
            };
        self.transitions.insert(
            output.to_string(),
            WorkspaceTransition {
                cluster_id,
                kind,
                layout,
                started_at: now,
                duration: Duration::from_millis(u64::from(duration_ms.max(1))),
                stagger,
                visible_members,
            },
        );
    }

    pub fn transition_cluster_on(&self, output: &str, now: Duration) -> Option<ClusterId> {
        let transition = self.transitions.get(output)?;
        self.transition_is_live(transition, now)
            .then_some(transition.cluster_id)
    }

    pub fn is_animating_on_output(&self, output: &str, now: Duration) -> bool {
        self.transitions
            .get(output)
            .is_some_and(|transition| self.transition_is_live(transition, now))
            || self
                .reflows
                .get(output)
                .is_some_and(|reflow| now.saturating_sub(reflow.started_at) < reflow.duration)
    }

    fn transition_is_live(&self, transition: &WorkspaceTransition, now: Duration) -> bool {
        let stagger_tail = transition
            .stagger
            .saturating_mul(transition.visible_members.saturating_sub(1) as u32);
        now.saturating_sub(transition.started_at) < transition.duration + stagger_tail
    }

    pub(super) fn transition_visual(
        &self,
        output: &str,
        cluster_id: ClusterId,
        member: NodeId,
        target: Rectangle<i32, Logical>,
        core: Option<Point<i32, Logical>>,
        now: Duration,
    ) -> Option<TransitionVisual> {
        let transition = self.transitions.get(output)?;
        if transition.cluster_id != cluster_id || !self.transition_is_live(transition, now) {
            return None;
        }
        let member_delay = if transition.kind == TransitionKind::Opening
            && transition.layout == ClusterWorkspaceLayoutKind::Tiling
        {
            self.registry
                .cluster(cluster_id)
                .and_then(|cluster| {
                    cluster
                        .members()
                        .iter()
                        .position(|candidate| *candidate == member)
                })
                .map_or(Duration::ZERO, |index| {
                    transition.stagger.saturating_mul(index as u32)
                })
        } else {
            Duration::ZERO
        };
        let elapsed = now
            .saturating_sub(transition.started_at)
            .saturating_sub(member_delay);
        let linear = (elapsed.as_secs_f32() / transition.duration.as_secs_f32()).clamp(0.0, 1.0);
        let eased = linear * linear * (3.0 - 2.0 * linear);
        let progress = match transition.kind {
            TransitionKind::Opening => eased,
            TransitionKind::Closing => 1.0 - eased,
        };
        let center = core.unwrap_or_else(|| {
            Point::from((
                target.loc.x + target.size.w / 2,
                target.loc.y + target.size.h / 2,
            ))
        });
        let origin = Rectangle::new(center, (1, 1).into());
        Some(TransitionVisual {
            rect: lerp_rect(origin, target, progress),
            alpha: progress,
        })
    }

    pub(super) fn begin_reflow(
        &mut self,
        output: &str,
        cluster_id: ClusterId,
        before: halley_core::cluster::layout::ClusterWorkspaceLayoutResult,
        now: Duration,
        duration_ms: u32,
    ) {
        if !self.animations.enabled {
            self.reflows.remove(output);
            return;
        }
        let from = before
            .placements
            .into_iter()
            .map(|placement| {
                (
                    placement.node_id,
                    Rectangle::new(
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
                    ),
                )
            })
            .collect();
        self.reflows.insert(
            output.to_string(),
            ReflowTransition {
                cluster_id,
                started_at: now,
                duration: Duration::from_millis(u64::from(duration_ms.max(1))),
                from,
            },
        );
    }

    pub(super) fn reflow_visual(
        &self,
        output: &str,
        cluster_id: ClusterId,
        member: NodeId,
        target: Rectangle<i32, Logical>,
        now: Duration,
    ) -> Option<Rectangle<i32, Logical>> {
        let reflow = self.reflows.get(output)?;
        if reflow.cluster_id != cluster_id {
            return None;
        }
        let elapsed = now.saturating_sub(reflow.started_at);
        if elapsed >= reflow.duration {
            return None;
        }
        let from = *reflow.from.get(&member)?;
        let linear = (elapsed.as_secs_f32() / reflow.duration.as_secs_f32()).clamp(0.0, 1.0);
        let eased = linear * linear * (3.0 - 2.0 * linear);
        Some(lerp_rect(from, target, eased))
    }
}

fn lerp_rect(
    from: Rectangle<i32, Logical>,
    to: Rectangle<i32, Logical>,
    progress: f32,
) -> Rectangle<i32, Logical> {
    let lerp = |from: i32, to: i32| (from as f32 + (to - from) as f32 * progress).round() as i32;
    Rectangle::new(
        (lerp(from.loc.x, to.loc.x), lerp(from.loc.y, to.loc.y)).into(),
        (
            lerp(from.size.w, to.size.w).max(1),
            lerp(from.size.h, to.size.h).max(1),
        )
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_interpolation_keeps_endpoints() {
        let from = Rectangle::new((20, 40).into(), (1, 1).into());
        let to = Rectangle::new((100, 80).into(), (800, 600).into());
        assert_eq!(lerp_rect(from, to, 0.0), from);
        assert_eq!(lerp_rect(from, to, 1.0), to);
    }
}
