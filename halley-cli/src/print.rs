use std::fmt::Write;
use std::process::ExitCode;

use halley_ipc::{
    ClusterInfo, ClusterLayoutKind, ClusterListResponse, ModeInfo, NodeInfo, NodeListResponse,
    NodeProtocolFamily, NodeRelationInfo, NodeRole, NodeState, OutputInfo, Response,
};

use crate::cmd::{ClusterOutput, NodeOutput};

pub fn node(response: Response, output: NodeOutput) -> ExitCode {
    match (response, output) {
        (Response::NodeList(list), NodeOutput::List { json: true }) => {
            print_json(&list, "node list")
        }
        (Response::NodeList(list), NodeOutput::List { json: false }) => {
            print!("{}", format_node_list(&list));
            ExitCode::SUCCESS
        }
        (Response::NodeInfo(info), NodeOutput::Info { json: true }) => {
            print_json(&info, "node info")
        }
        (Response::NodeInfo(info), NodeOutput::Info { json: false }) => {
            print!("{}", format_node_info(&info));
            ExitCode::SUCCESS
        }
        (Response::Ack, NodeOutput::Ack) => ExitCode::SUCCESS,
        (response, _) => unexpected(response),
    }
}

pub fn bearings(response: Response) -> ExitCode {
    match response {
        Response::Ack => ExitCode::SUCCESS,
        Response::BearingsStatus(status) => {
            println!("{}", if status.visible { "visible" } else { "hidden" });
            ExitCode::SUCCESS
        }
        response => unexpected(response),
    }
}

pub fn cluster(response: Response, output: ClusterOutput) -> ExitCode {
    match (response, output) {
        (Response::ClusterList(list), ClusterOutput::List { json: true }) => {
            print_json(&list, "cluster list")
        }
        (Response::ClusterList(list), ClusterOutput::List { json: false }) => {
            print!("{}", format_cluster_list(&list));
            ExitCode::SUCCESS
        }
        (Response::ClusterInfo(info), ClusterOutput::Info { json: true }) => {
            print_json(&info, "cluster info")
        }
        (Response::ClusterInfo(info), ClusterOutput::Info { json: false }) => {
            print!("{}", format_cluster_info(&info));
            ExitCode::SUCCESS
        }
        (Response::Ack, ClusterOutput::Ack) => ExitCode::SUCCESS,
        (response, _) => unexpected(response),
    }
}

pub fn ack(response: Response) -> ExitCode {
    match response {
        Response::Ack => ExitCode::SUCCESS,
        response => unexpected(response),
    }
}

pub fn outputs(response: Response) -> ExitCode {
    let Response::Outputs(outputs) = response else {
        return unexpected(response);
    };
    if outputs.outputs.is_empty() {
        println!("(no outputs)");
        return ExitCode::SUCCESS;
    }
    for (index, output) in outputs.outputs.iter().enumerate() {
        match format_output(output) {
            Ok(formatted) => {
                if index > 0 {
                    println!();
                }
                print!("{formatted}");
            }
            Err(err) => {
                eprintln!("halleyctl: invalid output response: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

pub fn version(response: Response) -> ExitCode {
    let Response::Version(version) = response else {
        return unexpected(response);
    };
    println!("halleyctl {}", env!("CARGO_PKG_VERSION"));
    println!(
        "compositor {} (ipc protocol {})",
        version.version, version.ipc_protocol
    );
    ExitCode::SUCCESS
}

fn print_json(value: &impl serde::Serialize, label: &str) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("halleyctl: failed to encode {label}: {err}");
            ExitCode::FAILURE
        }
    }
}

fn unexpected(response: Response) -> ExitCode {
    match response {
        Response::Error(message) => {
            eprintln!("halleyctl: compositor returned an error: {message}")
        }
        other => eprintln!("halleyctl: unexpected response: {other:?}"),
    }
    ExitCode::FAILURE
}

fn format_node_list(list: &NodeListResponse) -> String {
    if list.outputs.iter().all(|group| group.nodes.is_empty()) {
        return "No nodes.\n".to_string();
    }
    let mut formatted = String::new();
    for group in &list.outputs {
        writeln!(formatted, "{}", group.output).unwrap();
        writeln!(formatted, "  nodes: {}", group.nodes.len()).unwrap();
        if group.nodes.is_empty() {
            writeln!(formatted, "  entries: (none)").unwrap();
            continue;
        }
        writeln!(formatted, "  entries:").unwrap();
        for node in &group.nodes {
            let marker = if node.focused {
                "*"
            } else if node.latest {
                "+"
            } else {
                "-"
            };
            writeln!(formatted, "    {marker} {}  {}", node.id, node.title).unwrap();
            format_node_fields(&mut formatted, node, 6);
        }
    }
    formatted
}

fn format_node_info(node: &NodeInfo) -> String {
    let mut formatted = String::new();
    writeln!(formatted, "{}  {}", node.id, node.title).unwrap();
    format_node_fields(&mut formatted, node, 2);
    formatted
}

fn format_cluster_list(list: &ClusterListResponse) -> String {
    if list.outputs.iter().all(|group| group.clusters.is_empty()) {
        return "No clusters.\n".to_string();
    }
    let mut formatted = String::new();
    for group in &list.outputs {
        writeln!(formatted, "{}", group.output).unwrap();
        if group.clusters.is_empty() {
            writeln!(formatted, "  (none)").unwrap();
            continue;
        }
        for cluster in &group.clusters {
            let marker = if cluster.active {
                "*"
            } else if cluster.focused {
                "+"
            } else {
                "-"
            };
            let slot = cluster
                .slot
                .map(|slot| slot.to_string())
                .unwrap_or_else(|| "?".into());
            writeln!(
                formatted,
                "  {marker} slot {slot}: {}  {} [{}; {} window{}]",
                cluster.id,
                cluster.name,
                cluster_layout(cluster.layout),
                cluster.member_count,
                if cluster.member_count == 1 { "" } else { "s" },
            )
            .unwrap();
        }
    }
    formatted
}

fn format_cluster_info(info: &ClusterInfo) -> String {
    let mut formatted = String::new();
    writeln!(formatted, "{}  {}", info.summary.id, info.summary.name).unwrap();
    writeln!(formatted, "  output: {}", info.summary.output).unwrap();
    writeln!(
        formatted,
        "  slot: {}",
        info.summary
            .slot
            .map(|slot| slot.to_string())
            .unwrap_or_else(|| "(none)".into())
    )
    .unwrap();
    writeln!(
        formatted,
        "  layout: {}",
        cluster_layout(info.summary.layout)
    )
    .unwrap();
    writeln!(formatted, "  active: {}", info.summary.active).unwrap();
    writeln!(formatted, "  focused: {}", info.summary.focused).unwrap();
    writeln!(formatted, "  members: {}", info.members.len()).unwrap();
    for member in &info.members {
        writeln!(formatted, "    - {}  {}", member.id, member.title).unwrap();
    }
    formatted
}

fn cluster_layout(layout: ClusterLayoutKind) -> &'static str {
    match layout {
        ClusterLayoutKind::Tiling => "tiling",
        ClusterLayoutKind::Stacking => "stacking",
    }
}

fn format_node_fields(formatted: &mut String, node: &NodeInfo, indent: usize) {
    let pad = " ".repeat(indent);
    if let Some(output) = &node.output {
        writeln!(formatted, "{pad}output: {output}").unwrap();
    }
    writeln!(formatted, "{pad}state: {}", node_state(node.state)).unwrap();
    if let Some(app_id) = &node.app_id {
        writeln!(formatted, "{pad}app: {app_id}").unwrap();
    }
    writeln!(formatted, "{pad}role: {}", node_role(node.role)).unwrap();
    writeln!(
        formatted,
        "{pad}protocol: {}",
        node_protocol(node.protocol_family)
    )
    .unwrap();
    writeln!(formatted, "{pad}modal: {}", node.modal).unwrap();
    format_relation(formatted, "parent-node", node.parent.as_ref(), indent);
    format_relation(
        formatted,
        "transient-for",
        node.transient_for.as_ref(),
        indent,
    );
    if node.child_popup_count > 0 {
        writeln!(formatted, "{pad}child-popups: {}", node.child_popup_count).unwrap();
    }
    writeln!(formatted, "{pad}focused: {}", node.focused).unwrap();
    writeln!(formatted, "{pad}latest: {}", node.latest).unwrap();
    writeln!(formatted, "{pad}pos: {:.0}, {:.0}", node.pos_x, node.pos_y).unwrap();
    writeln!(
        formatted,
        "{pad}size: {:.0} x {:.0}",
        node.width, node.height
    )
    .unwrap();
}

fn format_relation(
    formatted: &mut String,
    label: &str,
    relation: Option<&NodeRelationInfo>,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    match relation {
        Some(NodeRelationInfo { node_id: Some(id) }) => {
            writeln!(formatted, "{pad}{label}: {id}").unwrap()
        }
        Some(NodeRelationInfo { node_id: None }) => {
            writeln!(formatted, "{pad}{label}: (unresolved)").unwrap()
        }
        None => writeln!(formatted, "{pad}{label}: (none)").unwrap(),
    }
}

fn node_state(state: NodeState) -> &'static str {
    match state {
        NodeState::Active => "active",
        NodeState::Drifting => "drifting",
        NodeState::Node => "node",
        NodeState::Core => "core",
    }
}

fn node_role(role: NodeRole) -> &'static str {
    match role {
        NodeRole::NormalToplevel => "normal",
        NodeRole::Dialog => "dialog",
        NodeRole::Popup => "popup",
        NodeRole::Unknown => "unknown",
    }
}

fn node_protocol(protocol: NodeProtocolFamily) -> &'static str {
    match protocol {
        NodeProtocolFamily::XdgToplevel => "xdg-toplevel",
        NodeProtocolFamily::XdgPopup => "xdg-popup",
        NodeProtocolFamily::Xwayland => "xwayland",
        NodeProtocolFamily::Unknown => "unknown",
    }
}

fn format_output(output: &OutputInfo) -> Result<String, String> {
    let mut formatted = String::new();
    writeln!(formatted, "{}", output.name).unwrap();

    if let Some(current_index) = output.current_mode {
        let current = output.modes.get(current_index).ok_or_else(|| {
            format!(
                "{} refers to missing current mode index {current_index}",
                output.name
            )
        })?;
        writeln!(
            formatted,
            "  Current mode: {}x{} @ {:.3} Hz{}",
            current.width,
            current.height,
            refresh_hz(current),
            mode_qualifier(current.preferred, false),
        )
        .unwrap();
        writeln!(
            formatted,
            "  Position: {}, {}",
            output.offset_x, output.offset_y
        )
        .unwrap();
        let vrr_state = if !output.vrr_supported {
            "unsupported"
        } else if output.vrr_active {
            "active"
        } else {
            "inactive"
        };
        writeln!(formatted, "  VRR: {} ({vrr_state})", output.vrr).unwrap();
    } else {
        writeln!(formatted, "  Disabled").unwrap();
    }

    if output.modes.is_empty() {
        writeln!(formatted, "  Available modes: (none)").unwrap();
        return Ok(formatted);
    }
    writeln!(formatted, "  Available modes:").unwrap();
    for (index, mode) in output.modes.iter().enumerate() {
        writeln!(
            formatted,
            "    {}x{}@{:.3}{}",
            mode.width,
            mode.height,
            refresh_hz(mode),
            mode_qualifier(mode.preferred, Some(index) == output.current_mode),
        )
        .unwrap();
    }
    Ok(formatted)
}

fn refresh_hz(mode: &ModeInfo) -> f64 {
    mode.refresh_millihz as f64 / 1000.0
}

fn mode_qualifier(preferred: bool, current: bool) -> &'static str {
    match (current, preferred) {
        (true, true) => " (current, preferred)",
        (true, false) => " (current)",
        (false, true) => " (preferred)",
        (false, false) => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use halley_ipc::{NodeKind, NodeOutputGroup};

    fn sample_node(id: u64, focused: bool, latest: bool) -> NodeInfo {
        NodeInfo {
            id,
            title: "Firefox".to_string(),
            app_id: Some("firefox".to_string()),
            output: Some("DP-1".to_string()),
            kind: NodeKind::Surface,
            state: NodeState::Active,
            visible: true,
            focused,
            latest,
            pinned: false,
            role: NodeRole::NormalToplevel,
            protocol_family: NodeProtocolFamily::XdgToplevel,
            modal: false,
            parent: None,
            transient_for: Some(NodeRelationInfo { node_id: Some(4) }),
            child_popup_count: 1,
            pos_x: 12.0,
            pos_y: 34.0,
            width: 1280.0,
            height: 720.0,
        }
    }

    #[test]
    fn rich_node_list_groups_outputs_and_marks_focus() {
        let list = NodeListResponse {
            outputs: vec![NodeOutputGroup {
                output: "DP-1".to_string(),
                nodes: vec![sample_node(7, true, false)],
            }],
        };
        assert_eq!(
            format_node_list(&list),
            "\
DP-1
  nodes: 1
  entries:
    * 7  Firefox
      output: DP-1
      state: active
      app: firefox
      role: normal
      protocol: xdg-toplevel
      modal: false
      parent-node: (none)
      transient-for: 4
      child-popups: 1
      focused: true
      latest: false
      pos: 12, 34
      size: 1280 x 720
"
        );
    }

    #[test]
    fn rich_node_info_includes_protocol_relations_and_geometry() {
        let formatted = format_node_info(&sample_node(9, false, true));
        assert!(formatted.starts_with("9  Firefox\n"));
        assert!(formatted.contains("  protocol: xdg-toplevel\n"));
        assert!(formatted.contains("  transient-for: 4\n"));
        assert!(formatted.contains("  pos: 12, 34\n"));
        assert!(formatted.contains("  size: 1280 x 720\n"));
    }

    #[test]
    fn empty_node_list_is_explicit() {
        assert_eq!(
            format_node_list(&NodeListResponse {
                outputs: Vec::new()
            }),
            "No nodes.\n"
        );
    }

    #[test]
    fn rejects_invalid_current_output_mode() {
        let output = OutputInfo {
            name: "DP-1".to_string(),
            modes: Vec::new(),
            current_mode: Some(1),
            offset_x: 0,
            offset_y: 0,
            vrr: "off".to_string(),
            vrr_supported: false,
            vrr_active: false,
        };
        assert_eq!(
            format_output(&output),
            Err("DP-1 refers to missing current mode index 1".to_string())
        );
    }
}
