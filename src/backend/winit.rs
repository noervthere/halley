use std::error::Error;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::utils::{Physical, Point, Rectangle, Size, Transform};

use super::Renderable;
use crate::cursor::CursorImage;

/// The winit (nested) backend - a real window on the host's desktop,
/// standing in for real hardware output. Used for dev/testing; the tty
/// backend (real hardware, a separate later plan) will be a sibling module
/// implementing the same `Renderable` trait, not sharing state with this
/// one.
pub struct WinitBackend {
    backend: WinitGraphicsBackend<GlesRenderer>,
}

impl WinitBackend {
    pub fn new(backend: WinitGraphicsBackend<GlesRenderer>) -> Self {
        Self { backend }
    }

    /// Ask the window for another `Redraw` event. Winit-specific (not part
    /// of `Renderable`) since the tty backend's equivalent is triggered by
    /// vblank/page-flip events, not a request call like this - there's no
    /// shared shape between the two worth forcing into one trait method.
    pub fn request_redraw(&self) {
        self.backend.window().request_redraw();
    }

    /// The window's current size - used by callers to convert absolute
    /// pointer positions into this output's coordinate space.
    pub fn window_size(&self) -> Size<i32, Physical> {
        self.backend.window_size()
    }
}

impl Renderable for WinitBackend {
    fn render(
        &mut self,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
    ) -> Result<(), Box<dyn Error>> {
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);

        // Scoped so `renderer`/`framebuffer` (both borrowed from
        // `self.backend`) are dropped before `submit()` needs its own
        // mutable borrow.
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;

            // Built before renderer.render() borrows renderer for the frame -
            // from_buffer() only needs it transiently to import the texture.
            let cursor_element = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                Point::<f64, Physical>::from(cursor_position),
                &cursor.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            )?;

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(clear, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &[cursor_element], &[damage])?;
            // No cross-GPU/import synchronization needed for a plain clear -
            // discarding the fence is fine at this stage.
            let _ = frame.finish()?;
        }

        self.backend.submit(Some(&[damage]))?;

        Ok(())
    }
}
