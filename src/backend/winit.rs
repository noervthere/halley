use std::error::Error;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Element, Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::desktop::{Space, Window};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::shell::wlr_layer::Layer;

use super::{RenderStatus, Renderable};
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

render_elements! {
    /// Layer surfaces, window content, and border strips in one homogeneous
    /// scene list, so every element retains its intended stacking depth.
    /// Drawing borders in a second `draw_render_elements` pass instead - as
    /// this backend used to - composites *every* border over *every* window
    /// regardless of z-order, so a buried window's border bleeds across
    /// whatever is stacked on top of it.
    WinitRenderElement<=GlesRenderer>;
    Rescaled=super::rescale::RescaledElement,
    Cropped=CropRenderElement<super::rescale::RescaledElement>,
    Border=SolidColorRenderElement,
    Layer=WaylandSurfaceRenderElement<GlesRenderer>,
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

impl crate::ipc::OutputInfoSource for WinitBackend {
    fn output_info(&self) -> Vec<halley_ipc::OutputInfo> {
        let location = self.output.current_location();
        vec![halley_ipc::OutputInfo {
            name: self.output.name(),
            current_mode: crate::ipc::mode_info(self.output.current_mode()),
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
        _target_presentation_time: std::time::Duration,
        clear: Color32F,
        cursor: &CursorImage,
        cursor_position: (f64, f64),
        space: &Space<Window>,
        focused: Option<&WlSurface>,
        decorations: &halley_config::Decorations,
        cameras: &crate::camera::OutputCameras,
    ) -> Result<RenderStatus, Box<dyn Error>> {
        if output != &self.output {
            return Err(format!("winit cannot render unknown output {:?}", output.name()).into());
        }
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);
        let view = cameras
            .view(&self.output.name())
            .expect("winit output camera initialized at startup");
        let camera_center = view.center;
        let zoom_scale = view.scale;

        // Scoped so `renderer`/`framebuffer` (both borrowed from
        // `self.backend`) are dropped before `submit()` needs its own
        // mutable borrow.
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;

            // Built directly per mapped window (not via `space_render_elements`)
            // so a live `zoom_scale` can be applied per window - mirrors
            // `TtyBackend::render`'s own manual loop, keeping both backends'
            // zoom behavior identical and equally testable. Each window is
            // rendered at native scale/position first, then wrapped in
            // `RescaledElement` to draw into a `camera_rect`-scaled
            // destination - see its doc comment for why passing `zoom_scale`
            // straight into `render_elements` doesn't actually resize
            // anything.
            let mut scene_elements: Vec<WinitRenderElement> =
                super::layer_surface_elements(renderer, &self.output, Layer::Overlay)
                    .into_iter()
                    .map(WinitRenderElement::Layer)
                    .collect();
            scene_elements.extend(
                super::layer_surface_elements(renderer, &self.output, Layer::Top)
                    .into_iter()
                    .map(WinitRenderElement::Layer),
            );
            // `.rev()` is load-bearing: `Space::elements()` iterates z-order
            // *back to front* (bottom-most first), but a render element list
            // is front-to-back by convention - `draw_render_elements`
            // reverses whatever it's given before drawing, so feeding it
            // bottom-to-front order inverts z-order on screen and raising a
            // window would push it visually to the back. Smithay's own
            // `Space::render_elements_for_region` calls `.rev()` here for
            // exactly this reason.
            for window in space.elements().rev() {
                let Some(geometry) = space.element_geometry(window) else {
                    continue;
                };
                let Some(location) = space.element_location(window) else {
                    continue;
                };
                let scaled_bbox = super::camera_rect(geometry.to_physical(1), camera_center, size, zoom_scale);

                // `Space` locations refer to window geometry, while Smithay
                // renders from the underlying surface origin. GTK, Qt and
                // Firefox commonly use a non-zero geometry offset for CSD.
                let surface_location =
                    super::window_surface_location(location, window.geometry());
                let (popup_elements, surface_elements) =
                    super::window_surface_elements(renderer, window, surface_location);
                scene_elements.extend(popup_elements.into_iter().map(|surface_element| {
                    let native_geo = surface_element.geometry(Scale::from(1.0));
                    let dst = super::camera_rect(native_geo, camera_center, size, zoom_scale);
                    WinitRenderElement::Rescaled(super::rescale::RescaledElement::new(surface_element, dst))
                }));
                scene_elements.extend(surface_elements.into_iter().filter_map(|surface_element| {
                    let native_geo = surface_element.geometry(Scale::from(1.0));
                    let dst = super::camera_rect(native_geo, camera_center, size, zoom_scale);
                    let element = super::rescale::RescaledElement::new(surface_element, dst);
                    CropRenderElement::from_element(element, 1.0, scaled_bbox)
                        .map(WinitRenderElement::Cropped)
                }));

                let is_focused = window
                    .toplevel()
                    .is_some_and(|t| Some(t.wl_surface()) == focused);
                let color = super::window_border_color(decorations, is_focused);
                let border_width = ((decorations.border_width_px as f32 * zoom_scale).round() as i32).max(1);
                // Pushed alongside this window's own content rather than into
                // a separate list, so it stays at this window's depth (see
                // `WinitRenderElement`). Relative order against this window's
                // own content doesn't matter - `border_strips` builds strips
                // that sit entirely outside the window's bbox.
                scene_elements.extend(
                    super::border_strips(scaled_bbox, border_width, color)
                        .into_iter()
                        .map(WinitRenderElement::Border),
                );
            }

            scene_elements.extend(
                super::layer_surface_elements(renderer, &self.output, Layer::Bottom)
                    .into_iter()
                    .map(WinitRenderElement::Layer),
            );
            scene_elements.extend(
                super::layer_surface_elements(renderer, &self.output, Layer::Background)
                    .into_iter()
                    .map(WinitRenderElement::Layer),
            );

            // Built before renderer.render() borrows renderer for the frame -
            // doesn't need it beyond this transient import step.
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
            // The scene already follows layer-shell and window z-order; the
            // cursor uses a later pass so it remains above every client.
            draw_render_elements(&mut frame, 1.0, &scene_elements, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &[cursor_element], &[damage])?;
            // No cross-GPU/import synchronization needed for a plain clear -
            // discarding the fence is fine at this stage.
            let _ = frame.finish()?;
        }

        self.backend.submit(Some(&[damage]))?;

        Ok(RenderStatus::Submitted)
    }
}
