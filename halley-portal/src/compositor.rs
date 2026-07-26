use halley_ipc::{
    Connection, Request, Response, ScreenshotRequest, ScreenshotResponse, ScreenshotTarget,
};

pub fn screenshot(
    request_handle: String,
    target: ScreenshotTarget,
) -> Result<ScreenshotResponse, String> {
    let mut connection =
        Connection::connect().map_err(|err| format!("connect to compositor: {err}"))?;
    let response = connection
        .request(
            &Request::Screenshot(ScreenshotRequest {
                request_handle,
                target,
            }),
            &[],
        )
        .map_err(|err| format!("compositor screenshot request: {err}"))?;
    if !response.fds.is_empty() {
        return Err("compositor returned unexpected screenshot descriptors".to_string());
    }
    match response.response {
        Response::Screenshot(response) => Ok(response),
        Response::Error(message) => Err(message),
        other => Err(format!("unexpected compositor response: {other:?}")),
    }
}

pub fn cancel_screenshot(request_handle: String) -> Result<(), String> {
    let mut connection =
        Connection::connect().map_err(|err| format!("connect to compositor: {err}"))?;
    let response = connection
        .request(&Request::CancelScreenshot { request_handle }, &[])
        .map_err(|err| format!("cancel compositor screenshot: {err}"))?;
    match response.response {
        Response::Ack => Ok(()),
        Response::Error(message) => Err(message),
        other => Err(format!("unexpected compositor response: {other:?}")),
    }
}
