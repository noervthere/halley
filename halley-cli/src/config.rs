use std::fmt::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use halley_ipc::{Request, Response};

pub fn verify(explicit: Option<PathBuf>) -> ExitCode {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
