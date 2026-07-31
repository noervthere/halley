use std::process::ExitCode;

use halley_ipc::{Request, Response};

/// All commands use one transport boundary so socket errors and response
/// ownership do not leak into parsing or presentation modules.
pub fn query(request: Request, on_response: impl FnOnce(Response) -> ExitCode) -> ExitCode {
    match halley_ipc::send_request(&request) {
        Ok(response) => on_response(response),
        Err(err) => {
            eprintln!("halleyctl: failed to reach the running compositor: {err}");
            ExitCode::FAILURE
        }
    }
}
