pub mod winit;

use smithay::backend::renderer::Color32F;

/// A backend that can render a single frame.
///
/// Deliberately narrow: takes only the clear color it needs, nothing else.
/// Old halley-wl's equivalent (`RenderBackend::draw_frame(&mut self, st:
/// &mut Halley, ...)`) baked a dependency on the *entire* compositor state
/// into the trait signature itself - a real flaw, confirmed by reading
/// `backend/interface.rs` in the old code. This doesn't repeat that: as
/// more backends/features land, this trait grows exactly the parameters a
/// render call actually needs, never "the whole state, just in case."
#[allow(dead_code)] // wired up (main.rs constructs+calls this) in the next commit
pub trait Renderable {
    fn render(&mut self, clear: Color32F) -> Result<(), Box<dyn std::error::Error>>;
}
