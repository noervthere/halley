use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

use smithay::backend::allocator::{
    Fourcc, Modifier,
    dmabuf::{Dmabuf, DmabufFlags},
};
use smithay::reexports::wayland_server::Resource;

use crate::session::{Session, SessionDriver};

#[derive(Default)]
pub struct ScreencastState {
    buffers: HashMap<(String, u64), Dmabuf>,
}

impl ScreencastState {
    pub fn register(
        &mut self,
        request: halley_ipc::RegisterDmabufRequest,
        fds: Vec<OwnedFd>,
    ) -> Result<(), String> {
        if request.width <= 0 || request.height <= 0 || request.planes.is_empty() {
            return Err("invalid DMA-BUF dimensions or planes".to_string());
        }
        let format = Fourcc::try_from(request.format)
            .map_err(|_| format!("unsupported DMA-BUF format 0x{:08x}", request.format))?;
        let mut descriptors = fds.into_iter().map(Some).collect::<Vec<_>>();
        let mut builder = Dmabuf::builder(
            (request.width, request.height),
            format,
            Modifier::from(request.modifier),
            DmabufFlags::from_bits_retain(request.flags),
        );
        for plane in request.planes {
            let fd = descriptors
                .get_mut(plane.fd_index as usize)
                .and_then(Option::take)
                .ok_or_else(|| format!("missing DMA-BUF descriptor {}", plane.fd_index))?;
            if !builder.add_plane(fd, plane.plane_index, plane.offset, plane.stride) {
                return Err("too many DMA-BUF planes".to_string());
            }
        }
        let dmabuf = builder
            .build()
            .ok_or_else(|| "could not build DMA-BUF".to_string())?;
        self.buffers
            .insert((request.stream_handle, request.buffer_id), dmabuf);
        Ok(())
    }

    pub fn remove(&mut self, stream_handle: &str, buffer_id: u64) {
        self.buffers.remove(&(stream_handle.to_string(), buffer_id));
    }
}

pub fn capture_frame<D: SessionDriver>(
    session: &mut Session<D>,
    request: halley_ipc::CaptureFrameRequest,
    fds: Vec<OwnedFd>,
) -> Result<halley_ipc::CaptureFrameResponse, String> {
    let embedded = request.cursor_mode == halley_ipc::CursorMode::Embedded;
    let width = request.source.width();
    let height = request.source.height();
    let expected = frame_len(width, height)?;

    match request.buffer {
        halley_ipc::CaptureBuffer::MemFd {
            fd_index,
            offset,
            size,
            stride,
        } => {
            if stride != width as u32 * 4 {
                return Err(format!(
                    "unsupported MemFd stride {stride}, expected {}",
                    width * 4
                ));
            }
            if size < expected as u64 {
                return Err(format!(
                    "MemFd buffer has {size} bytes, expected at least {expected}"
                ));
            }
            let fd = fds
                .get(fd_index as usize)
                .ok_or_else(|| format!("missing MemFd descriptor {fd_index}"))?;
            let mut pixels =
                crate::capture::capture_source_pixels(session, &request.source, embedded)
                    .map_err(|err| err.to_string())?;
            if pixels.len() != expected {
                return Err(format!(
                    "captured frame has {} bytes, expected {expected}",
                    pixels.len()
                ));
            }
            if embedded
                && matches!(request.source, halley_ipc::CaptureSource::Window { .. })
                && let Some(cursor) = cursor_metadata(session, &request.source)
            {
                blend_cursor_rgba(&mut pixels, width, height, &cursor);
            }
            rgba_to_bgrx(&mut pixels);
            write_all_at(fd, offset, &pixels).map_err(|err| err.to_string())?;
        }
        halley_ipc::CaptureBuffer::Dmabuf { buffer_id } => {
            if !fds.is_empty() {
                return Err("DMA-BUF frame request included descriptors".to_string());
            }
            let key = (request.stream_handle.clone(), buffer_id);
            let mut dmabuf = session
                .screencast
                .buffers
                .remove(&key)
                .ok_or_else(|| format!("unknown DMA-BUF {buffer_id}"))?;
            let result = crate::capture::render_source_dmabuf(
                session,
                &request.source,
                embedded,
                &mut dmabuf,
            )
            .map_err(|err| err.to_string());
            session.screencast.buffers.insert(key, dmabuf);
            result?;
        }
    }

    let cursor = (request.cursor_mode == halley_ipc::CursorMode::Metadata)
        .then(|| cursor_metadata(session, &request.source))
        .flatten();
    Ok(halley_ipc::CaptureFrameResponse { cursor })
}

fn rgba_to_bgrx(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
}

fn frame_len(width: i32, height: i32) -> Result<usize, String> {
    if width <= 0 || height <= 0 {
        return Err(format!("invalid capture dimensions {width}x{height}"));
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "capture dimensions overflow".to_string())
}

fn write_all_at(fd: &OwnedFd, offset: u64, bytes: &[u8]) -> io::Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        let position = offset
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("MemFd offset overflow"))?;
        let count = unsafe {
            libc::pwrite(
                fd.as_raw_fd(),
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
                position
                    .try_into()
                    .map_err(|_| io::Error::other("MemFd offset is too large"))?,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "could not write screencast frame",
            ));
        }
        written += count as usize;
    }
    Ok(())
}

fn cursor_metadata<D: SessionDriver>(
    session: &Session<D>,
    source: &halley_ipc::CaptureSource,
) -> Option<halley_ipc::CursorMetadata> {
    let (x, y) = cursor_position(session, source)?;
    Some(halley_ipc::CursorMetadata {
        x,
        y,
        hotspot_x: session.cursor.hotspot_x,
        hotspot_y: session.cursor.hotspot_y,
        width: session.cursor.width,
        height: session.cursor.height,
        bgra: session.cursor.metadata_bgra.clone(),
    })
}

fn cursor_position<D: SessionDriver>(
    session: &Session<D>,
    source: &halley_ipc::CaptureSource,
) -> Option<(i32, i32)> {
    match source {
        halley_ipc::CaptureSource::Monitor {
            name,
            x,
            y,
            width,
            height,
        } => {
            let position = session.pointer.position();
            let local = (position.0.round() as i32 - x, position.1.round() as i32 - y);
            (session
                .wayland
                .space
                .outputs()
                .any(|output| output.name() == *name)
                && local.0 >= 0
                && local.1 >= 0
                && local.0 < *width
                && local.1 < *height)
                .then_some(local)
        }
        halley_ipc::CaptureSource::Window { surface_id, .. } => {
            let route = crate::input::pointer::route_to_client(
                &session.wayland.space,
                &session.cameras,
                session.driver.primary_output(),
                &session.fullscreen,
                session.wayland.focused_window.as_ref(),
                crate::frame_clock::monotonic_now(),
                session.pointer.position(),
            )?;
            let crate::input::pointer::PointerTarget::Window(window) = route.target else {
                return None;
            };
            let toplevel = window.toplevel()?;
            if toplevel.wl_surface().id().protocol_id() != *surface_id {
                return None;
            }
            let location = session.wayland.space.element_location(&window)?;
            Some((
                (route.location.x - f64::from(location.x)).round() as i32,
                (route.location.y - f64::from(location.y)).round() as i32,
            ))
        }
    }
}

fn blend_cursor_rgba(
    frame: &mut [u8],
    width: i32,
    height: i32,
    cursor: &halley_ipc::CursorMetadata,
) {
    let left = cursor.x - cursor.hotspot_x;
    let top = cursor.y - cursor.hotspot_y;
    for row in 0..cursor.height as i32 {
        let y = top + row;
        if y < 0 || y >= height {
            continue;
        }
        for column in 0..cursor.width as i32 {
            let x = left + column;
            if x < 0 || x >= width {
                continue;
            }
            let source = ((row as u32 * cursor.width + column as u32) * 4) as usize;
            let destination = ((y * width + x) * 4) as usize;
            let alpha = u16::from(cursor.bgra[source + 3]);
            for (frame_channel, cursor_channel) in
                [(0usize, 2usize), (1usize, 1usize), (2usize, 0usize)]
            {
                let foreground = u16::from(cursor.bgra[source + cursor_channel]);
                let background = u16::from(frame[destination + frame_channel]);
                frame[destination + frame_channel] =
                    ((foreground * alpha + background * (255 - alpha)) / 255) as u8;
            }
            frame[destination + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_frames_are_converted_to_pipewire_bgrx() {
        let mut pixels = vec![10, 20, 30, 40, 50, 60, 70, 80];
        rgba_to_bgrx(&mut pixels);
        assert_eq!(pixels, vec![30, 20, 10, 255, 70, 60, 50, 255]);
    }

    #[test]
    fn cursor_blending_clips_at_stream_edges() {
        let mut frame = vec![0; 2 * 2 * 4];
        let cursor = halley_ipc::CursorMetadata {
            x: 0,
            y: 0,
            hotspot_x: 1,
            hotspot_y: 1,
            width: 2,
            height: 2,
            bgra: [0, 0, 255, 255].repeat(4),
        };
        blend_cursor_rgba(&mut frame, 2, 2, &cursor);
        assert_eq!(&frame[0..4], &[255, 0, 0, 255]);
        assert!(frame[4..].iter().all(|byte| *byte == 0));
    }
}
