use halley_core::cluster::ClusterId;
use halley_core::cluster::layout::ClusterWorkspaceLayoutKind;
use halley_core::field::NodeId;

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
            if session
                .clusters
                .activate_slot(&output, slot, crate::frame_clock::monotonic_now())
            {
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
