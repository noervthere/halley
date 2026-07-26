use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::backend::{self, RenderRequest};
use crate::session::{Session, SessionDriver};

struct OutputImage {
    geometry: Rectangle<i32, Logical>,
    pixels: Vec<u8>,
}

pub fn save_region<D: SessionDriver>(
    session: &mut Session<D>,
    region: Rectangle<i32, Logical>,
) -> Result<PathBuf, Box<dyn Error>> {
    if region.size.w <= 0 || region.size.h <= 0 {
        return Err(io::Error::other("selected screenshot region is empty").into());
    }
    let outputs = session
        .wayland
        .space
        .outputs()
        .filter_map(|output| {
            Some((
                output.clone(),
                session.wayland.space.output_geometry(output)?,
            ))
        })
        .collect::<Vec<_>>();
    let primary = session.driver.primary_output().clone();
    let target_time = crate::frame_clock::monotonic_now();

    let driver = &mut session.driver;
    let cursor = &session.cursor;
    let pointer_position = session.pointer.position();
    let wayland = &session.wayland;
    let decorations = &session.decorations;
    let cameras = &session.cameras;
    let window_open_animations = &session.window_open_animations;
    let fullscreen = &session.fullscreen;
    let fullscreen_textures = &mut session.fullscreen_textures;
    let images = driver.with_renderer(|renderer| -> Result<Vec<OutputImage>, Box<dyn Error>> {
        outputs
            .iter()
            .map(|(output, geometry)| {
                let pixels = capture_output(
                    renderer,
                    output,
                    &primary,
                    *geometry,
                    RenderRequest {
                        target_presentation_time: target_time,
                        clear: backend::CLEAR_COLOR,
                        cursor,
                        cursor_position: pointer_position,
                        show_cursor: false,
                        capture_region: None,
                        space: &wayland.space,
                        focused: wayland.focused_window.as_ref(),
                        decorations,
                        cameras,
                        window_open_animations,
                        fullscreen,
                        fullscreen_textures: &mut *fullscreen_textures,
                    },
                )?;
                Ok(OutputImage {
                    geometry: *geometry,
                    pixels,
                })
            })
            .collect()
    })?;

    let pixels = composite_region(region, &images)?;
    save_pixels(
        &session.screenshot.directory,
        region.size.w as u32,
        region.size.h as u32,
        &pixels,
    )
}

pub fn save_window<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> Result<PathBuf, Box<dyn Error>> {
    let window = session
        .wayland
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface)
        })
        .ok_or_else(|| io::Error::other("selected window is not mapped"))?;
    let toplevel = window
        .toplevel()
        .ok_or_else(|| io::Error::other("selected window has no toplevel"))?;
    let geometry = window.geometry();
    if geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Err(io::Error::other("selected window has empty geometry").into());
    }
    let surface = toplevel.wl_surface().clone();
    let pixels = session
        .driver
        .with_renderer(|renderer| capture_surface_tree(renderer, &surface, geometry))?;
    save_pixels(
        &session.screenshot.directory,
        geometry.size.w as u32,
        geometry.size.h as u32,
        &pixels,
    )
}

pub(crate) fn capture_source_pixels<D: SessionDriver>(
    session: &mut Session<D>,
    source: &halley_ipc::CaptureSource,
    show_cursor: bool,
) -> Result<Vec<u8>, Box<dyn Error>> {
    match source {
        halley_ipc::CaptureSource::Monitor {
            name,
            width,
            height,
            ..
        } => with_monitor_scene(
            session,
            name,
            *width,
            *height,
            show_cursor,
            |renderer, elements, size| {
                capture_elements(
                    renderer,
                    Fourcc::Abgr8888,
                    size,
                    elements,
                    backend::CLEAR_COLOR,
                )
            },
        ),
        halley_ipc::CaptureSource::Window {
            surface_id,
            width,
            height,
        } => {
            let (surface, geometry) =
                resolve_source_window(&session.wayland.space, *surface_id, *width, *height)?;
            session
                .driver
                .with_renderer(|renderer| capture_surface_tree(renderer, &surface, geometry))
        }
    }
}

pub(crate) fn render_source_dmabuf<D: SessionDriver>(
    session: &mut Session<D>,
    source: &halley_ipc::CaptureSource,
    show_cursor: bool,
    dmabuf: &mut Dmabuf,
) -> Result<(), Box<dyn Error>> {
    session.driver.import_dmabuf(dmabuf);
    match source {
        halley_ipc::CaptureSource::Monitor {
            name,
            width,
            height,
            ..
        } => with_monitor_scene(
            session,
            name,
            *width,
            *height,
            show_cursor,
            |renderer, elements, size| {
                render_elements_to_dmabuf(renderer, dmabuf, size, elements, backend::CLEAR_COLOR)
            },
        ),
        halley_ipc::CaptureSource::Window {
            surface_id,
            width,
            height,
        } => {
            let (surface, geometry) =
                resolve_source_window(&session.wayland.space, *surface_id, *width, *height)?;
            session.driver.with_renderer(|renderer| {
                let elements = surface_tree_elements(renderer, &surface, geometry)?;
                render_elements_to_dmabuf(
                    renderer,
                    dmabuf,
                    geometry.size.to_physical(1),
                    &elements,
                    Color32F::TRANSPARENT,
                )
            })
        }
    }
}

fn with_monitor_scene<D, T>(
    session: &mut Session<D>,
    name: &str,
    width: i32,
    height: i32,
    show_cursor: bool,
    consume: impl FnOnce(
        &mut GlesRenderer,
        &[backend::scene::SceneElement],
        smithay::utils::Size<i32, Physical>,
    ) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>>
where
    D: SessionDriver,
{
    let output = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == name)
        .cloned()
        .ok_or_else(|| io::Error::other(format!("unknown output {name}")))?;
    let geometry = session
        .wayland
        .space
        .output_geometry(&output)
        .ok_or_else(|| io::Error::other(format!("output {name} has no geometry")))?;
    if geometry.size != (width, height).into() {
        return Err(io::Error::other("selected output size changed").into());
    }
    let primary = session.driver.primary_output().clone();
    let target_time = crate::frame_clock::monotonic_now();
    let driver = &mut session.driver;
    let cursor = &session.cursor;
    let pointer_position = session.pointer.position();
    let wayland = &session.wayland;
    let decorations = &session.decorations;
    let cameras = &session.cameras;
    let window_open_animations = &session.window_open_animations;
    let fullscreen = &session.fullscreen;
    let fullscreen_textures = &mut session.fullscreen_textures;
    driver.with_renderer(|renderer| {
        let elements = backend::scene::build(
            renderer,
            &output,
            &primary,
            geometry,
            RenderRequest {
                target_presentation_time: target_time,
                clear: backend::CLEAR_COLOR,
                cursor,
                cursor_position: pointer_position,
                show_cursor,
                capture_region: None,
                space: &wayland.space,
                focused: wayland.focused_window.as_ref(),
                decorations,
                cameras,
                window_open_animations,
                fullscreen,
                fullscreen_textures,
            },
        )?;
        consume(renderer, &elements, geometry.size.to_physical(1))
    })
}

fn resolve_source_window(
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    surface_id: u32,
    width: i32,
    height: i32,
) -> Result<
    (
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        Rectangle<i32, Logical>,
    ),
    Box<dyn Error>,
> {
    let window = space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface().id().protocol_id() == surface_id)
        })
        .ok_or_else(|| io::Error::other("selected window is no longer mapped"))?;
    let geometry = window.geometry();
    if geometry.size != (width, height).into() {
        return Err(io::Error::other("selected window size changed").into());
    }
    Ok((
        window
            .toplevel()
            .expect("matched a toplevel above")
            .wl_surface()
            .clone(),
        geometry,
    ))
}

fn render_elements_to_dmabuf<E>(
    renderer: &mut GlesRenderer,
    dmabuf: &mut Dmabuf,
    size: smithay::utils::Size<i32, Physical>,
    elements: &[E],
    clear: Color32F,
) -> Result<(), Box<dyn Error>>
where
    E: smithay::backend::renderer::element::RenderElement<GlesRenderer>,
{
    let damage = Rectangle::<i32, Physical>::from_size(size);
    let mut target = renderer.bind(dmabuf)?;
    let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
    frame.clear(clear, &[damage])?;
    draw_render_elements(&mut frame, 1.0, elements, &[damage])?;
    let _ = frame.finish()?;
    Ok(())
}

fn save_pixels(
    configured_directory: &str,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<PathBuf, Box<dyn Error>> {
    let directory = expand_directory(configured_directory);
    fs::create_dir_all(&directory)?;
    let (path, file) = create_unique_file(&directory)?;
    if let Err(err) = write_png(file, width, height, pixels) {
        let _ = fs::remove_file(&path);
        return Err(err.into());
    }
    Ok(path)
}

fn capture_surface_tree(
    renderer: &mut GlesRenderer,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    geometry: Rectangle<i32, Logical>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let size = geometry.size.to_physical(1);
    let elements = surface_tree_elements(renderer, surface, geometry)?;
    capture_elements(
        renderer,
        Fourcc::Abgr8888,
        size,
        &elements,
        Color32F::TRANSPARENT,
    )
}

fn surface_tree_elements(
    renderer: &mut GlesRenderer,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    geometry: Rectangle<i32, Logical>,
) -> Result<Vec<WaylandSurfaceRenderElement<GlesRenderer>>, Box<dyn Error>> {
    let location = smithay::utils::Point::from((-geometry.loc.x, -geometry.loc.y)).to_physical(1);
    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            renderer,
            surface,
            location,
            Scale::from(1.0),
            1.0,
            Kind::Unspecified,
        );
    if elements.is_empty() {
        return Err(io::Error::other("selected window surface tree is empty").into());
    }
    Ok(elements)
}

fn capture_elements<E>(
    renderer: &mut GlesRenderer,
    format: Fourcc,
    size: smithay::utils::Size<i32, Physical>,
    elements: &[E],
    clear: Color32F,
) -> Result<Vec<u8>, Box<dyn Error>>
where
    E: smithay::backend::renderer::element::RenderElement<GlesRenderer>,
{
    let buffer_size = smithay::utils::Size::<i32, Buffer>::from((size.w, size.h));
    let mut texture =
        <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(renderer, format, buffer_size)?;
    let damage = Rectangle::<i32, Physical>::from_size(size);
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
        frame.clear(clear, &[damage])?;
        draw_render_elements(&mut frame, 1.0, elements, &[damage])?;
        let _ = frame.finish()?;
    }
    let mapping = renderer.copy_texture(
        &texture,
        Rectangle::<i32, Buffer>::from_size(buffer_size),
        format,
    )?;
    Ok(renderer.map_texture(&mapping)?.to_vec())
}

fn capture_output(
    renderer: &mut GlesRenderer,
    output: &Output,
    primary: &Output,
    geometry: Rectangle<i32, Logical>,
    request: RenderRequest<'_>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let elements = backend::scene::build(renderer, output, primary, geometry, request)?;
    let size = geometry.size.to_physical(1);
    capture_elements(
        renderer,
        Fourcc::Abgr8888,
        size,
        &elements,
        backend::CLEAR_COLOR,
    )
}

fn composite_region(
    region: Rectangle<i32, Logical>,
    outputs: &[OutputImage],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let width = region.size.w as usize;
    let height = region.size.h as usize;
    let mut composite = vec![0u8; width * height * 4];

    for output in outputs {
        let expected = output.geometry.size.w as usize * output.geometry.size.h as usize * 4;
        if output.pixels.len() != expected {
            return Err(io::Error::other("renderer returned an unexpected pixel count").into());
        }
        let Some(intersection) = region.intersection(output.geometry) else {
            continue;
        };
        let copy_width = intersection.size.w as usize;
        for row in 0..intersection.size.h as usize {
            let source_x = (intersection.loc.x - output.geometry.loc.x) as usize;
            let source_y = (intersection.loc.y - output.geometry.loc.y) as usize + row;
            let destination_x = (intersection.loc.x - region.loc.x) as usize;
            let destination_y = (intersection.loc.y - region.loc.y) as usize + row;
            let source_start = (source_y * output.geometry.size.w as usize + source_x) * 4;
            let destination_start = (destination_y * width + destination_x) * 4;
            let byte_count = copy_width * 4;
            composite[destination_start..destination_start + byte_count]
                .copy_from_slice(&output.pixels[source_start..source_start + byte_count]);
        }
    }
    Ok(composite)
}

fn expand_directory(raw: &str) -> PathBuf {
    for prefix in ["$env.HOME/", "$HOME/", "~/"] {
        if let Some(rest) = raw.strip_prefix(prefix)
            && let Some(home) = std::env::var_os("HOME")
        {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

fn create_unique_file(directory: &Path) -> io::Result<(PathBuf, File)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    for suffix in 0..1000u16 {
        let path = directory.join(format!(
            "halley-screenshot-{}-{:03}-{suffix}.png",
            now.as_secs(),
            now.subsec_millis()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique screenshot filename",
    ))
}

fn write_png(file: File, width: u32, height: u32, pixels: &[u8]) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(geometry: Rectangle<i32, Logical>, color: [u8; 4]) -> OutputImage {
        let pixel_count = geometry.size.w as usize * geometry.size.h as usize;
        OutputImage {
            geometry,
            pixels: color.repeat(pixel_count),
        }
    }

    #[test]
    fn desktop_composite_keeps_output_positions_and_transparent_gaps() {
        let outputs = [
            image(Rectangle::new((0, 0).into(), (2, 2).into()), [1, 2, 3, 255]),
            image(Rectangle::new((3, 0).into(), (1, 2).into()), [4, 5, 6, 255]),
        ];
        let pixels =
            composite_region(Rectangle::new((0, 0).into(), (4, 2).into()), &outputs).unwrap();

        assert_eq!(&pixels[0..4], &[1, 2, 3, 255]);
        assert_eq!(&pixels[8..12], &[0, 0, 0, 0]);
        assert_eq!(&pixels[12..16], &[4, 5, 6, 255]);
    }

    #[test]
    fn crop_can_cross_output_boundaries() {
        let outputs = [
            image(Rectangle::new((0, 0).into(), (2, 1).into()), [1, 0, 0, 255]),
            image(Rectangle::new((2, 0).into(), (2, 1).into()), [2, 0, 0, 255]),
        ];
        let pixels =
            composite_region(Rectangle::new((1, 0).into(), (2, 1).into()), &outputs).unwrap();
        assert_eq!(pixels, vec![1, 0, 0, 255, 2, 0, 0, 255]);
    }

    #[test]
    fn home_expansion_accepts_config_and_shell_spellings() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        for raw in ["$env.HOME/Pictures", "$HOME/Pictures", "~/Pictures"] {
            assert_eq!(expand_directory(raw), PathBuf::from(&home).join("Pictures"));
        }
    }
}
