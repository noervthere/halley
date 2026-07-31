use halley_config::{Animations, Font};

use super::background::BackgroundRenderer;
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
    pub(crate) background_renderer: BackgroundRenderer,
    pub(crate) fullscreen_textures: FullscreenTextureTransitions,
    pub(crate) overlay_previews: OverlayPreviewCache,
    pub(crate) node_renderer: NodeRenderer,
    pub(crate) window_decoration_renderer: WindowDecorationRenderer,
    pub(crate) backdrop_blur_renderer: BackdropBlurRenderer,
    pub(crate) shadow_renderer: ShadowRenderer,
    pub(crate) ui_text: UiTextRenderer,
}

/// Per-frame mutable view over [`RenderState`].
///
/// Splitting the aggregate once at the request boundary lets the scene builder
/// borrow independent caches without exposing many session-level fields.
pub struct RenderResources<'a> {
    pub window_close_animations: &'a mut WindowCloseAnimations,
    pub background_renderer: &'a mut BackgroundRenderer,
    pub fullscreen_textures: &'a mut FullscreenTextureTransitions,
    pub overlay_previews: &'a mut OverlayPreviewCache,
    pub node_renderer: &'a mut NodeRenderer,
    pub window_decoration_renderer: &'a mut WindowDecorationRenderer,
    pub backdrop_blur_renderer: &'a mut BackdropBlurRenderer,
    pub shadow_renderer: &'a mut ShadowRenderer,
    pub ui_text: &'a mut UiTextRenderer,
}

impl<'a> From<&'a mut RenderState> for RenderResources<'a> {
    fn from(state: &'a mut RenderState) -> Self {
        Self {
            window_close_animations: &mut state.window_close_animations,
            background_renderer: &mut state.background_renderer,
            fullscreen_textures: &mut state.fullscreen_textures,
            overlay_previews: &mut state.overlay_previews,
            node_renderer: &mut state.node_renderer,
            window_decoration_renderer: &mut state.window_decoration_renderer,
            backdrop_blur_renderer: &mut state.backdrop_blur_renderer,
            shadow_renderer: &mut state.shadow_renderer,
            ui_text: &mut state.ui_text,
        }
    }
}

impl RenderState {
    pub fn new(animations: Animations, font: &Font) -> Self {
        Self {
            window_close_animations: WindowCloseAnimations::new(animations),
            background_renderer: BackgroundRenderer::default(),
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
