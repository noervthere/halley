use std::error::Error;

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::utils::{Rectangle, Transform};

use super::Renderable;

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
}

impl Renderable for WinitBackend {
    fn render(&mut self, clear: Color32F) -> Result<(), Box<dyn Error>> {
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);

        // Scoped so `renderer`/`framebuffer` (both borrowed from
        // `self.backend`) are dropped before `submit()` needs its own
        // mutable borrow.
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(clear, &[damage])?;
            // No cross-GPU/import synchronization needed for a plain clear -
            // discarding the fence is fine at this stage.
            let _ = frame.finish()?;
        }

        self.backend.submit(Some(&[damage]))?;

        Ok(())
    }
}
