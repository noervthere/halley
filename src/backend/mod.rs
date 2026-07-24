pub mod tty;
pub mod winit;

use smithay::backend::renderer::Color32F;
use smithay::desktop::{Space, Window};

use crate::cursor::CursorImage;

/// Cool, slightly-blue-leaning light gray - visible without being stark.
/// Shared by both backends' drivers (main.rs, halley-tty.rs) so there's one
/// definition instead of two independently-maintained literals.
pub const CLEAR_COLOR: Color32F = Color32F::new(0.58, 0.64, 0.72, 1.0);

/// A backend that can render a single frame.
///
/// Deliberately narrow: takes only what it needs, nothing else. Old
/// halley-wl's equivalent (`RenderBackend::draw_frame(&mut self, st: &mut
/// Halley, ...)`) baked a dependency on the *entire* compositor state into
/// the trait signature itself - a real flaw, confirmed by reading
/// `backend/interface.rs` in the old code. This doesn't repeat that: the
/// trait grows exactly the parameters a render call actually needs, never
/// "the whole state, just in case" - `cursor` is a legitimate growth (every
/// frame concretely needs it drawn now), not the bloat this doc comment
/// warns against.
pub trait Renderable {
    fn render(
        &mut self,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
        space: &Space<Window>,
    ) -> Result<(), Box<dyn std::error::Error>>;
}
