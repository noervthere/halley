use std::collections::HashMap;
use std::error::Error;

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle};
use smithay::wayland::seat::WaylandFocus;

/// Outgoing client frames held while Field arrangement changes client size.
///
/// Clients may acknowledge a resize midway through the compositor timeline.
/// Rendering their new buffer immediately changes the texture's native pixel
/// basis and makes its contents jump in scale. Holding the pre-configure frame
/// until the timeline settles keeps the whole client image on one continuous
/// transform; the live target buffer replaces it only at the final rectangle.
#[derive(Default)]
pub struct ArrangeTextureTransitions {
    windows: HashMap<WlSurface, ArrangeTexture>,
}

struct ArrangeTexture {
    id: Id,
    texture: super::window_texture::WindowTexture,
}

impl ArrangeTextureTransitions {
    pub fn capture(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        preserve_existing: bool,
    ) -> Result<(), Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("arrange snapshot window has no surface")?
            .into_owned();
        if should_preserve_existing(self.windows.contains_key(&surface), preserve_existing) {
            return Ok(());
        }
        let texture = super::window_texture::capture(renderer, window, None)?;
        self.windows.insert(
            surface,
            ArrangeTexture {
                id: Id::new(),
                texture,
            },
        );
        Ok(())
    }

    pub fn element(
        &self,
        surface: &WlSurface,
        destination: Rectangle<i32, Physical>,
        alpha: f32,
    ) -> Option<(TextureRenderElement<GlesTexture>, GlesTexture)> {
        let entry = self.windows.get(surface)?;
        Some((
            entry
                .texture
                .render_element(entry.id.clone(), destination, alpha),
            entry.texture.texture().clone(),
        ))
    }

    pub fn retain_surfaces(&mut self, mut keep: impl FnMut(&WlSurface) -> bool) {
        self.windows.retain(|surface, _| keep(surface));
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
    }
}

fn should_preserve_existing(has_snapshot: bool, preserve_existing: bool) -> bool {
    has_snapshot && preserve_existing
}

#[cfg(test)]
mod tests {
    use super::should_preserve_existing;

    #[test]
    fn reversal_keeps_the_original_outgoing_frame_only_while_motion_is_live() {
        assert!(should_preserve_existing(true, true));
        assert!(!should_preserve_existing(true, false));
        assert!(!should_preserve_existing(false, true));
    }
}
