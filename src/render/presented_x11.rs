use std::collections::HashMap;

use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::seat::WaylandFocus;

/// One XWayland surface tree whose buffers were part of a submitted frame.
///
/// Smithay's render elements retain both the imported renderer texture and a
/// [`Buffer`](smithay::backend::renderer::utils::Buffer) guard. Keeping the
/// elements alive therefore freezes this generation without copying its
/// pixels and without releasing the client buffer for reuse.
#[derive(Debug)]
pub struct PresentedX11Frame {
    pub(crate) surface: WlSurface,
    pub(crate) geometry: Rectangle<i32, Logical>,
    pub(crate) elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
}

/// Renderer-owned last-presented content for managed X11 windows.
#[derive(Default)]
pub struct PresentedX11Frames {
    frames: HashMap<WlSurface, PresentedX11Frame>,
}

impl PresentedX11Frames {
    pub fn get(&self, surface: &WlSurface) -> Option<&PresentedX11Frame> {
        self.frames.get(surface)
    }

    pub fn remove(&mut self, surface: &WlSurface) -> bool {
        self.frames.remove(surface).is_some()
    }

    /// Promotes frames only after their backend has confirmed presentation.
    /// Candidates for windows that disappeared while a page flip was pending
    /// are discarded instead of resurrecting stale buffer guards.
    pub fn promote(&mut self, candidates: Vec<PresentedX11Frame>, space: &Space<Window>) {
        for candidate in candidates {
            if mapped_managed_x11(space, &candidate.surface) {
                self.frames.insert(candidate.surface.clone(), candidate);
            }
        }
        self.frames
            .retain(|surface, _| mapped_managed_x11(space, surface));
    }
}

/// Retains local-coordinate surface elements for X11 windows that Smithay
/// reports as visible in the frame being submitted on `output`.
pub fn candidates_for_output(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    primary_output: &Output,
    states: &RenderElementStates,
) -> Vec<PresentedX11Frame> {
    space
        .elements()
        .filter(|window| candidate_window(window, output, primary_output, states))
        .filter_map(|window| {
            let surface = window.wl_surface()?.into_owned();
            let geometry = window.geometry();
            if geometry.size.w <= 0 || geometry.size.h <= 0 {
                return None;
            }
            let location =
                smithay::utils::Point::from((-geometry.loc.x, -geometry.loc.y)).to_physical(1);
            let elements = render_elements_from_surface_tree(
                renderer,
                &surface,
                location,
                1.0,
                1.0,
                smithay::backend::renderer::element::Kind::Unspecified,
            );
            (!elements.is_empty()).then_some(PresentedX11Frame {
                surface,
                geometry,
                elements,
            })
        })
        .collect()
}

fn candidate_window(
    window: &Window,
    output: &Output,
    primary_output: &Output,
    states: &RenderElementStates,
) -> bool {
    candidate_is_eligible(
        crate::xwayland::is_x11(window),
        crate::xwayland::is_override_redirect(window),
        crate::wayland::window_is_on_output(window, output, primary_output),
        window
            .wl_surface()
            .is_some_and(|surface| states.element_was_presented(surface.as_ref())),
    )
}

fn candidate_is_eligible(
    is_x11: bool,
    is_override_redirect: bool,
    is_on_output: bool,
    was_presented: bool,
) -> bool {
    is_x11 && !is_override_redirect && is_on_output && was_presented
}

fn mapped_managed_x11(space: &Space<Window>, surface: &WlSurface) -> bool {
    space.elements().any(|window| {
        crate::xwayland::is_x11(window)
            && !crate::xwayland::is_override_redirect(window)
            && window
                .wl_surface()
                .is_some_and(|candidate| candidate.as_ref() == surface)
    })
}

#[cfg(test)]
mod tests {
    use super::candidate_is_eligible;

    #[test]
    fn only_visible_managed_x11_content_becomes_a_candidate() {
        assert!(candidate_is_eligible(true, false, true, true));
        assert!(!candidate_is_eligible(false, false, true, true));
        assert!(!candidate_is_eligible(true, true, true, true));
        assert!(!candidate_is_eligible(true, false, false, true));
        assert!(!candidate_is_eligible(true, false, true, false));
    }
}
