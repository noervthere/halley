mod accessibility;
mod animation;
mod apogee;
mod backend;
mod camera;
mod capture;
mod config;
mod cursor;
mod focus_cycle;
mod frame_clock;
mod input;
mod ipc;
mod logging;
mod nodes;
mod screencast;
mod session;
mod wayland;
mod window;
mod xwayland;

fn main() {
    logging::init();
    let args = match StartupArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("halley: {err}");
            eprintln!("Try `halley --help` for usage.");
            std::process::exit(2);
        }
    };
    if args.help {
        print_help();
        return;
    }
    if args.version {
        println!("halley {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.session {
        session::environment::prepare_session();
        session::tty::run(true);
    } else if args.force_winit || detect_nested_session() {
        session::winit::run();
    } else {
        session::tty::run(false);
    }
    logging::flush();
}

#[derive(Default)]
struct StartupArgs {
    force_winit: bool,
    session: bool,
    help: bool,
    version: bool,
}

impl StartupArgs {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut parsed = Self::default();
        for arg in args {
            match arg.as_str() {
                "--winit" => parsed.force_winit = true,
                "--session" => parsed.session = true,
                "-h" | "--help" => parsed.help = true,
                "-V" | "--version" => parsed.version = true,
                _ => return Err(format!("unknown argument {arg:?}")),
            }
        }
        if parsed.force_winit && parsed.session {
            return Err("`--winit` and `--session` cannot be used together".to_string());
        }
        Ok(parsed)
    }
}

fn print_help() {
    println!("halley {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Usage: halley [OPTIONS]");
    println!();
    println!("Options:");
    println!("      --session  Start a full TTY desktop session");
    println!("      --winit    Run nested inside the current desktop");
    println!("  -h, --help     Show this help");
    println!("  -V, --version  Show the version");
}

/// Whether we're already running inside another Wayland/X compositor - if
/// so, taking over real hardware (the tty/DRM session) would be wrong even
/// without `--winit`. A `WAYLAND_DISPLAY` or `DISPLAY` already set in the
/// environment means a host compositor/X server is available to nest under.
fn detect_nested_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

#[cfg(test)]
mod tests {
    use super::StartupArgs;

    #[test]
    fn session_and_winit_are_mutually_exclusive() {
        assert!(StartupArgs::parse(["--session".to_string(), "--winit".to_string()]).is_err());
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(StartupArgs::parse(["--mystery".to_string()]).is_err());
    }
}
