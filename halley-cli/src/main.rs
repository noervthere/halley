use std::fmt::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use halley_ipc::{
    BearingsRequest, ModeInfo, NodeInfo, NodeMoveDirection, NodeRequest, NodeSelector, OutputInfo,
    Request, Response,
};

/// Hand-rolled arg parsing, no `clap` - matches old halley's own
/// `halley-cli` (which had the same "no dependency without a concrete
/// need" instinct), and there's exactly one real subcommand so far.
const HELP: &str = "\
Usage: halleyctl <command>

Commands:
  outputs        List connected monitors and their current mode/position
  node           List, inspect, focus, move, collapse, restore, toggle, or close nodes
  bearings       Show, hide, toggle, or inspect Bearings
  config         Verify the configuration selected by the compositor
  quit           Open Halley's exit confirmation

Options:
  -h, --help     Print this message
  -V, --version  Print both halleyctl's and the running compositor's version
";

const CONFIG_HELP: &str = "\
Usage:
  halleyctl config verify
  halleyctl config verify -c PATH
  halleyctl config verify --config PATH
";

const NODE_HELP: &str = "\
Usage:
  halleyctl node list [-o OUTPUT] [--json]
  halleyctl node info [SELECTOR] [-o OUTPUT] [--json]
  halleyctl node focus [SELECTOR] [-o OUTPUT]
  halleyctl node move left|right|up|down [SELECTOR] [-o OUTPUT]
  halleyctl node collapse [SELECTOR] [-o OUTPUT]
  halleyctl node restore [SELECTOR] [-o OUTPUT]
  halleyctl node toggle [SELECTOR] [-o OUTPUT]
  halleyctl node close [SELECTOR] [-o OUTPUT]

Selectors:
  focused, latest, ID, id:ID, title:TEXT, app:APP_ID
";

const BEARINGS_HELP: &str = "\
Usage:
  halleyctl bearings show
  halleyctl bearings hide
  halleyctl bearings toggle
  halleyctl bearings status
";

#[derive(Clone, Debug, PartialEq)]
enum Action {
    Outputs,
    Node {
        request: NodeRequest,
        output: NodeOutput,
    },
    NodeHelp,
    Bearings(BearingsRequest),
    BearingsHelp,
    ConfigVerify(Option<PathBuf>),
    ConfigHelp,
    Quit,
    Version,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeOutput {
    List { json: bool },
    Info { json: bool },
    Ack,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Action::Outputs) => query(Request::Outputs, print_outputs),
        Ok(Action::Node { request, output }) => query(Request::Node(request), |response| {
            print_node(response, output)
        }),
        Ok(Action::NodeHelp) => {
            print!("{NODE_HELP}");
            ExitCode::SUCCESS
        }
        Ok(Action::Bearings(request)) => query(Request::Bearings(request), print_bearings),
        Ok(Action::BearingsHelp) => {
            print!("{BEARINGS_HELP}");
            ExitCode::SUCCESS
        }
        Ok(Action::ConfigVerify(path)) => verify_config(path),
        Ok(Action::ConfigHelp) => {
            print!("{CONFIG_HELP}");
            ExitCode::SUCCESS
        }
        Ok(Action::Quit) => query(Request::Quit, print_ack),
        Ok(Action::Version) => query(Request::Version, print_version),
        Ok(Action::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("halleyctl: {err}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn parse_args(args: &[String]) -> Result<Action, String> {
    let action = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => Action::Help,
        Some("--version") | Some("-V") => Action::Version,
        Some("outputs") => {
            if let Some(unexpected) = args.get(1) {
                return Err(format!("unexpected argument {unexpected:?}"));
            }
            Action::Outputs
        }
        Some("node") => return parse_node(&args[1..]),
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

fn parse_node(args: &[String]) -> Result<Action, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Action::NodeHelp);
    };
    if command == "-h"
        || command == "--help"
        || args[1..].iter().any(|arg| arg == "-h" || arg == "--help")
    {
        return Ok(Action::NodeHelp);
    }
    match command {
        "list" => {
            let (selector, output, json) = parse_node_flags(&args[1..], false)?;
            debug_assert!(selector.is_none());
            Ok(Action::Node {
                request: NodeRequest::List { output },
                output: NodeOutput::List { json },
            })
        }
        "info" | "focus" | "collapse" | "restore" | "toggle" | "close" => {
            let (selector, output, json) = parse_node_flags(&args[1..], true)?;
            if json && command != "info" {
                return Err("--json is supported only by node list and node info".to_string());
            }
            let request = match command {
                "info" => NodeRequest::Info { selector, output },
                "focus" => NodeRequest::Focus { selector, output },
                "collapse" => NodeRequest::Collapse { selector, output },
                "restore" => NodeRequest::Restore { selector, output },
                "toggle" => NodeRequest::Toggle { selector, output },
                "close" => NodeRequest::Close { selector, output },
                _ => unreachable!(),
            };
            Ok(Action::Node {
                request,
                output: if command == "info" {
                    NodeOutput::Info { json }
                } else {
                    NodeOutput::Ack
                },
            })
        }
        "move" => {
            let direction = match args.get(1).map(String::as_str) {
                Some("left") => NodeMoveDirection::Left,
                Some("right") => NodeMoveDirection::Right,
                Some("up") => NodeMoveDirection::Up,
                Some("down") => NodeMoveDirection::Down,
                Some(other) => return Err(format!("unknown node move direction {other:?}")),
                None => return Err("node move requires left, right, up, or down".to_string()),
            };
            let (selector, output, json) = parse_node_flags(&args[2..], true)?;
            if json {
                return Err("--json is supported only by node list and node info".to_string());
            }
            Ok(Action::Node {
                request: NodeRequest::Move {
                    direction,
                    selector,
                    output,
                },
                output: NodeOutput::Ack,
            })
        }
        other => Err(format!("unknown node command {other:?}")),
    }
}

fn parse_node_flags(
    args: &[String],
    allow_selector: bool,
) -> Result<(Option<NodeSelector>, Option<String>, bool), String> {
    let mut selector = None;
    let mut output = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "-o" | "--output" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "node output option requires a connector name".to_string())?;
                if output.replace(value.clone()).is_some() {
                    return Err("node output option was specified more than once".to_string());
                }
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown node option {value:?}"));
            }
            value if allow_selector && selector.is_none() => {
                selector = Some(parse_selector(value)?);
            }
            value if !allow_selector => return Err(format!("unexpected argument {value:?}")),
            value => return Err(format!("unexpected extra selector {value:?}")),
        }
        index += 1;
    }
    Ok((selector, output, json))
}

fn parse_selector(value: &str) -> Result<NodeSelector, String> {
    match value {
        _ if value.eq_ignore_ascii_case("focused") => Ok(NodeSelector::Focused),
        _ if value.eq_ignore_ascii_case("latest") => Ok(NodeSelector::Latest),
        _ if value.parse::<u64>().is_ok() => Ok(NodeSelector::Id(
            value.parse().expect("checked numeric selector"),
        )),
        _ if value.starts_with("id:") => value[3..]
            .parse::<u64>()
            .map(NodeSelector::Id)
            .map_err(|_| format!("invalid node id selector {value:?}")),
        _ if value.starts_with("title:") => Ok(NodeSelector::Title(value[6..].to_string())),
        _ if value.starts_with("app:") => Ok(NodeSelector::App(value[4..].to_string())),
        _ => Err(format!("invalid node selector {value:?}")),
    }
}

fn print_node(response: Response, output: NodeOutput) -> ExitCode {
    match (response, output) {
        (Response::NodeList(list), NodeOutput::List { json: true }) => {
            match serde_json::to_string_pretty(&list) {
                Ok(json) => println!("{json}"),
                Err(err) => {
                    eprintln!("halleyctl: failed to encode node list: {err}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        (Response::NodeList(list), NodeOutput::List { json: false }) => {
            for group in list.outputs {
                println!("{}", group.output);
                for node in group.nodes {
                    println!(
                        "  {}  {:<8}  {}{}",
                        node.id,
                        format!("{:?}", node.state).to_ascii_lowercase(),
                        node.title,
                        node.app_id
                            .as_deref()
                            .map(|app| format!(" ({app})"))
                            .unwrap_or_default()
                    );
                }
            }
            ExitCode::SUCCESS
        }
        (Response::NodeInfo(info), NodeOutput::Info { json: true }) => {
            match serde_json::to_string_pretty(&info) {
                Ok(json) => println!("{json}"),
                Err(err) => {
                    eprintln!("halleyctl: failed to encode node info: {err}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        (Response::NodeInfo(info), NodeOutput::Info { json: false }) => {
            print_node_info(&info);
            ExitCode::SUCCESS
        }
        (Response::Ack, NodeOutput::Ack) => ExitCode::SUCCESS,
        (response, _) => print_unexpected(response),
    }
}

fn print_node_info(node: &NodeInfo) {
    println!("Node {}", node.id);
    println!("  Title: {}", node.title);
    println!("  App ID: {}", node.app_id.as_deref().unwrap_or("(none)"));
    println!("  Output: {}", node.output.as_deref().unwrap_or("(none)"));
    println!("  State: {:?}", node.state);
    println!(
        "  Geometry: {:.0}, {:.0}  {:.0}x{:.0}",
        node.pos_x, node.pos_y, node.width, node.height
    );
    println!("  Focused: {}", node.focused);
}

fn print_bearings(response: Response) -> ExitCode {
    match response {
        Response::Ack => ExitCode::SUCCESS,
        Response::BearingsStatus(status) => {
            println!("{}", if status.visible { "visible" } else { "hidden" });
            ExitCode::SUCCESS
        }
        response => print_unexpected(response),
    }
}

fn print_ack(response: Response) -> ExitCode {
    match response {
        Response::Ack => ExitCode::SUCCESS,
        response => print_unexpected(response),
    }
}

fn verify_config(explicit: Option<PathBuf>) -> ExitCode {
    let path = match explicit {
        Some(path) => match absolute_path(path) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("halleyctl: could not resolve config path: {error}");
                return ExitCode::from(2);
            }
        },
        None => match discover_config_path() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("halleyctl: {error}");
                return ExitCode::from(2);
            }
        },
    };

    match halley_config::load_runtime_config_diagnostic_at(&path) {
        Ok(_) => {
            println!("Configuration valid");
            println!("  File: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(diagnostic) => {
            eprint!("{}", format_diagnostic(&diagnostic));
            ExitCode::FAILURE
        }
    }
}

fn absolute_path(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn discover_config_path() -> Result<PathBuf, String> {
    let socket = match halley_ipc::default_socket_path() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return default_config_path();
        }
        Err(error) => return Err(format!("could not resolve the compositor socket: {error}")),
    };
    let mut connection = match halley_ipc::Connection::connect_to(&socket) {
        Ok(connection) => connection,
        Err(halley_ipc::CodecError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return default_config_path();
        }
        Err(error) => return Err(format!("could not connect to the compositor: {error}")),
    };
    let response = connection.request(&Request::ConfigPath, &[]).map_err(|error| {
        format!(
            "the running compositor cannot report its config path ({error}); pass `-c PATH` explicitly"
        )
    })?;
    if !response.fds.is_empty() {
        return Err(
            "the compositor returned unexpected descriptors for its config path".to_string(),
        );
    }
    match response.response {
        Response::ConfigPath(Some(path)) => Ok(PathBuf::from(path)),
        Response::ConfigPath(None) => {
            Err("the running compositor has no selected config path; pass `-c PATH`".to_string())
        }
        Response::Error(error) => Err(format!(
            "the running compositor cannot report its config path ({error}); pass `-c PATH` explicitly"
        )),
        response => Err(format!(
            "the running compositor returned {response:?} instead of its config path; pass `-c PATH` explicitly"
        )),
    }
}

fn default_config_path() -> Result<PathBuf, String> {
    halley_config::config_path()
        .ok_or_else(|| "HOME and XDG_CONFIG_HOME are unset; pass `-c PATH`".to_string())
        .and_then(|path| {
            absolute_path(path)
                .map_err(|error| format!("could not resolve default config: {error}"))
        })
}

fn format_diagnostic(diagnostic: &halley_config::ConfigDiagnostic) -> String {
    let mut formatted = String::from("Configuration invalid\n");
    if let Some(path) = diagnostic.path.as_deref() {
        writeln!(formatted, "  File: {}", path.display()).unwrap();
    }
    match (diagnostic.line, diagnostic.column) {
        (Some(line), Some(column)) => {
            writeln!(formatted, "  Location: line {line}, column {column}").unwrap();
        }
        (Some(line), None) => {
            writeln!(formatted, "  Location: line {line}").unwrap();
        }
        (None, Some(column)) => {
            writeln!(formatted, "  Location: column {column}").unwrap();
        }
        (None, None) => {}
    }
    writeln!(formatted, "  Error: {}", diagnostic.message).unwrap();
    if let (Some(line), Some(source)) = (diagnostic.line, diagnostic.source_line.as_deref()) {
        let width = line.to_string().len();
        writeln!(formatted, "  {line:>width$} | {source}").unwrap();
        if let Some(column) = diagnostic.column {
            let caret_padding = " ".repeat(column.saturating_sub(1));
            writeln!(formatted, "  {:>width$} | {caret_padding}^", "").unwrap();
        }
    }
    if let Some(hint) = diagnostic.hint.as_deref() {
        writeln!(formatted, "  Hint: {hint}").unwrap();
    }
    if let Some(code) = diagnostic.code {
        writeln!(formatted, "  Code: E{code}").unwrap();
    }
    formatted
}

/// Sends `req` to the running compositor and hands the response to
/// `on_response` - shared by every subcommand so connection-failure
/// handling isn't duplicated per command.
fn query(req: Request, on_response: impl FnOnce(Response) -> ExitCode) -> ExitCode {
    match halley_ipc::send_request(&req) {
        Ok(resp) => on_response(resp),
        Err(err) => {
            eprintln!("halleyctl: failed to reach the running compositor: {err}");
            ExitCode::FAILURE
        }
    }
}

fn print_outputs(resp: Response) -> ExitCode {
    let Response::Outputs(outputs) = resp else {
        return print_unexpected(resp);
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
        writeln!(formatted, "  VRR: {}", output.vrr).unwrap();
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

fn mode_qualifier(preferred: bool, current: bool) -> String {
    match (current, preferred) {
        (true, true) => " (current, preferred)".to_string(),
        (true, false) => " (current)".to_string(),
        (false, true) => " (preferred)".to_string(),
        (false, false) => String::new(),
    }
}

fn print_version(resp: Response) -> ExitCode {
    let Response::Version(version) = resp else {
        return print_unexpected(resp);
    };
    println!("halleyctl {}", env!("CARGO_PKG_VERSION"));
    println!(
        "compositor {} (ipc protocol {})",
        version.version, version.ipc_protocol
    );
    ExitCode::SUCCESS
}

fn print_unexpected(resp: Response) -> ExitCode {
    match resp {
        Response::Error(msg) => eprintln!("halleyctl: compositor returned an error: {msg}"),
        other => eprintln!("halleyctl: unexpected response: {other:?}"),
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn help_separates_commands_from_options() {
        let commands = HELP
            .split_once("Commands:\n")
            .and_then(|(_, rest)| rest.split_once("\nOptions:"))
            .map(|(commands, _)| commands)
            .expect("help has Commands followed by Options");

        assert!(commands.contains("outputs"));
        assert!(commands.contains("bearings"));
        assert!(commands.contains("config"));
        assert!(commands.contains("quit"));
        assert!(!commands.contains("--help"));
        assert!(!commands.contains("--version"));
        assert!(HELP.contains("Options:\n  -h, --help"));
        assert!(HELP.contains("  -V, --version"));
    }

    #[test]
    fn parser_covers_old_bearings_commands() {
        assert_eq!(parse_args(&args(&["bearings"])), Ok(Action::BearingsHelp));
        assert_eq!(
            parse_args(&args(&["bearings", "--help"])),
            Ok(Action::BearingsHelp)
        );
        assert_eq!(
            parse_args(&args(&["bearings", "show"])),
            Ok(Action::Bearings(BearingsRequest::Show))
        );
        assert_eq!(
            parse_args(&args(&["bearings", "hide"])),
            Ok(Action::Bearings(BearingsRequest::Hide))
        );
        assert_eq!(
            parse_args(&args(&["bearings", "toggle"])),
            Ok(Action::Bearings(BearingsRequest::Toggle))
        );
        assert_eq!(
            parse_args(&args(&["bearings", "status"])),
            Ok(Action::Bearings(BearingsRequest::Status))
        );
        assert!(parse_args(&args(&["bearings", "status", "extra"])).is_err());
        assert!(parse_args(&args(&["bearings", "wat"])).is_err());
    }

    #[test]
    fn parser_accepts_only_complete_supported_invocations() {
        assert_eq!(parse_args(&[]), Ok(Action::Help));
        assert_eq!(parse_args(&args(&["outputs"])), Ok(Action::Outputs));
        assert_eq!(parse_args(&args(&["--help"])), Ok(Action::Help));
        assert_eq!(parse_args(&args(&["-h"])), Ok(Action::Help));
        assert_eq!(parse_args(&args(&["--version"])), Ok(Action::Version));
        assert_eq!(parse_args(&args(&["-V"])), Ok(Action::Version));
        assert_eq!(parse_args(&args(&["quit"])), Ok(Action::Quit));
    }

    #[test]
    fn parser_accepts_config_verify_path_forms() {
        assert_eq!(parse_args(&args(&["config"])), Ok(Action::ConfigHelp));
        assert_eq!(
            parse_args(&args(&["config", "verify"])),
            Ok(Action::ConfigVerify(None))
        );
        assert_eq!(
            parse_args(&args(&["config", "verify", "-c", "one.rune"])),
            Ok(Action::ConfigVerify(Some("one.rune".into())))
        );
        assert_eq!(
            parse_args(&args(&["config", "verify", "--config=/tmp/two.rune"])),
            Ok(Action::ConfigVerify(Some("/tmp/two.rune".into())))
        );
        assert!(parse_args(&args(&["config", "verify", "-c"])).is_err());
        assert!(parse_args(&args(&["config", "verify", "-c", "one", "--config=two"])).is_err());
    }

    #[test]
    fn detailed_diagnostic_omits_unavailable_fields() {
        let diagnostic = halley_config::ConfigDiagnostic {
            path: Some("/tmp/halley.rune".into()),
            line: Some(12),
            column: Some(4),
            message: "bad value".to_string(),
            source_line: Some("  value nope".to_string()),
            hint: Some("use yes".to_string()),
            code: Some(7),
        };
        let formatted = format_diagnostic(&diagnostic);
        assert!(formatted.contains("File: /tmp/halley.rune"));
        assert!(formatted.contains("Location: line 12, column 4"));
        assert!(formatted.contains("12 |   value nope"));
        assert!(formatted.contains("|    ^"));
        assert!(formatted.contains("Hint: use yes"));
        assert!(formatted.contains("Code: E7"));

        let semantic =
            halley_config::ConfigDiagnostic::message(None, "semantic failure".to_string());
        let formatted = format_diagnostic(&semantic);
        assert!(!formatted.contains("File:"));
        assert!(!formatted.contains("Location:"));
    }

    #[test]
    fn parser_rejects_unknown_commands_and_trailing_arguments() {
        assert_eq!(
            parse_args(&args(&["output"])),
            Err("unknown command \"output\"".to_string())
        );
        assert_eq!(
            parse_args(&args(&["outputs", "extra"])),
            Err("unexpected argument \"extra\"".to_string())
        );
        assert_eq!(
            parse_args(&args(&["--version", "extra"])),
            Err("unexpected argument \"extra\"".to_string())
        );
    }

    #[test]
    fn parser_covers_old_node_commands_and_explicit_state_controls() {
        assert_eq!(parse_args(&args(&["node"])), Ok(Action::NodeHelp));
        assert_eq!(
            parse_args(&args(&["node", "focus", "--help"])),
            Ok(Action::NodeHelp)
        );
        assert_eq!(
            parse_args(&args(&["node", "list", "-o", "DP-1", "--json"])),
            Ok(Action::Node {
                request: NodeRequest::List {
                    output: Some("DP-1".into())
                },
                output: NodeOutput::List { json: true },
            })
        );
        assert_eq!(
            parse_args(&args(&["node", "focus", "42"])),
            Ok(Action::Node {
                request: NodeRequest::Focus {
                    selector: Some(NodeSelector::Id(42)),
                    output: None,
                },
                output: NodeOutput::Ack,
            })
        );
        assert_eq!(
            parse_args(&args(&["node", "collapse", "focused"])),
            Ok(Action::Node {
                request: NodeRequest::Collapse {
                    selector: Some(NodeSelector::Focused),
                    output: None,
                },
                output: NodeOutput::Ack,
            })
        );
        assert_eq!(
            parse_args(&args(&["node", "restore", "latest", "-o", "DP-1"])),
            Ok(Action::Node {
                request: NodeRequest::Restore {
                    selector: Some(NodeSelector::Latest),
                    output: Some("DP-1".into()),
                },
                output: NodeOutput::Ack,
            })
        );
        assert_eq!(
            parse_args(&args(&["node", "toggle", "id:7"])),
            Ok(Action::Node {
                request: NodeRequest::Toggle {
                    selector: Some(NodeSelector::Id(7)),
                    output: None,
                },
                output: NodeOutput::Ack,
            })
        );
        assert_eq!(
            parse_args(&args(&[
                "node",
                "move",
                "left",
                "app:firefox",
                "--output",
                "DP-2"
            ])),
            Ok(Action::Node {
                request: NodeRequest::Move {
                    direction: NodeMoveDirection::Left,
                    selector: Some(NodeSelector::App("firefox".into())),
                    output: Some("DP-2".into()),
                },
                output: NodeOutput::Ack,
            })
        );
        assert!(parse_args(&args(&["node", "close", "title:term", "--json"])).is_err());
        assert!(parse_args(&args(&["node", "collapse", "--json"])).is_err());
    }

    fn mode(width: i32, height: i32, refresh_millihz: i32, preferred: bool) -> ModeInfo {
        ModeInfo {
            width,
            height,
            refresh_millihz,
            preferred,
        }
    }

    #[test]
    fn formats_every_available_mode_with_exact_refresh_and_qualifiers() {
        let output = OutputInfo {
            name: "DP-1".to_string(),
            modes: vec![
                mode(2560, 1440, 179_998, true),
                mode(2560, 1440, 143_912, false),
                mode(1920, 1080, 60_000, false),
            ],
            current_mode: Some(0),
            offset_x: 2560,
            offset_y: 0,
            vrr: "auto".to_string(),
        };

        assert_eq!(
            format_output(&output).unwrap(),
            "\
DP-1
  Current mode: 2560x1440 @ 179.998 Hz (preferred)
  Position: 2560, 0
  VRR: auto
  Available modes:
    2560x1440@179.998 (current, preferred)
    2560x1440@143.912
    1920x1080@60.000
"
        );
    }

    #[test]
    fn formats_connected_but_inactive_output_with_its_modes() {
        let output = OutputInfo {
            name: "HDMI-A-1".to_string(),
            modes: vec![mode(1920, 1080, 60_000, true)],
            current_mode: None,
            offset_x: 0,
            offset_y: 0,
            vrr: "off".to_string(),
        };

        assert_eq!(
            format_output(&output).unwrap(),
            "\
HDMI-A-1
  Disabled
  Available modes:
    1920x1080@60.000 (preferred)
"
        );
    }

    #[test]
    fn rejects_current_mode_index_outside_available_modes() {
        let output = OutputInfo {
            name: "DP-1".to_string(),
            modes: Vec::new(),
            current_mode: Some(1),
            offset_x: 0,
            offset_y: 0,
            vrr: "off".to_string(),
        };

        assert_eq!(
            format_output(&output),
            Err("DP-1 refers to missing current mode index 1".to_string())
        );
    }
}
