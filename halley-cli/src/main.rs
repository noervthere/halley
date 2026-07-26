use std::fmt::Write;
use std::process::ExitCode;

use halley_ipc::{ModeInfo, OutputInfo, Request, Response};

/// Hand-rolled arg parsing, no `clap` - matches old halley's own
/// `halley-cli` (which had the same "no dependency without a concrete
/// need" instinct), and there's exactly one real subcommand so far.
const HELP: &str = "\
Usage: halleyctl <command>

Commands:
  outputs        List connected monitors and their current mode/position

Options:
  -h, --help     Print this message
  -V, --version  Print both halleyctl's and the running compositor's version
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Outputs,
    Version,
    Help,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Action::Outputs) => query(Request::Outputs, print_outputs),
        Ok(Action::Version) => query(Request::Version, print_version),
        Ok(Action::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("halleyctl: {err}\n\n{HELP}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: &[String]) -> Result<Action, String> {
    let action = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => Action::Help,
        Some("--version") | Some("-V") => Action::Version,
        Some("outputs") => Action::Outputs,
        Some(other) => return Err(format!("unknown command {other:?}")),
    };

    if let Some(unexpected) = args.get(1) {
        return Err(format!("unexpected argument {unexpected:?}"));
    }

    Ok(action)
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
        assert!(!commands.contains("--help"));
        assert!(!commands.contains("--version"));
        assert!(HELP.contains("Options:\n  -h, --help"));
        assert!(HELP.contains("  -V, --version"));
    }

    #[test]
    fn parser_accepts_only_complete_supported_invocations() {
        assert_eq!(parse_args(&[]), Ok(Action::Help));
        assert_eq!(parse_args(&args(&["outputs"])), Ok(Action::Outputs));
        assert_eq!(parse_args(&args(&["--help"])), Ok(Action::Help));
        assert_eq!(parse_args(&args(&["-h"])), Ok(Action::Help));
        assert_eq!(parse_args(&args(&["--version"])), Ok(Action::Version));
        assert_eq!(parse_args(&args(&["-V"])), Ok(Action::Version));
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
