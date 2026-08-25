use std::env;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use halley_api::Client;
use halley_config::{
    GamescopeDecision, RuntimeConfig, TargetDims, build_gamescope_argv, resolve_profile,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamescopeMode {
    Run,
    Print,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamescopeInvocation {
    pub mode: GamescopeMode,
    pub app_id: Option<String>,
    pub command: Vec<String>,
}

pub(super) fn parse(args: &[String]) -> Result<super::Action, String> {
    let Some(mode) = args.first().map(String::as_str) else {
        return Ok(super::Action::GamescopeHelp);
    };
    let mode = match mode {
        "run" => GamescopeMode::Run,
        "print" => GamescopeMode::Print,
        "help" | "-h" | "--help" => return Ok(super::Action::GamescopeHelp),
        other => return Err(format!("unknown gamescope command {other:?}")),
    };
    let mut app_id = None;
    let mut index = 1;
    let mut separator = None;
    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                separator = Some(index + 1);
                break;
            }
            "--app-id" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "gamescope --app-id requires a value".to_string())?;
                if app_id.replace(value.clone()).is_some() {
                    return Err("gamescope --app-id was specified more than once".to_string());
                }
            }
            other => {
                return Err(format!(
                    "unexpected gamescope argument {other:?} before `--`"
                ));
            }
        }
        index += 1;
    }
    let start = separator.ok_or_else(|| {
        "gamescope requires `--` before the game command (for example: gamescope run -- game)"
            .to_string()
    })?;
    let command = args[start..].to_vec();
    if command.is_empty() {
        return Err("gamescope requires a game command after `--`".to_string());
    }
    Ok(super::Action::Gamescope(GamescopeInvocation {
        mode,
        app_id,
        command,
    }))
}

pub fn run(invocation: GamescopeInvocation) -> ExitCode {
    let config = load_runtime_config();
    let app_id = invocation.app_id.clone().or_else(steam_app_id_from_env);
    match resolve_profile(&config.gaming.gamescope, app_id.as_deref()) {
        GamescopeDecision::Disabled => finish(invocation.mode, invocation.command),
        GamescopeDecision::Skip => {
            eprintln!(
                "halleyctl gamescope: profile for {} is disabled; running unwrapped",
                app_id.as_deref().unwrap_or("<unknown app-id>")
            );
            finish(invocation.mode, invocation.command)
        }
        GamescopeDecision::Wrap(profile) => {
            if !command_exists("gamescope") {
                eprintln!(
                    "halleyctl gamescope: gamescope was not found in PATH; running unwrapped"
                );
                return finish(invocation.mode, invocation.command);
            }
            let target = resolve_target(&profile.monitor);
            let (arguments, diagnostics) =
                build_gamescope_argv(&profile, &target, &invocation.command);
            for diagnostic in diagnostics {
                eprintln!("halleyctl {diagnostic}");
            }
            finish(invocation.mode, arguments)
        }
    }
}

fn finish(mode: GamescopeMode, arguments: Vec<String>) -> ExitCode {
    match mode {
        GamescopeMode::Print => {
            println!("{}", shell_join(&arguments));
            ExitCode::SUCCESS
        }
        GamescopeMode::Run => {
            let error = Command::new(&arguments[0]).args(&arguments[1..]).exec();
            eprintln!(
                "halleyctl gamescope: failed to execute {:?}: {error}",
                arguments[0]
            );
            ExitCode::from(127)
        }
    }
}

fn resolve_target(selector: &str) -> TargetDims {
    match Client::connect().and_then(|client| client.gamescope_target(selector)) {
        Ok(target) => TargetDims {
            width: (target.width > 0).then_some(target.width),
            height: (target.height > 0).then_some(target.height),
            refresh_hz: target.refresh_hz,
        },
        Err(error) => {
            eprintln!(
                "halleyctl gamescope: could not resolve monitor {selector:?} ({error}); using gamescope auto-detection"
            );
            TargetDims::default()
        }
    }
}

fn load_runtime_config() -> RuntimeConfig {
    let Some(path) = resolve_config_path() else {
        return RuntimeConfig::default();
    };
    match halley_config::load_runtime_config_at(&path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "halleyctl gamescope: could not load config {} ({error}); using built-in gamescope defaults",
                path.display()
            );
            RuntimeConfig::default()
        }
    }
}

fn resolve_config_path() -> Option<PathBuf> {
    env::var_os("HALLEY_WL_CONFIG")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            Client::connect()
                .ok()
                .and_then(|client| client.config_path().ok())
                .flatten()
        })
        .or_else(halley_config::config_path)
        .filter(|path| path.is_file())
}

fn steam_app_id_from_env() -> Option<String> {
    ["SteamAppId", "SteamGameId"].into_iter().find_map(|key| {
        let value = env::var(key).ok()?;
        let value = value.trim();
        (!value.is_empty() && value.chars().all(|character| character.is_ascii_digit()))
            .then(|| format!("steam_app_{value}"))
    })
}

fn command_exists(command: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|directory| {
            let candidate = directory.join(command);
            candidate.is_file() && is_executable(&candidate)
        })
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn shell_join(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '/' | '.' | ':' | '=')
        })
    {
        argument.to_string()
    } else {
        format!("'{}'", argument.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parser_requires_separator_and_preserves_game_arguments() {
        assert_eq!(
            parse(&args(&[
                "print",
                "--app-id",
                "steam_app_42",
                "--",
                "game",
                "--flag",
            ])),
            Ok(super::super::Action::Gamescope(GamescopeInvocation {
                mode: GamescopeMode::Print,
                app_id: Some("steam_app_42".into()),
                command: vec!["game".into(), "--flag".into()],
            }))
        );
        assert!(parse(&args(&["run", "game"])).is_err());
        assert!(parse(&args(&["run", "--"])).is_err());
    }

    #[test]
    fn shell_output_quotes_unsafe_arguments() {
        assert_eq!(
            shell_join(&args(&["game", "two words"])),
            "game 'two words'"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
