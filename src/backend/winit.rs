use std::error::Error;

use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::WinitGraphicsBackend;

use super::Renderable;

/// The winit (nested) backend - a real window on the host's desktop,
/// standing in for real hardware output. Used for dev/testing; the tty
/// backend (real hardware, a separate later plan) will be a sibling module
/// implementing the same `Renderable` trait, not sharing state with this
/// one.
#[allow(dead_code)] // constructed in the next commit, once main.rs opens a window
pub struct WinitBackend {
    backend: WinitGraphicsBackend<GlesRenderer>,
}

impl WinitBackend {
    #[allow(dead_code)]
    pub fn new(backend: WinitGraphicsBackend<GlesRenderer>) -> Self {
        Self { backend }
    }
}

impl Renderable for WinitBackend {
    fn render(&mut self, _clear: Color32F) -> Result<(), Box<dyn Error>> {
        // TODO: bind -> render -> clear -> finish -> submit. Filled in once
        // a window actually exists to render into (next step).
        Ok(())
    }
}
