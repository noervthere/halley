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
pub struct WinitBackend {
    #[allow(dead_code)] // read starting next commit, when render() is filled in
    backend: WinitGraphicsBackend<GlesRenderer>,
}

impl WinitBackend {
    pub fn new(backend: WinitGraphicsBackend<GlesRenderer>) -> Self {
        Self { backend }
    }
}

impl Renderable for WinitBackend {
    fn render(&mut self, _clear: Color32F) -> Result<(), Box<dyn Error>> {
        // TODO: bind -> render -> clear -> finish -> submit (next commit).
        Ok(())
    }
}
