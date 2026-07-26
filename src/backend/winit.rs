use std::error::Error;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Frame, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::utils::{Physical, Rectangle, Size, Transform};

use super::{RenderRequest, RenderStatus, Renderable};

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

    pub fn dmabuf_capabilities(&mut self) -> super::dmabuf::DmabufCapabilities {
        let renderer = self.backend.renderer();
        let formats = renderer.dmabuf_formats();
        let main_device = EGLDevice::device_for_display(renderer.egl_context().display())
            .and_then(|device| device.try_get_render_node())
            .ok()
            .flatten()
            .map(|node| node.dev_id());

        super::dmabuf::DmabufCapabilities::new(main_device, formats)
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self.backend.renderer().import_dmabuf(dmabuf, None) {
            Ok(_) => true,
            Err(err) => {
                eventline::debug!("winit: failed to import DMA-BUF: {err}");
                false
            }
        }
    }

    /// Keeps the advertised wl_output mode in sync with the real window
    /// size after a resize - stale output geometry would mislead clients
    /// about how large they're allowed to be.
    pub fn update_output_mode(&self) {
        self.output
            .change_current_state(Some(output_mode(self.backend.window_size())), None, None, None);
    }
}

impl crate::ipc::OutputInfoSource for WinitBackend {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo> {
        let location = self.output.current_location();
        let mode = self
            .output
            .current_mode()
            .expect("winit output always has a current mode");
        vec![halley_ipc::OutputInfo {
            name: self.output.name(),
            modes: vec![crate::ipc::mode_info(mode, true)],
            current_mode: Some(0),
            offset_x: location.x,
            offset_y: location.y,
            // The dev-mode nested backend has no real VRR/hardware concept
            // at all - config-driven output selection doesn't apply to it
            // either (see `WinitBackend`'s own doc comment).
            vrr: "off".to_string(),
        }]
    }
}

impl Renderable for WinitBackend {
    fn render(
        &mut self,
        output: &Output,
        request: RenderRequest<'_>,
    ) -> Result<RenderStatus, Box<dyn Error>> {
        if output != &self.output {
            return Err(format!("winit cannot render unknown output {:?}", output.name()).into());
        }
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);
        let output_geometry = request
            .space
            .output_geometry(output)
            .ok_or_else(|| format!("winit output {:?} is not mapped", output.name()))?;

        // Scoped so `renderer`/`framebuffer` (both borrowed from
        // `self.backend`) are dropped before `submit()` needs its own
        // mutable borrow.
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;
            let elements = super::scene::build(
                renderer,
                output,
                &self.output,
                output_geometry,
                request,
            )?;

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(request.clear, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            let _ = frame.finish()?;
        }

        self.backend.submit(Some(&[damage]))?;

        Ok(RenderStatus::Submitted)
    }
}
