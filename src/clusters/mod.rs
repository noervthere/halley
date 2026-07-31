use std::collections::{HashMap, HashSet};

use halley_core::cluster::layout::ClusterWorkspaceLayoutKind;
use halley_core::cluster::{ClusterId, ClusterRegistry};
use halley_core::field::NodeId;

#[derive(Clone, Debug)]
pub struct ClusterMetadata {
    pub name: String,
    pub output: String,
    pub layout: ClusterWorkspaceLayoutKind,
    pub core: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub struct CreationState {
    pub output: String,
    pub selected: HashSet<NodeId>,
    pub naming: bool,
    pub name_buffer: String,
}

/// Owns every cluster-specific state transition. Field and Nodes remain
/// unaware of membership, slots, workspace modes, naming, and presentation.
pub struct ClusterSystem {
    registry: ClusterRegistry,
    metadata: HashMap<ClusterId, ClusterMetadata>,
    slots: HashMap<String, Vec<ClusterId>>,
    active: HashMap<String, ClusterId>,
    creation: Option<CreationState>,
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
            creation: None,
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

    pub fn creation(&self) -> Option<&CreationState> {
        self.creation.as_ref()
    }

    pub fn begin_creation(&mut self, output: String) -> bool {
        if self.creation.is_some() {
            return false;
        }
        self.creation = Some(CreationState {
            output,
            selected: HashSet::new(),
            naming: false,
            name_buffer: String::new(),
        });
        true
    }

    pub fn cancel_creation(&mut self) -> bool {
        self.creation.take().is_some()
    }

    pub fn cycle_active_layout(&mut self, output: &str) -> bool {
        let Some(id) = self.active_on(output) else {
            return false;
        };
        let Some(metadata) = self.metadata.get_mut(&id) else {
            return false;
        };
        metadata.layout = match metadata.layout {
            ClusterWorkspaceLayoutKind::Tiling => ClusterWorkspaceLayoutKind::Stacking,
            ClusterWorkspaceLayoutKind::Stacking => ClusterWorkspaceLayoutKind::Tiling,
        };
        true
    }

    pub fn activate_slot(&mut self, output: &str, slot: u8) -> bool {
        let Some(id) = self
            .slots
            .get(output)
            .and_then(|slots| slots.get(usize::from(slot.saturating_sub(1))))
            .copied()
        else {
            return false;
        };
        if self.active_on(output) == Some(id) {
            self.active.remove(output);
            self.registry.deactivate_cluster_workspace(id);
        } else {
            if let Some(previous) = self.active.insert(output.to_string(), id) {
                self.registry.deactivate_cluster_workspace(previous);
            }
            self.registry.activate_cluster_workspace(id);
        }
        true
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

    fn slot_of(&self, output: &str, id: ClusterId) -> Option<u8> {
        self.slots
            .get(output)?
            .iter()
            .position(|candidate| *candidate == id)
            .and_then(|index| u8::try_from(index + 1).ok())
    }
}

fn output_context<D: crate::session::SessionDriver>(
    session: &crate::session::Session<D>,
    requested: Option<&str>,
) -> Result<String, String> {
    if let Some(output) = requested {
        if session
            .driver
            .output_info()
            .iter()
            .any(|candidate| candidate.name == output)
        {
            return Ok(output.to_string());
        }
        return Err(format!("output {output:?} was not found"));
    }
    Ok(crate::wayland::focus::selected_output(&session.wayland)
        .unwrap_or_else(|| session.driver.primary_output())
        .name())
}

fn summary<D: crate::session::SessionDriver>(
    session: &crate::session::Session<D>,
    id: ClusterId,
) -> Option<halley_ipc::ClusterSummary> {
    let cluster = session.clusters.registry.cluster(id)?;
    let metadata = session.clusters.metadata(id)?;
    Some(halley_ipc::ClusterSummary {
        id: id.as_u64(),
        slot: session.clusters.slot_of(&metadata.output, id),
        name: metadata.name.clone(),
        output: metadata.output.clone(),
        layout: match metadata.layout {
            ClusterWorkspaceLayoutKind::Tiling => halley_ipc::ClusterLayoutKind::Tiling,
            ClusterWorkspaceLayoutKind::Stacking => halley_ipc::ClusterLayoutKind::Stacking,
        },
        member_count: cluster.members().len(),
        active: session.clusters.active_on(&metadata.output) == Some(id),
        focused: session
            .nodes
            .focused()
            .is_some_and(|focused| cluster.contains(focused)),
    })
}

pub fn handle_request<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    request: halley_ipc::ClusterRequest,
) -> halley_ipc::Response {
    match request {
        halley_ipc::ClusterRequest::List { output } => {
            let outputs = match output {
                Some(output) => match output_context(session, Some(&output)) {
                    Ok(output) => vec![output],
                    Err(message) => return halley_ipc::Response::Error(message),
                },
                None => session
                    .driver
                    .output_info()
                    .into_iter()
                    .map(|output| output.name)
                    .collect(),
            };
            halley_ipc::Response::ClusterList(halley_ipc::ClusterListResponse {
                outputs: outputs
                    .into_iter()
                    .map(|output| {
                        let clusters = session
                            .clusters
                            .clusters_for_output(&output)
                            .filter_map(|(_, id, _)| summary(session, id))
                            .collect();
                        halley_ipc::ClusterOutputGroup { output, clusters }
                    })
                    .collect(),
            })
        }
        halley_ipc::ClusterRequest::Inspect { target, output } => {
            let id = match target {
                halley_ipc::ClusterTarget::Id(raw) => ClusterId::new(raw),
                halley_ipc::ClusterTarget::Current => {
                    let output = match output_context(session, output.as_deref()) {
                        Ok(output) => output,
                        Err(message) => return halley_ipc::Response::Error(message),
                    };
                    let Some(id) = session.clusters.active_on(&output) else {
                        return halley_ipc::Response::Error(format!(
                            "no active cluster on output {output}"
                        ));
                    };
                    id
                }
            };
            let Some(cluster) = session.clusters.registry.cluster(id) else {
                return halley_ipc::Response::Error(format!(
                    "cluster {} was not found",
                    id.as_u64()
                ));
            };
            let Some(summary) = summary(session, id) else {
                return halley_ipc::Response::Error("cluster metadata is incomplete".into());
            };
            let members = cluster
                .members()
                .iter()
                .filter_map(|id| crate::nodes::ipc::node_info(session, *id))
                .collect();
            halley_ipc::Response::ClusterInfo(halley_ipc::ClusterInfo {
                summary,
                core_node_id: cluster.core_node().map(NodeId::as_u64),
                members,
            })
        }
        halley_ipc::ClusterRequest::LayoutCycle { output } => {
            let output = match output_context(session, output.as_deref()) {
                Ok(output) => output,
                Err(message) => return halley_ipc::Response::Error(message),
            };
            if session.clusters.cycle_active_layout(&output) {
                session.request_redraw();
                halley_ipc::Response::Ack
            } else {
                halley_ipc::Response::Error(format!("no active cluster on output {output}"))
            }
        }
        halley_ipc::ClusterRequest::Slot { slot, output } => {
            if !(1..=10).contains(&slot) {
                return halley_ipc::Response::Error(format!(
                    "cluster slot must be between 1 and 10, got {slot}"
                ));
            }
            let output = match output_context(session, output.as_deref()) {
                Ok(output) => output,
                Err(message) => return halley_ipc::Response::Error(message),
            };
            if session.clusters.activate_slot(&output, slot) {
                session.request_redraw();
                halley_ipc::Response::Ack
            } else {
                halley_ipc::Response::Error(format!(
                    "no cluster exists in slot {slot} on output {output}"
                ))
            }
        }
    }
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
}
