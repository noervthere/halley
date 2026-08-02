use std::collections::HashMap;
use std::error::Error;

use resvg::{tiny_skia, usvg};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ContextId, ImportMem, Renderer};
use smithay::utils::{Physical, Rectangle};

use super::window_decoration::{CornerRadii, RoundedTextureElement, WindowDecorationRenderer};
use super::window_texture::WindowTexture;
use crate::titlebar::Control;

const RASTER_SIZE: u32 = 64;
const CLOSE: &[u8] = include_bytes!("../../assets/titlebars/close.svg");
const MINIMIZE: &[u8] = include_bytes!("../../assets/titlebars/minimize.svg");
const MAXIMIZE: &[u8] = include_bytes!("../../assets/titlebars/maximize.svg");

#[derive(Default)]
pub struct TitlebarRenderer {
    context: Option<ContextId<GlesTexture>>,
    icons: HashMap<Control, WindowTexture>,
    failed: bool,
}

impl TitlebarRenderer {
    pub fn control_element(
        &mut self,
        renderer: &mut GlesRenderer,
        decorations: &mut WindowDecorationRenderer,
        control: Control,
        destination: Rectangle<i32, Physical>,
        color: halley_config::BorderColor,
        alpha: f32,
    ) -> Option<RoundedTextureElement> {
        if destination.size.w <= 0 || destination.size.h <= 0 || alpha <= 0.0 {
            return None;
        }
        if let Err(err) = self.ensure(renderer) {
            if !self.failed {
                eventline::warn!("titlebars: failed to prepare bundled button masks: {err}");
                self.failed = true;
            }
            return None;
        }
        let icon = self.icons.get(&control)?;
        let texture = icon.texture.clone();
        let base = icon.render_element(Id::new(), destination, alpha);
        decorations.texture_element_with_radii(
            renderer,
            base,
            texture,
            destination,
            CornerRadii::default(),
            (color.r, color.g, color.b, 1.0),
        )
    }

    fn ensure(&mut self, renderer: &mut GlesRenderer) -> Result<(), Box<dyn Error>> {
        let context = renderer.context_id();
        if self.context.as_ref() == Some(&context) && self.icons.len() == 3 {
            return Ok(());
        }
        self.context = Some(context.clone());
        self.icons.clear();
        self.failed = false;
        for (control, source) in [
            (Control::Close, CLOSE),
            (Control::Minimize, MINIMIZE),
            (Control::Maximize, MAXIMIZE),
        ] {
            let pixels = raster_mask(source).ok_or("bundled SVG did not produce an alpha mask")?;
            let texture = renderer.import_memory(
                &pixels,
                Fourcc::Abgr8888,
                (RASTER_SIZE as i32, RASTER_SIZE as i32).into(),
                false,
            )?;
            self.icons.insert(
                control,
                WindowTexture {
                    texture,
                    context: context.clone(),
                },
            );
        }
        Ok(())
    }
}

fn raster_mask(source: &[u8]) -> Option<Vec<u8>> {
    let tree = usvg::Tree::from_data(source, &usvg::Options::default()).ok()?;
    let size = tree.size().to_int_size();
    if size.width() == 0 || size.height() == 0 {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(RASTER_SIZE, RASTER_SIZE)?;
    let scale =
        (RASTER_SIZE as f32 / size.width() as f32).min(RASTER_SIZE as f32 / size.height() as f32);
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(
        (RASTER_SIZE as f32 - size.width() as f32 * scale) / 2.0,
        (RASTER_SIZE as f32 - size.height() as f32 * scale) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut pixels = pixmap.data().to_vec();
    let mut visible = false;
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3];
        visible |= alpha != 0;
        pixel[0] = alpha;
        pixel[1] = alpha;
        pixel[2] = alpha;
    }
    visible.then_some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_icons_are_nonempty_alpha_masks() {
        for source in [CLOSE, MINIMIZE, MAXIMIZE] {
            let pixels = raster_mask(source).expect("valid bundled SVG");
            assert_eq!(
                pixels.len(),
                RASTER_SIZE as usize * RASTER_SIZE as usize * 4
            );
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
            assert!(
                pixels.chunks_exact(4).all(|pixel| pixel[0] == pixel[3]
                    && pixel[1] == pixel[3]
                    && pixel[2] == pixel[3])
            );
        }
    }
}
