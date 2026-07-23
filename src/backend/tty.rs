use std::error::Error;

use smithay::backend::renderer::Color32F;

use super::Renderable;

/// The tty (DRM/KMS) backend - real hardware output, no host compositor
/// involved. Built up incrementally via `src/bin/tty_probe.rs` before this
/// struct holds real state; see the tty backend plan for the step sequence.
pub struct TtyBackend;

impl Renderable for TtyBackend {
    fn render(&mut self, _clear: Color32F) -> Result<(), Box<dyn Error>> {
        unimplemented!("tty backend under construction")
    }
}
