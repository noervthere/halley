pub mod tty;
pub mod winit;

use halley_config::Decorations;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle};

use crate::cursor::CursorImage;

/// Cool, slightly-blue-leaning light gray - visible without being stark.
/// Shared by both backends' drivers (session::tty, session::winit) so there's one
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
/// "the whole state, just in case" - `cursor`, `focused`, and `decorations`
/// are each a legitimate growth (every frame concretely needs to know what
/// to draw and how), not the bloat this doc comment warns against.
pub trait Renderable {
    fn render(
        &mut self,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
        space: &Space<Window>,
        focused: Option<&WlSurface>,
        decorations: &Decorations,
    ) -> Result<(), Box<dyn std::error::Error>>;
}

fn border_color(color: halley_config::BorderColor) -> Color32F {
    Color32F::new(color.r, color.g, color.b, 1.0)
}

/// Whichever color a window's border should be drawn in, given whether its
/// surface is the focused one.
pub fn window_border_color(decorations: &Decorations, is_focused: bool) -> Color32F {
    border_color(if is_focused {
        decorations.border_color_focused
    } else {
        decorations.border_color_unfocused
    })
}

/// Four thin solid-color strips forming a frame just outside `bbox` - not
/// overlapping window content, since nothing shrinks a window to make room
/// for a border yet (that's real decoration-chrome-painting work, deferred).
/// A fresh `Id` per call is fine: neither backend does incremental damage
/// tracking already - a full-output clear + redraw happens every frame
/// regardless.
pub fn border_strips(
    bbox: Rectangle<i32, Physical>,
    width: i32,
    color: Color32F,
) -> [SolidColorRenderElement; 4] {
    let make = |rect: Rectangle<i32, Physical>| {
        SolidColorRenderElement::new(Id::new(), rect, CommitCounter::default(), color, Kind::Unspecified)
    };
    let top = Rectangle::new(
        (bbox.loc.x - width, bbox.loc.y - width).into(),
        (bbox.size.w + width * 2, width).into(),
    );
    let bottom = Rectangle::new(
        (bbox.loc.x - width, bbox.loc.y + bbox.size.h).into(),
        (bbox.size.w + width * 2, width).into(),
    );
    let left = Rectangle::new(
        (bbox.loc.x - width, bbox.loc.y).into(),
        (width, bbox.size.h).into(),
    );
    let right = Rectangle::new(
        (bbox.loc.x + bbox.size.w, bbox.loc.y).into(),
        (width, bbox.size.h).into(),
    );
    [make(top), make(bottom), make(left), make(right)]
}
