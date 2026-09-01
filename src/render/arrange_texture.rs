use std::collections::HashMap;
use std::error::Error;

use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Rectangle, Size};
use smithay::wayland::seat::WaylandFocus;

use super::window_texture::ResizeWindowTexture;

/// The resized endpoint becomes visible only after most geometry motion has
/// completed. This keeps the client-content transition subtle while the
/// compositor-rendered titlebar remains stable throughout.
const CROSSFADE_START: f64 = 0.60;

/// Arrange-owned client resize transitions.
///
/// This intentionally owns only client surface-tree textures. Server-side
/// titlebars, borders, shadows, and badges remain on the ordinary compositor
/// rendering path and are never part of this blend.
#[derive(Default)]
pub struct ArrangeTextureTransitions {
    windows: HashMap<WlSurface, ArrangeTexture>,
    resize_renderer: super::resize::ResizeRenderer,
}

struct ArrangeTexture {
    id: Id,
    previous: ResizeWindowTexture,
    target: Option<ResizeWindowTexture>,
    target_size: Size<i32, Logical>,
    target_ready_at_completion: Option<f64>,
    capture_generation: CommitCounter,
}

impl ArrangeTextureTransitions {
    /// The live client tree is held offscreen while its compositor snapshot is
    /// presented. Frame callbacks must still be delivered so it can paint the
    /// resized endpoint that will be blended in.
    pub fn awaiting_target(&self, surface: &WlSurface) -> bool {
        self.windows.contains_key(surface)
    }

    pub fn capture(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        target_size: Size<i32, Logical>,
        preserve_existing: bool,
    ) -> Result<(), Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("arrange snapshot window has no surface")?
            .into_owned();
        if should_preserve_existing(self.windows.contains_key(&surface), preserve_existing) {
            let entry = self
                .windows
                .get_mut(&surface)
                .expect("existing arrange snapshot checked above");
            // A reversal keeps the original outgoing endpoint so its pixels do
            // not jump at the retarget instant, but any endpoint captured for
            // the abandoned direction must not bleed into the reverse blend.
            entry.target_size = target_size;
            entry.target = None;
            entry.target_ready_at_completion = None;
            entry.capture_generation.increment();
            return Ok(());
        }

        let previous = super::window_texture::capture_for_resize(renderer, window, None)?;
        self.windows.insert(
            surface,
            ArrangeTexture {
                id: Id::new(),
                previous,
                target: None,
                target_size,
                target_ready_at_completion: None,
                capture_generation: CommitCounter::default(),
            },
        );
        Ok(())
    }

    /// Tries to freeze a complete resized client endpoint after a surface-tree
    /// commit. Firefox can commit resized roots and child surfaces separately,
    /// so incomplete trees are rejected and retried on later child commits.
    pub fn capture_target(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        arrange_completion: f64,
    ) -> Result<bool, Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("arrange target window has no surface")?
            .into_owned();
        let context = renderer.context_id();
        let Some(entry) = self.windows.get(&surface) else {
            return Ok(false);
        };
        if entry.previous.context != context {
            self.windows.remove(&surface);
            return Ok(false);
        }
        if !target_geometry_is_ready(window.geometry().size, entry.target_size) {
            return Ok(false);
        }

        let candidate = super::window_texture::capture_for_resize(renderer, window, None)?;
        let entry = self
            .windows
            .get_mut(&surface)
            .expect("arrange transition checked above");
        if !crate::render::fullscreen_texture::snapshot_matches_target_endpoint(
            &entry.previous,
            &candidate,
        ) || !crate::render::fullscreen_texture::expanding_endpoint_is_painted(
            &entry.previous,
            &candidate,
        ) {
            return Ok(false);
        }

        entry.target = Some(candidate);
        entry
            .target_ready_at_completion
            .get_or_insert(arrange_completion.clamp(0.0, 1.0));
        entry.capture_generation.increment();
        Ok(true)
    }

    pub fn blend_element(
        &mut self,
        renderer: &mut GlesRenderer,
        surface: &WlSurface,
        destination: Rectangle<i32, Physical>,
        arrange_completion: f64,
        alpha: f32,
        radii: super::window_decoration::CornerRadii,
    ) -> Result<Option<super::resize::ResizeRenderElement>, Box<dyn Error>> {
        let context = renderer.context_id();
        let Some(entry) = self.windows.get(surface) else {
            return Ok(None);
        };
        if entry.previous.context != context {
            self.windows.remove(surface);
            return Ok(None);
        }

        let previous = entry.previous.clone();
        let next = entry.target.clone().unwrap_or_else(|| previous.clone());
        let progress = crossfade_progress(
            arrange_completion,
            entry.target_ready_at_completion,
            entry.target.is_some(),
        ) as f32;
        let id = entry.id.clone();
        let generation = entry.capture_generation;
        Ok(Some(self.resize_renderer.element(
            renderer,
            id,
            &previous,
            next,
            destination,
            progress,
            alpha,
            radii,
            generation,
        )?))
    }

    pub fn retain_surfaces(&mut self, mut keep: impl FnMut(&WlSurface) -> bool) {
        self.windows.retain(|surface, _| keep(surface));
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
    }
}

fn target_geometry_is_ready(committed: Size<i32, Logical>, requested: Size<i32, Logical>) -> bool {
    committed == requested
}

fn crossfade_progress(
    arrange_completion: f64,
    target_ready_at_completion: Option<f64>,
    has_target: bool,
) -> f64 {
    if !has_target {
        return 0.0;
    }
    let start = CROSSFADE_START.max(
        target_ready_at_completion
            .unwrap_or(CROSSFADE_START)
            .clamp(0.0, 1.0),
    );
    if start >= 1.0 {
        return 1.0;
    }
    ((arrange_completion.clamp(0.0, 1.0) - start) / (1.0 - start)).clamp(0.0, 1.0)
}

fn should_preserve_existing(has_snapshot: bool, preserve_existing: bool) -> bool {
    has_snapshot && preserve_existing
}

#[cfg(test)]
mod tests {
    use super::{crossfade_progress, should_preserve_existing, target_geometry_is_ready};

    #[test]
    fn reversal_keeps_the_original_outgoing_frame_only_while_motion_is_live() {
        assert!(should_preserve_existing(true, true));
        assert!(!should_preserve_existing(true, false));
        assert!(!should_preserve_existing(false, true));
    }

    #[test]
    fn target_requires_the_committed_requested_geometry() {
        assert!(target_geometry_is_ready(
            (1200, 900).into(),
            (1200, 900).into()
        ));
        assert!(!target_geometry_is_ready(
            (800, 600).into(),
            (1200, 900).into()
        ));
    }

    #[test]
    fn client_endpoint_blends_only_near_the_destination() {
        assert_eq!(crossfade_progress(0.59, Some(0.1), true), 0.0);
        assert_eq!(crossfade_progress(0.60, Some(0.1), true), 0.0);
        assert!((crossfade_progress(0.80, Some(0.1), true) - 0.5).abs() < f64::EPSILON);
        assert_eq!(crossfade_progress(1.0, Some(0.1), true), 1.0);
    }

    #[test]
    fn late_target_starts_its_blend_without_an_opacity_jump() {
        assert_eq!(crossfade_progress(0.8, Some(0.8), true), 0.0);
        assert!((crossfade_progress(0.9, Some(0.8), true) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_target_keeps_the_outgoing_client_texture() {
        assert_eq!(crossfade_progress(1.0, None, false), 0.0);
    }
}
