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
    for output in &outputs.outputs {
        print_output(output);
    }
    ExitCode::SUCCESS
}

fn print_output(output: &OutputInfo) {
    println!("{}", output.name);
    match &output.current_mode {
        Some(ModeInfo {
            width,
            height,
            refresh_hz,
        }) => {
            println!("  mode: {width}x{height} @ {refresh_hz:.3}Hz");
        }
        None => println!("  mode: (none)"),
    }
    println!("  position: {}, {}", output.offset_x, output.offset_y);
    println!("  vrr: {}", output.vrr);
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
}
