use std::error::Error;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::desktop::{Space, Window};
use smithay::desktop::space::space_render_elements;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
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
    /// The wl_output clients see for this window. A plain geometry/mode
    /// object - constructing it needs no `DisplayHandle`, so this backend
    /// still never touches Wayland protocol state directly; the driving
    /// code (main.rs) is what registers the actual global and maps it into
    /// a `Space`.
    output: Output,
}

fn output_mode(size: Size<i32, Physical>) -> Mode {
    Mode {
        size,
        refresh: 60_000,
    }
}

impl WinitBackend {
    pub fn new(backend: WinitGraphicsBackend<GlesRenderer>) -> Self {
        let mode = output_mode(backend.window_size());
        let output = Output::new(
            "winit".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "halley-next".into(),
                model: "winit".into(),
                serial_number: "unknown".into(),
            },
        );
        output.change_current_state(
            Some(mode),
            Some(Transform::Flipped180),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);

        Self { backend, output }
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

    pub fn output(&self) -> &Output {
        &self.output
    }

    /// Keeps the advertised wl_output mode in sync with the real window
    /// size after a resize - stale output geometry would mislead clients
    /// about how large they're allowed to be.
    pub fn update_output_mode(&self) {
        self.output
            .change_current_state(Some(output_mode(self.backend.window_size())), None, None, None);
    }
}

impl Renderable for WinitBackend {
    fn render(
        &mut self,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
        space: &Space<Window>,
    ) -> Result<(), Box<dyn Error>> {
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);

        // Scoped so `renderer`/`framebuffer` (both borrowed from
        // `self.backend`) are dropped before `submit()` needs its own
        // mutable borrow.
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;

            // Both built before renderer.render() borrows renderer for the
            // frame - neither needs it beyond this transient import step.
            let space_elements = space_render_elements::<_, Window, _>(renderer, [space], &self.output, 1.0)?;
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
            // Windows first, cursor last - the cursor composites on top.
            draw_render_elements(&mut frame, 1.0, &space_elements, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &[cursor_element], &[damage])?;
            // No cross-GPU/import synchronization needed for a plain clear -
            // discarding the fence is fine at this stage.
            let _ = frame.finish()?;
        }

        self.backend.submit(Some(&[damage]))?;

        Ok(())
    }
}
