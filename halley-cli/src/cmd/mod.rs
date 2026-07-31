mod cluster;
mod node;

use std::path::PathBuf;

use halley_ipc::{BearingsRequest, ClusterRequest, DpmsCommand, NodeRequest};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Outputs,
    Dpms {
        command: DpmsCommand,
        output: Option<String>,
    },
    DpmsHelp,
    Node {
        request: NodeRequest,
        output: NodeOutput,
    },
    NodeHelp,
    Cluster {
        request: ClusterRequest,
        output: ClusterOutput,
    },
    ClusterHelp,
    Bearings(BearingsRequest),
    BearingsHelp,
    ConfigVerify(Option<PathBuf>),
    ConfigHelp,
    Quit,
    Version,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeOutput {
    List { json: bool },
    Info { json: bool },
    Ack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClusterOutput {
    List { json: bool },
    Info { json: bool },
    Ack,
}

pub fn parse(args: &[String]) -> Result<Action, String> {
    let action = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => Action::Help,
        Some("--version") | Some("-V") => Action::Version,
        Some("outputs") => {
            if let Some(unexpected) = args.get(1) {
                return Err(format!("unexpected argument {unexpected:?}"));
            }
            Action::Outputs
        }
        Some("dpms") => return parse_dpms(&args[1..]),
        Some("node") => return node::parse(&args[1..]),
        Some("cluster") => return cluster::parse(&args[1..]),
        Some("bearings") => return parse_bearings(&args[1..]),
        Some("config") => return parse_config(&args[1..]),
        Some("quit") => Action::Quit,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };

    if let Some(unexpected) = args.get(1) {
        return Err(format!("unexpected argument {unexpected:?}"));
    }
    Ok(action)
}

fn parse_dpms(args: &[String]) -> Result<Action, String> {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Action::DpmsHelp);
    }

    let mut command = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "dpms output option requires a connector name".to_string())?;
                if output.replace(value.clone()).is_some() {
                    return Err("dpms output option was specified more than once".to_string());
                }
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown dpms option {value:?}"));
            }
            value if command.is_none() => command = Some(value),
            value => return Err(format!("unexpected dpms argument {value:?}")),
        }
        index += 1;
    }

    let command = match command {
        Some("off") => DpmsCommand::Off,
        Some("on") => DpmsCommand::On,
        Some("toggle") => DpmsCommand::Toggle,
        Some(other) => return Err(format!("unknown dpms command {other:?}")),
        None => return Ok(Action::DpmsHelp),
    };
    Ok(Action::Dpms { command, output })
}

fn parse_config(args: &[String]) -> Result<Action, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Action::ConfigHelp);
    };
    if command == "-h" || command == "--help" {
        return Ok(Action::ConfigHelp);
    }
    if command != "verify" {
        return Err(format!("unknown config command {command:?}"));
    }
    let mut path = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(Action::ConfigHelp),
            "-c" | "--config" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "config verify path option requires a path".to_string())?;
                set_verify_path(&mut path, value)?;
            }
            value if value.starts_with("--config=") => {
                set_verify_path(&mut path, &value["--config=".len()..])?;
            }
            value => return Err(format!("unexpected config verify argument {value:?}")),
        }
        index += 1;
    }
    Ok(Action::ConfigVerify(path))
}

fn set_verify_path(path: &mut Option<PathBuf>, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("config verify path cannot be empty".to_string());
    }
    if path.replace(PathBuf::from(value)).is_some() {
        return Err("config verify path was specified more than once".to_string());
    }
    Ok(())
}

fn parse_bearings(args: &[String]) -> Result<Action, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Action::BearingsHelp);
    };
    if command == "-h" || command == "--help" {
        return Ok(Action::BearingsHelp);
    }
    if let Some(unexpected) = args.get(1) {
        return Err(format!("unexpected argument {unexpected:?}"));
    }
    let request = match command {
        "show" => BearingsRequest::Show,
        "hide" => BearingsRequest::Hide,
        "toggle" => BearingsRequest::Toggle,
        "status" => BearingsRequest::Status,
        other => return Err(format!("unknown bearings command {other:?}")),
    };
    Ok(Action::Bearings(request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use halley_ipc::{NodeMoveDirection, NodeSelector};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parser_accepts_top_level_commands() {
        assert_eq!(parse(&[]), Ok(Action::Help));
        assert_eq!(parse(&args(&["outputs"])), Ok(Action::Outputs));
        assert_eq!(parse(&args(&["--help"])), Ok(Action::Help));
        assert_eq!(parse(&args(&["--version"])), Ok(Action::Version));
        assert_eq!(parse(&args(&["quit"])), Ok(Action::Quit));
        assert!(parse(&args(&["output"])).is_err());
        assert!(parse(&args(&["outputs", "extra"])).is_err());
    }

    #[test]
    fn parser_covers_bearings_dpms_and_config() {
        assert_eq!(
            parse(&args(&["bearings", "show"])),
            Ok(Action::Bearings(BearingsRequest::Show))
        );
        assert_eq!(
            parse(&args(&["dpms", "-o", "DP-1", "on"])),
            Ok(Action::Dpms {
                command: DpmsCommand::On,
                output: Some("DP-1".to_string()),
            })
        );
        assert_eq!(
            parse(&args(&["config", "verify", "--config=/tmp/two.rune"])),
            Ok(Action::ConfigVerify(Some("/tmp/two.rune".into())))
        );
        assert!(parse(&args(&["dpms", "sleep"])).is_err());
        assert!(parse(&args(&["config", "verify", "-c"])).is_err());
    }

    #[test]
    fn parser_covers_node_state_controls() {
        assert_eq!(
            parse(&args(&["node", "list", "-o", "DP-1", "--json"])),
            Ok(Action::Node {
                request: NodeRequest::List {
                    output: Some("DP-1".into())
                },
                output: NodeOutput::List { json: true },
            })
        );
        assert_eq!(
            parse(&args(&["node", "collapse", "focused"])),
            Ok(Action::Node {
                request: NodeRequest::Collapse {
                    selector: Some(NodeSelector::Focused),
                    output: None,
                },
                output: NodeOutput::Ack,
            })
        );
        assert_eq!(
            parse(&args(&["node", "move", "left", "app:firefox"])),
            Ok(Action::Node {
                request: NodeRequest::Move {
                    direction: NodeMoveDirection::Left,
                    selector: Some(NodeSelector::App("firefox".into())),
                    output: None,
                },
                output: NodeOutput::Ack,
            })
        );
        assert!(parse(&args(&["node", "collapse", "--json"])).is_err());
    }
}
