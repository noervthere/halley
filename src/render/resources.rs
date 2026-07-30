use halley_config::{Animations, Font};

use super::close::WindowCloseAnimations;
use super::effects::backdrop_blur::BackdropBlurRenderer;
use super::effects::shadow::ShadowRenderer;
use super::fullscreen_texture::FullscreenTextureTransitions;
use super::node::NodeRenderer;
use super::overlays::preview::OverlayPreviewCache;
use super::text::UiTextRenderer;
use super::window_decoration::WindowDecorationRenderer;

/// Renderer-owned caches and GPU resources shared by every output.
///
/// Keeping these behind one session field makes their lifetime and reload
/// boundary explicit. Compositor policy remains in `Session`; this aggregate
/// contains only presentation resources that must survive across frames.
pub struct RenderState {
    pub(crate) window_close_animations: WindowCloseAnimations,
    pub(crate) fullscreen_textures: FullscreenTextureTransitions,
    pub(crate) overlay_previews: OverlayPreviewCache,
    pub(crate) node_renderer: NodeRenderer,
    pub(crate) window_decoration_renderer: WindowDecorationRenderer,
    pub(crate) backdrop_blur_renderer: BackdropBlurRenderer,
    pub(crate) shadow_renderer: ShadowRenderer,
    pub(crate) ui_text: UiTextRenderer,
}

impl RenderState {
    pub fn new(animations: Animations, font: &Font) -> Self {
        Self {
            window_close_animations: WindowCloseAnimations::new(animations),
            fullscreen_textures: FullscreenTextureTransitions::default(),
            overlay_previews: OverlayPreviewCache::default(),
            node_renderer: NodeRenderer::default(),
            window_decoration_renderer: WindowDecorationRenderer::default(),
            backdrop_blur_renderer: BackdropBlurRenderer::default(),
            shadow_renderer: ShadowRenderer::default(),
            ui_text: UiTextRenderer::new(font),
        }
    }
}
