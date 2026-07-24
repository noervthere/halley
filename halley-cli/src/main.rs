use std::process::ExitCode;

use halley_ipc::{ModeInfo, OutputInfo, Request, Response};

/// Hand-rolled arg parsing, no `clap` - matches old halley's own
/// `halley-cli` (which had the same "no dependency without a concrete
/// need" instinct), and there's exactly one real subcommand so far.
const USAGE: &str = "\
Usage: halleyctl <command>

Commands:
  outputs        List connected monitors and their current mode/position
  --version, -V  Print both halleyctl's and the running compositor's version
  --help, -h     Print this message
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("outputs") => query(Request::Outputs, print_outputs),
        Some("--version") | Some("-V") => query(Request::Version, print_version),
        Some("--help") | Some("-h") | None => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("halleyctl: unknown command {other:?}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
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
        Some(ModeInfo { width, height, refresh_hz }) => {
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
    println!("compositor {} (ipc protocol {})", version.version, version.ipc_protocol);
    ExitCode::SUCCESS
}

fn print_unexpected(resp: Response) -> ExitCode {
    match resp {
        Response::Error(msg) => eprintln!("halleyctl: compositor returned an error: {msg}"),
        other => eprintln!("halleyctl: unexpected response: {other:?}"),
    }
    ExitCode::FAILURE
}
