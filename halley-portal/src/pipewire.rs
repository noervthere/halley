use std::collections::HashMap;
use std::io::Cursor;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::loop_::Timeout;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::PropertiesBox;
use pipewire::spa;
use pipewire::spa::buffer::DataType;
use pipewire::spa::sys as spa_sys;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};

const CURSOR_META_SIZE: usize = std::mem::size_of::<spa_sys::spa_meta_cursor>()
    + std::mem::size_of::<spa_sys::spa_meta_bitmap>()
    + 256 * 256 * 4;
// Halley's DMA-BUF capture path is retained for future explicit-sync work, but
// it must not be negotiated until the producer can signal frame completion to
// PipeWire. Mapped MemFd buffers are coherent when the process callback queues
// them and are therefore the reliable default for every source/cursor mode.
const ADVERTISE_DMABUF: bool = false;

enum Command {
    Create {
        handle: String,
        source: halley_ipc::CaptureSource,
        cursor_mode: halley_ipc::CursorMode,
        reply: Sender<Result<(u32, Option<u64>), String>>,
    },
    Destroy(String),
    Quit,
}

struct ActiveStream {
    stream: StreamRc,
    _listener: StreamListener<u64>,
}

pub struct Producer {
    commands: Sender<Command>,
    quit: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Producer {
    pub fn new() -> Self {
        let (commands, receiver) = mpsc::channel();
        let quit = Arc::new(AtomicBool::new(false));
        let thread_quit = quit.clone();
        let thread = std::thread::Builder::new()
            .name("halley-pipewire".to_string())
            .spawn(move || run_thread(receiver, thread_quit))
            .expect("failed to start PipeWire thread");
        Self {
            commands,
            quit,
            thread: Some(thread),
        }
    }

    pub fn create_stream(
        &self,
        handle: &str,
        source: halley_ipc::CaptureSource,
        cursor_mode: halley_ipc::CursorMode,
    ) -> Result<(u32, Option<u64>), String> {
        let (reply, receiver) = mpsc::channel();
        self.commands
            .send(Command::Create {
                handle: handle.to_string(),
                source,
                cursor_mode,
                reply,
            })
            .map_err(|_| "PipeWire thread stopped".to_string())?;
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|err| format!("timed out creating PipeWire stream: {err}"))?
    }

    pub fn destroy_stream(&self, handle: &str) {
        let _ = self.commands.send(Command::Destroy(handle.to_string()));
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_thread(commands: Receiver<Command>, quit: Arc<AtomicBool>) {
    pipewire::init();
    let mainloop = match MainLoopRc::new(None) {
        Ok(mainloop) => mainloop,
        Err(err) => {
            eventline::error!("PipeWire main loop: {err}");
            return;
        }
    };
    let context = match ContextRc::new(&mainloop, None) {
        Ok(context) => context,
        Err(err) => {
            eventline::error!("PipeWire context: {err}");
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(core) => core,
        Err(err) => {
            eventline::error!("PipeWire connection: {err}");
            return;
        }
    };
    let mut streams = HashMap::new();
    while !quit.load(Ordering::Relaxed) {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Create {
                    handle,
                    source,
                    cursor_mode,
                    reply,
                } => {
                    let result = create_stream(&mainloop, &core, &handle, source, cursor_mode).map(
                        |(stream, listener, node, serial)| {
                            streams.insert(
                                handle,
                                ActiveStream {
                                    stream,
                                    _listener: listener,
                                },
                            );
                            (node, serial)
                        },
                    );
                    let _ = reply.send(result);
                }
                Command::Destroy(handle) => {
                    if let Some(active) = streams.remove(&handle) {
                        let _ = active.stream.disconnect();
                    }
                }
                Command::Quit => return,
            }
        }
        let _ = mainloop
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(16)));
    }
}

fn create_stream(
    mainloop: &MainLoopRc,
    core: &CoreRc,
    handle: &str,
    source: halley_ipc::CaptureSource,
    cursor_mode: halley_ipc::CursorMode,
) -> Result<(StreamRc, StreamListener<u64>, u32, Option<u64>), String> {
    let width = source.width() as u32;
    let height = source.height() as u32;
    let mut properties = PropertiesBox::new();
    properties.insert("media.class", "Video/Source");
    properties.insert("media.name", format!("halley-screencast-{handle}"));
    properties.insert("media.role", "Screen");
    properties.insert("node.name", "xdg-desktop-portal-halley");
    properties.insert("node.pause-on-idle", "false");
    properties.insert("stream.is-live", "true");
    let stream = StreamRc::new(
        core.clone(),
        &format!("halley-screencast-{handle}"),
        properties,
    )
    .map_err(|err| format!("create PipeWire stream: {err}"))?;

    let process_handle = handle.to_string();
    let process_source = source.clone();
    let listener = stream
        .add_local_listener_with_user_data(0u64)
        .add_buffer({
            let handle = handle.to_string();
            move |_stream, _frame_count, buffer| {
                let Some((buffer_id, planes, fds)) = dmabuf_buffer_info(buffer, width) else {
                    return;
                };
                let request = halley_ipc::RegisterDmabufRequest {
                    stream_handle: handle.clone(),
                    buffer_id,
                    width: width as i32,
                    height: height as i32,
                    format: u32::from_le_bytes(*b"XR24"),
                    modifier: u64::MAX,
                    flags: 0,
                    planes,
                };
                if let Err(err) = crate::compositor::register_dmabuf(request, &fds) {
                    eventline::debug!("DMA-BUF registration rejected: {err}");
                }
            }
        })
        .remove_buffer({
            let handle = handle.to_string();
            move |_stream, _frame_count, buffer| {
                if let Some((buffer_id, _, _)) = dmabuf_buffer_info(buffer, width) {
                    let _ = crate::compositor::remove_dmabuf(handle.clone(), buffer_id);
                }
            }
        })
        .process(move |stream, frame_count| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let response = {
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let raw = data.as_raw();
                let buffer = match data.type_() {
                    DataType::MemFd if data.fd() >= 0 => halley_ipc::CaptureBuffer::MemFd {
                        fd_index: 0,
                        offset: u64::from(raw.mapoffset),
                        size: u64::from(raw.maxsize),
                        stride: width * 4,
                    },
                    DataType::DmaBuf => halley_ipc::CaptureBuffer::Dmabuf {
                        buffer_id: data.fd() as u64,
                    },
                    _ => return,
                };
                let request = halley_ipc::CaptureFrameRequest {
                    stream_handle: process_handle.clone(),
                    source: process_source.clone(),
                    cursor_mode,
                    buffer,
                };
                let fd = (data.type_() == DataType::MemFd).then_some(data.fd());
                match crate::compositor::capture_frame_optional(request, fd) {
                    Ok(response) => {
                        let chunk = data.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.size_mut() = width * height * 4;
                        *chunk.stride_mut() = (width * 4) as i32;
                        *frame_count = frame_count.wrapping_add(1);
                        response
                    }
                    Err(err) => {
                        eventline::warn!("screencast frame failed: {err}");
                        return;
                    }
                }
            };
            fill_cursor_meta(&buffer, response.cursor.as_ref());
        })
        .register()
        .map_err(|err| format!("register PipeWire listener: {err}"))?;

    let format_data = format_pod(width, height)?;
    let buffers_data = buffers_pod(width, height, ADVERTISE_DMABUF)?;
    let meta_data = meta_pod();
    let format = spa::pod::Pod::from_bytes(&format_data)
        .ok_or_else(|| "invalid PipeWire format POD".to_string())?;
    let buffers = spa::pod::Pod::from_bytes(&buffers_data)
        .ok_or_else(|| "invalid PipeWire buffers POD".to_string())?;
    let meta = spa::pod::Pod::from_bytes(&meta_data)
        .ok_or_else(|| "invalid PipeWire cursor POD".to_string())?;
    let mut params = [format, buffers, meta];
    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|err| format!("connect PipeWire stream: {err}"))?;
    stream
        .set_active(true)
        .map_err(|err| format!("activate PipeWire stream: {err}"))?;
    let node = wait_for_node(mainloop, &stream)?;
    let serial = stream
        .properties()
        .get("object.serial")
        .and_then(|value| value.parse().ok());
    Ok((stream, listener, node, serial))
}

fn wait_for_node(mainloop: &MainLoopRc, stream: &StreamRc) -> Result<u32, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let node = stream.node_id();
        if node != pipewire::constants::ID_ANY {
            return Ok(node);
        }
        if std::time::Instant::now() >= deadline {
            return Err("PipeWire did not assign a node id".to_string());
        }
        let _ = mainloop
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(16)));
    }
}

fn format_pod(width: u32, height: u32) -> Result<Vec<u8>, String> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Id,
            spa::param::video::VideoFormat::BGRx
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            spa::utils::Rectangle { width, height }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 1, denom: 1 },
            spa::utils::Fraction { num: 360, denom: 1 }
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|result| result.0.into_inner())
    .map_err(|err| format!("serialize PipeWire format: {err}"))
}

fn buffer_data_types(allow_dmabuf: bool) -> i32 {
    (1 << spa_sys::SPA_DATA_MemFd)
        | if allow_dmabuf {
            1 << spa_sys::SPA_DATA_DmaBuf
        } else {
            0
        }
}

fn buffers_pod(width: u32, height: u32, allow_dmabuf: bool) -> Result<Vec<u8>, String> {
    use spa::pod::Property;
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamBuffers,
        spa::param::ParamType::Buffers,
        Property::new(spa_sys::SPA_PARAM_BUFFERS_blocks, spa::pod::Value::Int(1)),
        Property::new(
            spa_sys::SPA_PARAM_BUFFERS_size,
            spa::pod::Value::Int((width * height * 4) as i32)
        ),
        Property::new(
            spa_sys::SPA_PARAM_BUFFERS_stride,
            spa::pod::Value::Int((width * 4) as i32)
        ),
        Property::new(spa_sys::SPA_PARAM_BUFFERS_align, spa::pod::Value::Int(16)),
        Property::new(
            spa_sys::SPA_PARAM_BUFFERS_dataType,
            spa::pod::Value::Int(buffer_data_types(allow_dmabuf))
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|result| result.0.into_inner())
    .map_err(|err| format!("serialize PipeWire buffers: {err}"))
}

fn dmabuf_buffer_info(
    buffer: *mut pipewire::sys::pw_buffer,
    width: u32,
) -> Option<(u64, Vec<halley_ipc::DmabufPlane>, Vec<RawFd>)> {
    if buffer.is_null() {
        return None;
    }
    let spa_buffer = unsafe { (*buffer).buffer };
    if spa_buffer.is_null() {
        return None;
    }
    let count = unsafe { (*spa_buffer).n_datas as usize };
    let datas = unsafe { (*spa_buffer).datas };
    if count == 0 || datas.is_null() {
        return None;
    }
    let mut planes = Vec::with_capacity(count);
    let mut fds = Vec::with_capacity(count);
    for index in 0..count {
        let data = unsafe { &*datas.add(index) };
        if data.type_ != spa_sys::SPA_DATA_DmaBuf || data.fd < 0 {
            return None;
        }
        let stride = if data.chunk.is_null() {
            width * 4
        } else {
            let stride = unsafe { (*data.chunk).stride };
            if stride > 0 { stride as u32 } else { width * 4 }
        };
        planes.push(halley_ipc::DmabufPlane {
            fd_index: index as u32,
            plane_index: index as u32,
            offset: data.mapoffset,
            stride,
        });
        fds.push(data.fd as RawFd);
    }
    Some((fds[0] as u64, planes, fds))
}

fn meta_pod() -> Vec<u8> {
    use spa::pod::builder::Builder;
    let mut data = vec![0u8; 256];
    let mut builder = Builder::new(&mut data);
    let mut frame: MaybeUninit<spa_sys::spa_pod_frame> = MaybeUninit::uninit();
    unsafe {
        builder
            .push_object(
                &mut frame,
                spa_sys::SPA_TYPE_OBJECT_ParamMeta,
                spa_sys::SPA_PARAM_Meta,
            )
            .expect("cursor metadata object");
    }
    builder
        .add_prop(spa_sys::SPA_PARAM_META_type, 0)
        .expect("cursor metadata type");
    builder
        .add_id(spa::utils::Id(spa_sys::SPA_META_Cursor))
        .expect("cursor metadata id");
    builder
        .add_prop(spa_sys::SPA_PARAM_META_size, 0)
        .expect("cursor metadata size");
    builder
        .add_int(CURSOR_META_SIZE as i32)
        .expect("cursor metadata size value");
    unsafe {
        builder.pop(&mut frame.assume_init());
    }
    data
}

fn fill_cursor_meta(
    buffer: &pipewire::buffer::Buffer<'_>,
    metadata: Option<&halley_ipc::CursorMetadata>,
) {
    let Some(meta) = buffer.find_meta::<spa::buffer::meta::MetaCursor>() else {
        return;
    };
    let cursor = meta as *const _ as *mut spa_sys::spa_meta_cursor;
    unsafe {
        let Some(metadata) = metadata else {
            (*cursor).id = 0;
            (*cursor).bitmap_offset = 0;
            return;
        };
        (*cursor).id = 1;
        (*cursor).position.x = metadata.x;
        (*cursor).position.y = metadata.y;
        (*cursor).hotspot.x = metadata.hotspot_x;
        (*cursor).hotspot.y = metadata.hotspot_y;
        let cursor_size = std::mem::size_of::<spa_sys::spa_meta_cursor>();
        let bitmap_size = std::mem::size_of::<spa_sys::spa_meta_bitmap>();
        let bytes = metadata.bgra.len();
        if cursor_size + bitmap_size + bytes > CURSOR_META_SIZE {
            (*cursor).bitmap_offset = 0;
            return;
        }
        (*cursor).bitmap_offset = cursor_size as u32;
        let bitmap = (cursor as *mut u8)
            .add(cursor_size)
            .cast::<spa_sys::spa_meta_bitmap>();
        (*bitmap).format = spa_sys::SPA_VIDEO_FORMAT_BGRA;
        (*bitmap).size.width = metadata.width;
        (*bitmap).size.height = metadata.height;
        (*bitmap).stride = (metadata.width * 4) as i32;
        (*bitmap).offset = bitmap_size as u32;
        std::ptr::copy_nonoverlapping(
            metadata.bgra.as_ptr(),
            (bitmap as *mut u8).add(bitmap_size),
            bytes,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_window_fallback_can_exclude_dmabuf() {
        let data_types = buffer_data_types(false);
        assert_ne!(data_types & (1 << spa_sys::SPA_DATA_MemFd), 0);
        assert_eq!(data_types & (1 << spa_sys::SPA_DATA_DmaBuf), 0);
    }

    #[test]
    fn dmabuf_support_stays_isolated_for_explicit_enablement() {
        let data_types = buffer_data_types(true);
        assert_ne!(data_types & (1 << spa_sys::SPA_DATA_MemFd), 0);
        assert_ne!(data_types & (1 << spa_sys::SPA_DATA_DmaBuf), 0);
    }

    #[test]
    fn production_negotiation_uses_only_coherent_mapped_buffers() {
        let data_types = buffer_data_types(ADVERTISE_DMABUF);
        assert_ne!(data_types & (1 << spa_sys::SPA_DATA_MemFd), 0);
        assert_eq!(data_types & (1 << spa_sys::SPA_DATA_DmaBuf), 0);
    }
}
