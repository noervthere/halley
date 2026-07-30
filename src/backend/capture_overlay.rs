use std::collections::HashMap;
use std::error::Error;
use std::io;
use std::sync::{Mutex, OnceLock};

use resvg::{tiny_skia, usvg};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::{Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Logical, Physical, Rectangle, Transform};

const ICON_RASTER_SIZE: u32 = 48;
const ICON_DISPLAY_SIZE: i32 = 42;
const REGION_SVG: &[u8] = include_bytes!("../../assets/screenshot/region.svg");
const SCREEN_SVG: &[u8] = include_bytes!("../../assets/screenshot/screen.svg");
const WINDOW_SVG: &[u8] = include_bytes!("../../assets/screenshot/window.svg");

type IconSet = Result<[MemoryRenderBuffer; 3], String>;
type IconCache = Mutex<HashMap<[u8; 3], IconSet>>;

static ICONS: OnceLock<IconCache> = OnceLock::new();

render_elements! {
    pub CaptureOverlayElement<=GlesRenderer>;
    Icon=MemoryRenderBufferRenderElement<GlesRenderer>,
    Card=super::node::LabelRenderElement,
}

pub fn menu_elements(
    renderer: &mut GlesRenderer,
    node_renderer: &mut super::node::NodeRenderer,
    output: Rectangle<i32, Logical>,
    selected: usize,
    hovered: Option<usize>,
    window_available: bool,
    visuals: super::overlay::OverlayVisuals,
) -> Result<Vec<CaptureOverlayElement>, Box<dyn Error>> {
    let layout = crate::capture::menu::layout(output);
    let localize = |rectangle: Rectangle<i32, Logical>| {
        Rectangle::<i32, Physical>::new(
            (rectangle.loc - output.loc).to_physical(1),
            rectangle.size.to_physical(1),
        )
    };
    let mut elements = Vec::new();
    let bar = localize(layout.bar);
    elements.push(CaptureOverlayElement::Card(super::overlay::card_element(
        renderer,
        node_renderer,
        bar,
        visuals,
        visuals.fill,
        0.96,
    )?));

    for (index, item) in layout.items.into_iter().enumerate() {
        let disabled = index == 2 && !window_available;
        let active = !disabled && (selected == index || hovered == Some(index));
        let item = localize(item);
        let fill = if disabled {
            visuals.key_fill.mix(visuals.fill, 0.55)
        } else if active {
            visuals.fill.mix(visuals.border, 0.12)
        } else {
            visuals.key_fill
        };
        let accent = if disabled {
            visuals.subtext.mix(visuals.fill, 0.45)
        } else if active {
            visuals.border
        } else {
            visuals.subtext
        };
        let mut item_visuals = visuals;
        item_visuals.border = accent;
        item_visuals.border_px = if visuals.border_px > 0.0 { 2.0 } else { 0.0 };
        elements.push(CaptureOverlayElement::Card(super::overlay::card_element(
            renderer,
            node_renderer,
            item,
            item_visuals,
            fill,
            if disabled {
                0.50
            } else if active {
                0.98
            } else {
                0.94
            },
        )?));
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
        let icon_rgb = if active {
            visuals.text.bytes()
        } else {
            visuals.subtext.bytes()
        };
        let icons = icon_buffers(icon_rgb)?;
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

fn icon_buffers(rgb: [u8; 3]) -> Result<[MemoryRenderBuffer; 3], Box<dyn Error>> {
    let cache = ICONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().expect("screenshot icon cache poisoned");
    let result = cache.entry(rgb).or_insert_with(|| {
        let region = rasterize_svg(REGION_SVG, rgb)
            .ok_or_else(|| "could not rasterize Region screenshot icon".to_string())?;
        let screen = rasterize_svg(SCREEN_SVG, rgb)
            .ok_or_else(|| "could not rasterize Screen screenshot icon".to_string())?;
        let window = rasterize_svg(WINDOW_SVG, rgb)
            .ok_or_else(|| "could not rasterize Window screenshot icon".to_string())?;
        Ok([
            memory_buffer(&region),
            memory_buffer(&screen),
            memory_buffer(&window),
        ])
    });
    match result {
        Ok(icons) => Ok(icons.clone()),
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

fn rasterize_svg(svg: &[u8], rgb: [u8; 3]) -> Option<Vec<u8>> {
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
        pixel[0] = ((u16::from(rgb[0]) * u16::from(alpha)) / 255) as u8;
        pixel[1] = ((u16::from(rgb[1]) * u16::from(alpha)) / 255) as u8;
        pixel[2] = ((u16::from(rgb[2]) * u16::from(alpha)) / 255) as u8;
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_original_screenshot_icons_rasterize() {
        for svg in [REGION_SVG, SCREEN_SVG, WINDOW_SVG] {
            let pixels = rasterize_svg(svg, [32, 64, 96]).expect("bundled SVG should rasterize");
            assert_eq!(
                pixels.len(),
                ICON_RASTER_SIZE as usize * ICON_RASTER_SIZE as usize * 4
            );
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
    }
}
