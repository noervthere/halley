use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;

use halley_config::Animations;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Rectangle};
use smithay::wayland::seat::WaylandFocus;

use crate::animation::close::CloseTimeline;
use crate::camera::OutputCameras;

#[derive(Clone, Copy, Debug)]
pub struct CloseBorder {
    pub width: i32,
    pub color: Color32F,
}

#[derive(Clone, Copy, Debug)]
pub enum CloseAnchor {
    Windowed {
        world_geometry: Rectangle<i32, Logical>,
        captured_camera_rect: Rectangle<i32, Physical>,
    },
    OutputLocal,
}

#[derive(Clone, Debug)]
pub struct CloseSnapshotMetadata {
    pub output_name: String,
    pub initial_destination: Rectangle<i32, Physical>,
    pub anchor: CloseAnchor,
    pub stack_index: usize,
    pub start_alpha: f32,
    pub border: Option<CloseBorder>,
}

struct CapturedWindow {
    id: Id,
    texture: super::window_texture::WindowTexture,
    metadata: CloseSnapshotMetadata,
}

struct ActiveClose {
    captured: CapturedWindow,
    timeline: CloseTimeline,
    order: u64,
}

pub struct ClosingWindowRender {
    pub texture: TextureRenderElement<GlesTexture>,
    pub destination: Rectangle<i32, Physical>,
    pub border: Option<CloseBorder>,
    pub stack_index: usize,
    pub order: u64,
}

pub struct WindowCloseAnimations {
    config: Animations,
    pending: HashMap<WlSurface, CapturedWindow>,
    active: HashMap<WlSurface, ActiveClose>,
    next_order: u64,
}

impl WindowCloseAnimations {
    pub fn new(config: Animations) -> Self {
        Self {
            config,
            pending: HashMap::new(),
            active: HashMap::new(),
            next_order: 0,
        }
    }

    pub fn capture(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        metadata: CloseSnapshotMetadata,
    ) -> Result<bool, Box<dyn Error>> {
        let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
            return Ok(false);
        };
        if !close_enabled(self.config) {
            self.pending.remove(&surface);
            return Ok(false);
        }

        let texture = super::window_texture::capture(renderer, window, None)?;
        self.pending.insert(
            surface,
            CapturedWindow {
                id: Id::new(),
                texture,
                metadata,
            },
        );
        Ok(true)
    }

    pub fn start(&mut self, surface: &WlSurface, now: Duration) -> bool {
        let Some(captured) = self.pending.remove(surface) else {
            return false;
        };
        if !close_enabled(self.config) {
            return false;
        }

        let config = self.config.window_close;
        self.next_order = self.next_order.wrapping_add(1);
        self.active.insert(
            surface.clone(),
            ActiveClose {
                timeline: CloseTimeline::new(config, now, captured.metadata.start_alpha),
                captured,
                order: self.next_order,
            },
        );
        true
    }

    pub fn cancel(&mut self, surface: &WlSurface) {
        self.pending.remove(surface);
        self.active.remove(surface);
    }

    pub fn discard_pending(&mut self, surface: &WlSurface) {
        self.pending.remove(surface);
    }

    pub fn reload(&mut self, config: Animations) {
        self.config = config;
        if !close_enabled(config) {
            self.pending.clear();
        }
    }

    pub fn is_animating_on_output(&self, output: &Output, now: Duration) -> bool {
        self.active.values().any(|active| {
            active.captured.metadata.output_name == output.name()
                && !active.timeline.is_finished_at(now)
        })
    }

    pub fn renders_for_output(
        &self,
        renderer: &GlesRenderer,
        output: &Output,
        output_geometry: Rectangle<i32, Logical>,
        cameras: &OutputCameras,
        now: Duration,
    ) -> Vec<ClosingWindowRender> {
        let context = renderer.context_id();
        self.active
            .values()
            .filter(|active| {
                active.captured.metadata.output_name == output.name()
                    && active.captured.texture.context == context
                    && !active.timeline.is_finished_at(now)
            })
            .filter_map(|active| {
                let metadata = &active.captured.metadata;
                let start = destination_for(metadata, output, output_geometry, cameras)?;
                let visual = active.timeline.visual_at(now);
                let destination =
                    crate::animation::scale_rect_from_center(start, start, visual.scale);
                if destination.size.w <= 0 || destination.size.h <= 0 || visual.alpha <= 0.0 {
                    return None;
                }
                let border = metadata.border.map(|mut border| {
                    border.width = (f64::from(border.width) * visual.scale).round().max(1.0) as i32;
                    border.color = border.color * visual.alpha;
                    border
                });
                Some(ClosingWindowRender {
                    texture: active.captured.texture.render_element(
                        active.captured.id.clone(),
                        destination,
                        visual.alpha,
                    ),
                    destination,
                    border,
                    stack_index: metadata.stack_index,
                    order: active.order,
                })
            })
            .collect()
    }

    pub fn cleanup(&mut self, now: Duration) {
        self.active
            .retain(|_, active| !active.timeline.is_finished_at(now));
    }
}

fn close_enabled(config: Animations) -> bool {
    config.enabled && config.window_close.enabled && config.window_close.duration_ms > 0
}

fn destination_for(
    metadata: &CloseSnapshotMetadata,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    cameras: &OutputCameras,
) -> Option<Rectangle<i32, Physical>> {
    match metadata.anchor {
        CloseAnchor::OutputLocal => Some(metadata.initial_destination),
        CloseAnchor::Windowed {
            world_geometry,
            captured_camera_rect,
        } => {
            let view = cameras.view(&output.name())?;
            let camera_center = crate::camera::global_center(view.center, output_geometry);
            let current_camera_rect = super::camera_rect(
                world_geometry.to_physical(1),
                camera_center,
                output_geometry.size.to_physical(1),
                view.scale,
            );
            Some(resolve_windowed_destination(
                metadata.initial_destination,
                captured_camera_rect,
                current_camera_rect,
            ))
        }
    }
}

fn resolve_windowed_destination(
    initial: Rectangle<i32, Physical>,
    captured_camera_rect: Rectangle<i32, Physical>,
    current_camera_rect: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    crate::animation::map_rect(initial, captured_camera_rect, current_camera_rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_policy_respects_master_local_and_zero_duration_killswitches() {
        let mut animations = Animations::default();
        assert!(close_enabled(animations));

        animations.enabled = false;
        assert!(!close_enabled(animations));
        animations.enabled = true;
        animations.window_close.enabled = false;
        assert!(!close_enabled(animations));
        animations.window_close.enabled = true;
        animations.window_close.duration_ms = 0;
        assert!(!close_enabled(animations));
    }

    #[test]
    fn windowed_ghost_tracks_camera_translation_and_scale() {
        let initial = Rectangle::new((250, 150).into(), (400, 300).into());
        let captured = Rectangle::new((100, 50).into(), (800, 600).into());
        let current = Rectangle::new((0, 0).into(), (400, 300).into());

        assert_eq!(
            resolve_windowed_destination(initial, captured, current),
            Rectangle::new((75, 50).into(), (200, 150).into())
        );
    }
}
