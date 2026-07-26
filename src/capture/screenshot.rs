use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer};
use smithay::output::Output;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Transform};

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
    let directory = expand_directory(&session.screenshot.directory);
    fs::create_dir_all(&directory)?;
    let (path, file) = create_unique_file(&directory)?;
    if let Err(err) = write_png(file, region.size.w as u32, region.size.h as u32, &pixels) {
        let _ = fs::remove_file(&path);
        return Err(err.into());
    }
    Ok(path)
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
    let buffer_size = smithay::utils::Size::<i32, Buffer>::from((size.w, size.h));
    let mut texture = <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
        renderer,
        Fourcc::Abgr8888,
        buffer_size,
    )?;
    let damage = Rectangle::<i32, Physical>::from_size(size);
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
        frame.clear(backend::CLEAR_COLOR, &[damage])?;
        draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
        let _ = frame.finish()?;
    }

    let region = Rectangle::<i32, Buffer>::from_size(buffer_size);
    let mapping = renderer.copy_texture(&texture, region, Fourcc::Abgr8888)?;
    Ok(renderer.map_texture(&mapping)?.to_vec())
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
