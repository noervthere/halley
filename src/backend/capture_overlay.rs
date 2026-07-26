use std::error::Error;
use std::io;
use std::sync::OnceLock;

use resvg::{tiny_skia, usvg};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Id, Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Logical, Physical, Rectangle, Transform};

const ICON_RASTER_SIZE: u32 = 48;
const ICON_DISPLAY_SIZE: i32 = 42;
const REGION_SVG: &[u8] = include_bytes!("../../assets/screenshot/region.svg");
const SCREEN_SVG: &[u8] = include_bytes!("../../assets/screenshot/screen.svg");
const WINDOW_SVG: &[u8] = include_bytes!("../../assets/screenshot/window.svg");

static ICONS: OnceLock<Result<[MemoryRenderBuffer; 3], String>> = OnceLock::new();

render_elements! {
    pub CaptureOverlayElement<=GlesRenderer>;
    Icon=MemoryRenderBufferRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
}

pub fn menu_elements(
    renderer: &mut GlesRenderer,
    output: Rectangle<i32, Logical>,
    selected: usize,
    hovered: Option<usize>,
    window_available: bool,
    highlight: Color32F,
) -> Result<Vec<CaptureOverlayElement>, Box<dyn Error>> {
    let layout = crate::capture::menu::layout(output);
    let localize = |rectangle: Rectangle<i32, Logical>| {
        Rectangle::<i32, Physical>::new(
            (rectangle.loc - output.loc).to_physical(1),
            rectangle.size.to_physical(1),
        )
    };
    let make = |geometry, color| {
        SolidColorRenderElement::new(
            Id::new(),
            geometry,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
    };
    let mut elements = Vec::new();
    let bar = localize(layout.bar);
    elements.push(CaptureOverlayElement::Solid(make(
        bar,
        Color32F::new(0.055, 0.065, 0.075, 0.96),
    )));
    elements.extend(
        super::border_strips(bar, 2, highlight)
            .into_iter()
            .map(CaptureOverlayElement::Solid),
    );

    let icons = icon_buffers()?;
    for (index, item) in layout.items.into_iter().enumerate() {
        let disabled = index == 2 && !window_available;
        let active = !disabled && (selected == index || hovered == Some(index));
        let item = localize(item);
        let fill = if disabled {
            Color32F::new(0.10, 0.11, 0.12, 0.50)
        } else if active {
            Color32F::new(0.14, 0.16, 0.18, 0.98)
        } else {
            Color32F::new(0.09, 0.105, 0.12, 0.94)
        };
        let accent = if disabled {
            Color32F::new(0.35, 0.37, 0.39, 0.42)
        } else if active {
            highlight
        } else {
            Color32F::new(0.60, 0.63, 0.66, 0.72)
        };
        elements.push(CaptureOverlayElement::Solid(make(item, fill)));
        elements.extend(
            super::border_strips(item, 2, accent)
                .into_iter()
                .map(CaptureOverlayElement::Solid),
        );
        let icon_size = ICON_DISPLAY_SIZE
            .min(item.size.w - 12)
            .min(item.size.h - 12)
            .max(12);
        let location = (
            f64::from(item.loc.x + (item.size.w - icon_size) / 2),
            f64::from(item.loc.y + (item.size.h - icon_size) / 2),
        );
        let alpha = if disabled {
            0.30
        } else if active {
            1.0
        } else {
            0.72
        };
        let icon = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location,
            &icons[index],
            Some(alpha),
            None,
            Some((icon_size, icon_size).into()),
            Kind::Unspecified,
        )?;
        elements.push(CaptureOverlayElement::Icon(icon));
    }
    Ok(elements)
}

fn icon_buffers() -> Result<&'static [MemoryRenderBuffer; 3], Box<dyn Error>> {
    match ICONS.get_or_init(|| {
        let region = rasterize_svg(REGION_SVG)
            .ok_or_else(|| "could not rasterize Region screenshot icon".to_string())?;
        let screen = rasterize_svg(SCREEN_SVG)
            .ok_or_else(|| "could not rasterize Screen screenshot icon".to_string())?;
        let window = rasterize_svg(WINDOW_SVG)
            .ok_or_else(|| "could not rasterize Window screenshot icon".to_string())?;
        Ok([
            memory_buffer(&region),
            memory_buffer(&screen),
            memory_buffer(&window),
        ])
    }) {
        Ok(icons) => Ok(icons),
        Err(message) => Err(io::Error::other(message.clone()).into()),
    }
}

fn memory_buffer(pixels: &[u8]) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        pixels,
        Fourcc::Abgr8888,
        (ICON_RASTER_SIZE as i32, ICON_RASTER_SIZE as i32),
        1,
        Transform::Normal,
        None,
    )
}

fn rasterize_svg(svg: &[u8]) -> Option<Vec<u8>> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).ok()?;
    let svg_size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(ICON_RASTER_SIZE, ICON_RASTER_SIZE)?;
    let scale_x = ICON_RASTER_SIZE as f32 / svg_size.width() as f32;
    let scale_y = ICON_RASTER_SIZE as f32 / svg_size.height() as f32;
    let scale = scale_x.min(scale_y);
    let dx = (ICON_RASTER_SIZE as f32 - svg_size.width() as f32 * scale) * 0.5;
    let dy = (ICON_RASTER_SIZE as f32 - svg_size.height() as f32 * scale) * 0.5;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut pixels = pixmap.take();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3];
        pixel[0] = alpha;
        pixel[1] = alpha;
        pixel[2] = alpha;
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_original_screenshot_icons_rasterize() {
        for svg in [REGION_SVG, SCREEN_SVG, WINDOW_SVG] {
            let pixels = rasterize_svg(svg).expect("bundled SVG should rasterize");
            assert_eq!(
                pixels.len(),
                ICON_RASTER_SIZE as usize * ICON_RASTER_SIZE as usize * 4
            );
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
    }
}
