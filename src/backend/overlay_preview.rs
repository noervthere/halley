use std::collections::{HashMap, HashSet};
use std::error::Error;

use halley_core::field::NodeId;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::desktop::Window;
use smithay::utils::{Physical, Rectangle};

use super::window_texture::WindowTexture;

struct Entry {
    id: Id,
    texture: WindowTexture,
}

/// GPU-local overview previews shared by Apogee and the focus cycle.
///
/// Captures render directly into GLES textures, so DMA-BUF-backed client
/// buffers remain on the GPU. Commits only mark an entry dirty; the texture is
/// refreshed when an overlay actually presents it.
#[derive(Default)]
pub struct OverlayPreviewCache {
    entries: HashMap<NodeId, Entry>,
    dirty: HashSet<NodeId>,
}

impl OverlayPreviewCache {
    pub fn mark_dirty(&mut self, id: NodeId) {
        self.dirty.insert(id);
    }

    pub fn remove(&mut self, id: NodeId) {
        self.entries.remove(&id);
        self.dirty.remove(&id);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.dirty.clear();
    }

    pub fn retain(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        let ids = ids.into_iter().collect::<HashSet<_>>();
        self.entries.retain(|id, _| ids.contains(id));
        self.dirty.retain(|id| ids.contains(id));
    }

    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        id: NodeId,
        window: &Window,
        destination: Rectangle<i32, Physical>,
        alpha: f32,
        live: bool,
    ) -> Result<TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>, Box<dyn Error>>
    {
        self.element_with_texture(renderer, id, window, destination, alpha, live)
            .map(|(element, _)| element)
    }

    pub fn element_with_texture(
        &mut self,
        renderer: &mut GlesRenderer,
        id: NodeId,
        window: &Window,
        destination: Rectangle<i32, Physical>,
        alpha: f32,
        live: bool,
    ) -> Result<(TextureRenderElement<GlesTexture>, GlesTexture), Box<dyn Error>> {
        let refresh = !self.entries.contains_key(&id) || live && self.dirty.remove(&id);
        if refresh {
            let previous = self.entries.remove(&id);
            let element_id = previous
                .as_ref()
                .map(|entry| entry.id.clone())
                .unwrap_or_else(Id::new);
            let reusable = previous.map(|entry| entry.texture.texture);
            match super::window_texture::capture(renderer, window, reusable) {
                Ok(texture) => {
                    self.dirty.remove(&id);
                    self.entries.insert(
                        id,
                        Entry {
                            id: element_id,
                            texture,
                        },
                    );
                }
                Err(err) => {
                    self.dirty.insert(id);
                    return Err(err);
                }
            }
        }
        let entry = self
            .entries
            .get(&id)
            .ok_or("overview preview capture did not produce a texture")?;
        Ok((
            entry
                .texture
                .render_element(entry.id.clone(), destination, alpha),
            entry.texture.texture.clone(),
        ))
    }
}
