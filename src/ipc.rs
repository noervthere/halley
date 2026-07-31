use std::cell::RefCell;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;

use calloop::channel::{Event, Sender, channel};
use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode as CalloopMode, PostAction};
use smithay::output::Mode as OutputMode;

const MAX_REQUEST_FDS: usize = 32;

/// Backend-agnostic access to real per-output info, mirroring `Renderable`'s
/// existing shape (one small trait, implemented once per backend) rather
/// than threading backend-specific types through the IPC layer.
pub trait OutputInfoSource {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo>;
}

/// Shared by both backends' `OutputInfoSource` impls - keeps Smithay's
/// millihertz refresh representation intact on the wire.
pub fn mode_info(mode: OutputMode, preferred: bool) -> halley_ipc::ModeInfo {
    halley_ipc::ModeInfo {
        width: mode.size.w,
        height: mode.size.h,
        refresh_millihz: mode.refresh,
        preferred,
    }
}

pub fn vrr_str(vrr: halley_config::Vrr) -> &'static str {
    match vrr {
        halley_config::Vrr::Off => "off",
        halley_config::Vrr::On => "on",
        halley_config::Vrr::Auto => "auto",
    }
}

fn version_info() -> halley_ipc::VersionInfo {
    halley_ipc::VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        ipc_protocol: halley_ipc::HALLEY_IPC_VERSION,
    }
}

struct ReplyFrame {
    response: halley_ipc::Response,
    fds: Vec<OwnedFd>,
}

/// The reply half of one IPC request. It is deliberately consumed by
/// `send`, making double replies impossible while allowing a user-driven
/// operation to retain it until selection completes.
pub struct ReplySender(mpsc::SyncSender<ReplyFrame>);

impl ReplySender {
    pub fn send(
        self,
        response: halley_ipc::Response,
        fds: Vec<OwnedFd>,
    ) -> Result<(), Box<halley_ipc::Response>> {
        self.0
            .send(ReplyFrame { response, fds })
            .map_err(|err| Box::new(err.0.response))
    }
}

/// One request delivered on the compositor thread.
pub struct RequestEnvelope {
    pub request: halley_ipc::Request,
    pub fds: Vec<OwnedFd>,
    pub reply: ReplySender,
}

fn client_worker(stream: UnixStream, requests: Sender<RequestEnvelope>) {
    loop {
        let (bytes, fds) = match halley_ipc::read_frame_with_fds(&stream, MAX_REQUEST_FDS) {
            Ok(frame) => frame,
            Err(halley_ipc::CodecError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                break;
            }
            Err(err) => {
                eventline::warn!("ipc: failed to read request: {err}");
                break;
            }
        };
        let request = match halley_ipc::decode_request(&bytes) {
            Ok(request) => request,
            Err(err) => {
                if write_response(
                    &stream,
                    ReplyFrame {
                        response: halley_ipc::Response::Error(err.to_string()),
                        fds: Vec::new(),
                    },
                )
                .is_err()
                {
                    break;
                }
                continue;
            }
        };

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if requests
            .send(RequestEnvelope {
                request,
                fds,
                reply: ReplySender(reply_tx),
            })
            .is_err()
        {
            break;
        }
        let Ok(reply) = reply_rx.recv() else {
            break;
        };
        if let Err(err) = write_response(&stream, reply) {
            eventline::warn!("ipc: failed to write response: {err}");
            break;
        }
    }
}

fn write_response(stream: &UnixStream, reply: ReplyFrame) -> Result<(), halley_ipc::CodecError> {
    let bytes = halley_ipc::encode_response(&reply.response)?;
    let fds = reply.fds.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
    halley_ipc::write_frame_with_fds(stream, &bytes, &fds)
}

/// If a socket file already exists at `path`, checks whether it's actually
/// live (another halley IPC listener still holds it) or stale (the process
/// that created it is gone) - refuses to start in the former case, removes
/// the file and proceeds in the latter. Mirrors old halley's own
/// stale-socket handling.
fn remove_stale_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::other(
            "another halley IPC listener is already active on this socket",
        )),
        Err(_) => std::fs::remove_file(path),
    }
}

/// Binds the IPC socket and routes decoded requests onto the compositor
/// loop. Socket I/O waits on per-connection workers, so a deferred portal
/// reply never blocks rendering or input dispatch.
pub fn init_ipc_listener<App: 'static>(
    loop_handle: &LoopHandle<'_, App>,
    handler: impl Fn(&mut App, RequestEnvelope) + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = halley_ipc::ensure_runtime_dir()?.join("halley.sock");
    remove_stale_socket(&path)?;

    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    let (request_tx, request_rx) = channel();
    loop_handle.insert_source(request_rx, move |event, _, app| {
        if let Event::Msg(request) = event {
            handler(app, request);
        }
    })?;

    loop_handle.insert_source(
        Generic::new(listener, Interest::READ, CalloopMode::Level),
        move |_, listener, _app| {
            loop {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let requests = request_tx.clone();
                        if let Err(err) = std::thread::Builder::new()
                            .name("halley-ipc-client".to_string())
                            .spawn(move || client_worker(stream, requests))
                        {
                            eventline::error!("ipc: failed to start client worker: {err}");
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(err) => {
                        eventline::error!("ipc: accept failed: {err}");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    )?;

    Ok(())
}

pub fn handle_request<D: crate::session::SessionDriver>(
    app: &mut crate::session::Session<D>,
    request: RequestEnvelope,
) {
    let RequestEnvelope {
        request,
        fds,
        reply,
    } = request;
    let accepts_descriptors = matches!(
        request,
        halley_ipc::Request::RegisterDmabuf(_) | halley_ipc::Request::CaptureFrame(_)
    );
    if !accepts_descriptors && !fds.is_empty() {
        let _ = reply.send(
            halley_ipc::Response::Error("request included unexpected descriptors".to_string()),
            Vec::new(),
        );
        return;
    }
    let response = match request {
        halley_ipc::Request::Outputs => {
            halley_ipc::Response::Outputs(halley_ipc::OutputsResponse {
                outputs: app.driver.output_info(),
            })
        }
        halley_ipc::Request::Version => halley_ipc::Response::Version(version_info()),
        halley_ipc::Request::Screenshot(request) => {
            crate::capture::request_screenshot(app, request, reply);
            return;
        }
        halley_ipc::Request::CancelScreenshot { request_handle } => {
            if crate::capture::cancel_portal(app, &request_handle) {
                halley_ipc::Response::Ack
            } else {
                halley_ipc::Response::Error("screenshot request is not active".to_string())
            }
        }
        halley_ipc::Request::ChooseSource(request) => {
            crate::capture::request_source(app, request, reply);
            return;
        }
        halley_ipc::Request::CancelSourceChooser { request_handle } => {
            if crate::capture::cancel_portal(app, &request_handle) {
                halley_ipc::Response::Ack
            } else {
                halley_ipc::Response::Error("source chooser is not active".to_string())
            }
        }
        halley_ipc::Request::RegisterDmabuf(request) => {
            match app.screencast.register(request, fds) {
                Ok(()) => halley_ipc::Response::Ack,
                Err(message) => halley_ipc::Response::Error(message),
            }
        }
        halley_ipc::Request::RemoveDmabuf {
            stream_handle,
            buffer_id,
        } => {
            app.screencast.remove(&stream_handle, buffer_id);
            halley_ipc::Response::Ack
        }
        halley_ipc::Request::CaptureFrame(request) => {
            match crate::capture::screencast::capture_frame(app, request, fds) {
                Ok(crate::capture::screencast::CaptureFrameResult::Immediate(response)) => {
                    halley_ipc::Response::Frame(response)
                }
                Ok(crate::capture::screencast::CaptureFrameResult::Submitted {
                    response,
                    sync,
                }) => {
                    let pending_reply = Rc::new(RefCell::new(Some(reply)));
                    let completion_reply = pending_reply.clone();
                    let completion = Box::new(move || {
                        if let Some(reply) = completion_reply.borrow_mut().take() {
                            let _ = reply.send(halley_ipc::Response::Frame(response), Vec::new());
                        }
                    });
                    if let Err(message) = app.driver.schedule_render_completion(sync, completion)
                        && let Some(reply) = pending_reply.borrow_mut().take()
                    {
                        let _ = reply.send(halley_ipc::Response::Error(message), Vec::new());
                    }
                    return;
                }
                Err(message) => halley_ipc::Response::Error(message),
            }
        }
        halley_ipc::Request::Node(request) => crate::nodes::handle_request(app, request),
        halley_ipc::Request::Bearings(request) => match request {
            halley_ipc::BearingsRequest::Show => {
                if app.bearings.set_visible(true) {
                    app.request_redraw();
                }
                halley_ipc::Response::Ack
            }
            halley_ipc::BearingsRequest::Hide => {
                if app.bearings.set_visible(false) {
                    app.request_redraw();
                }
                halley_ipc::Response::Ack
            }
            halley_ipc::BearingsRequest::Toggle => {
                app.bearings.toggle();
                app.request_redraw();
                halley_ipc::Response::Ack
            }
            halley_ipc::BearingsRequest::Status => {
                halley_ipc::Response::BearingsStatus(halley_ipc::BearingsStatusResponse {
                    visible: app.bearings.visible(),
                })
            }
        },
        halley_ipc::Request::Quit => {
            app.show_exit_confirmation();
            halley_ipc::Response::Ack
        }
        halley_ipc::Request::ConfigPath => halley_ipc::Response::ConfigPath(
            app.config_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        ),
        halley_ipc::Request::Dpms { command, output } => {
            match app.driver.apply_dpms(command, output.as_deref()) {
                Ok(()) => {
                    crate::wayland::session_lock::confirm_unlit_outputs(app);
                    halley_ipc::Response::Ack
                }
                Err(message) => halley_ipc::Response::Error(message),
            }
        }
        halley_ipc::Request::Cluster(request) => crate::clusters::handle_request(app, request),
        halley_ipc::Request::CaptureCapabilities => {
            let capabilities = app.driver.dmabuf_capabilities();
            halley_ipc::Response::CaptureCapabilities(halley_ipc::CaptureCapabilities {
                main_device: capabilities.main_device(),
                dmabuf_formats: capabilities
                    .formats()
                    .iter()
                    .map(|format| halley_ipc::DmabufFormat {
                        fourcc: format.code as u32,
                        modifier: format.modifier.into(),
                    })
                    .collect(),
            })
        }
    };
    let _ = reply.send(response, Vec::new());
}
