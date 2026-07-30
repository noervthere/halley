use std::error::Error;

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{CommitCounter, with_renderer_surface_state};
use smithay::desktop::{PopupManager, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Physical, Rectangle, Scale};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer;

use super::RenderRequest;
use crate::input::presentation::window_visual_state;
mod capture_ui;
mod effects;
mod nodes;
mod overview;
mod windows;

use capture_ui::capture_overlay_elements;
use effects::{
    append_compositor_overlay_blur, append_overlay_shadows, layer_surface_scene_elements,
};
use nodes::{NodeElementContext, node_elements};
use overview::{apogee_elements, focus_cycle_elements, hover_preview_elements};
use windows::{LiveWindowContext, StackGroup, live_window_elements, sort_stack_groups};

#[cfg(test)]
use capture_ui::{capture_picker_elements, dashed_border_rects, inner_border_rects};
#[cfg(test)]
use overview::{
    apogee_caption_rect, apogee_transition_visuals, aspect_fit_rect, outset_physical,
    preview_content_radius, sort_apogee_tiles,
};

#[cfg(test)]
use smithay::utils::Point;

render_elements! {
    /// The complete front-to-back scene consumed by both presentation
    /// backends. Keeping one element type and one builder prevents nested and
    /// real-hardware sessions from drifting in z-order or visual policy.
    pub SceneElement<=GlesRenderer>;
    Cursor=crate::cursor::render::CursorRenderElement,
    Rescaled=super::rescale::RescaledElement,
    Cropped=CropRenderElement<super::rescale::RescaledElement>,
    RoundedCropped=CropRenderElement<super::window_decoration::RoundedSurfaceElement>,
    FullscreenBlend=super::fullscreen_texture::FullscreenBlendElement,
    WindowBorder=super::window_decoration::RoundedBorderElement,
    RoundedClosing=super::window_decoration::RoundedTextureElement,
    Node=super::node::NodeRenderElement,
    NodeLabel=super::node::LabelRenderElement,
    NodeTexture=super::node::NodeTextureElement,
    UiText=super::text::UiTextElement,
    BackdropBlur=super::effects::backdrop_blur::BackdropBlurElement,
    Shadow=super::effects::shadow::ShadowElement,
    Closing=smithay::backend::renderer::element::texture::TextureRenderElement<
        smithay::backend::renderer::gles::GlesTexture
    >,
    CaptureOverlay=super::overlays::capture::CaptureOverlayElement,
    Border=SolidColorRenderElement,
    Layer=WaylandSurfaceRenderElement<GlesRenderer>,
}

pub fn build(
    renderer: &mut GlesRenderer,
    output: &Output,
    primary_output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    request: RenderRequest<'_>,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    if request.desktop.session_lock.active() {
        let scale = Scale::from(output.current_scale().fractional_scale());
        let mut elements = request
            .desktop
            .session_lock
            .surfaces_for_output(output)
            .flat_map(|surface| {
                smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
                    renderer,
                    surface.wl_surface(),
                    (0, 0),
                    scale,
                    1.0,
                    Kind::ScanoutCandidate,
                )
            })
            .map(SceneElement::Layer)
            .collect::<Vec<_>>();
        let backdrop_size = output_geometry
            .size
            .to_f64()
            .to_physical(scale)
            .to_i32_round();
        elements.push(SceneElement::Border(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::from_size(backdrop_size),
            CommitCounter::default(),
            super::SESSION_LOCK_COLOR,
            Kind::Unspecified,
        )));
        if request.cursor.show_cursor {
            let cursor = crate::cursor::render::elements(
                renderer,
                request.cursor.cursor,
                output,
                output_geometry,
                request.cursor.cursor_position,
                request.frame.target_presentation_time,
                request.cursor.cursor_override,
            )?;
            elements.splice(0..0, cursor.into_iter().map(SceneElement::Cursor));
        }
        return Ok(elements);
    }

    request
        .desktop
        .cameras
        .view(&output.name())
        .ok_or_else(|| format!("output {:?} has no camera", output.name()))?;
    if request.visuals.decorations.border_radius_px > 0 {
        // Compile both programs as one capability before any window needs
        // them. A bad driver/compiler therefore selects the coherent square
        // fallback at startup instead of halfway through a transition.
        request
            .resources
            .window_decoration_renderer
            .available(renderer);
    }
    let overlay_snapshot = request
        .overlays
        .overlays
        .snapshot(&output.name(), request.frame.target_presentation_time);

    // Apogee is a replacement scene, not a translucent layer over the live
    // desktop. Keep only its tiles and the wallpaper layer behind them; normal
    // windows, nodes, panels, and desktop overlays must not bleed through.
    if request.overlays.apogee.is_active() {
        let mut elements = apogee_elements(
            renderer,
            output,
            output_geometry,
            request.overlays.apogee,
            request.overlays.apogee_config,
            request.overlays.overlay_config,
            request.visuals.decorations,
            request.desktop.space,
            request.desktop.cameras,
            request.desktop.nodes,
            request.resources.node_renderer,
            request.resources.window_decoration_renderer,
            request.resources.ui_text,
            request.desktop.window_open_animations,
            request.desktop.fullscreen,
            request.desktop.maximize,
            request.resources.overlay_previews,
            request.frame.target_presentation_time,
        )?;
        append_overlay_shadows(
            renderer,
            output,
            "apogee",
            request.visuals.shadows.overlay,
            request.resources.shadow_renderer,
            &mut elements,
        )?;
        elements.extend(
            super::layer_surface_elements(renderer, output, Layer::Background)
                .into_iter()
                .map(SceneElement::Layer),
        );
        let mut overlay_elements = super::overlays::shell::elements(
            renderer,
            output_geometry,
            overlay_snapshot,
            request.overlays.overlay_config,
            request.visuals.decorations,
            request.resources.node_renderer,
            request.resources.ui_text,
        )?;
        append_compositor_overlay_blur(
            renderer,
            output,
            output_geometry.size,
            "shell-overlay",
            request.visuals.blur,
            request.visuals.shadows.overlay,
            request.resources.backdrop_blur_renderer,
            request.resources.shadow_renderer,
            &mut overlay_elements,
        )?;
        elements.splice(0..0, overlay_elements);
        if request.cursor.show_cursor {
            let cursor = crate::cursor::render::elements(
                renderer,
                request.cursor.cursor,
                output,
                output_geometry,
                request.cursor.cursor_position,
                request.frame.target_presentation_time,
                request.cursor.cursor_override,
            )?;
            elements.splice(0..0, cursor.into_iter().map(SceneElement::Cursor));
        }
        return Ok(elements);
    }

    let mut elements = capture_overlay_elements(
        renderer,
        output,
        output_geometry,
        request.overlays.capture_overlay,
        request.resources.node_renderer,
        request.overlays.overlay_config,
        request.visuals.decorations,
    )?;
    let mut bearings = super::overlays::bearings::elements(
        renderer,
        output,
        output_geometry,
        request.overlays.bearings,
        request.desktop.nodes,
        request.desktop.cameras,
        request.visuals.blur,
        request.resources.backdrop_blur_renderer,
        request.resources.node_renderer,
        request.resources.ui_text,
        request.overlays.overlay_config,
        request.visuals.decorations,
    )?;
    append_overlay_shadows(
        renderer,
        output,
        "bearings",
        request.visuals.shadows.overlay,
        request.resources.shadow_renderer,
        &mut bearings,
    )?;
    elements.extend(bearings);
    elements.extend(layer_surface_scene_elements(
        renderer,
        output,
        output_geometry,
        Layer::Overlay,
        request.visuals.blur,
        request.resources.backdrop_blur_renderer,
    )?);
    let mut focus_cycle = focus_cycle_elements(
        renderer,
        output_geometry,
        request.overlays.focus_cycle,
        request.desktop.nodes,
        request.resources.overlay_previews,
        request.resources.node_renderer,
        request.resources.window_decoration_renderer,
        request.resources.ui_text,
        request.overlays.overlay_config,
        request.visuals.decorations,
        request.frame.target_presentation_time,
    )?;
    append_compositor_overlay_blur(
        renderer,
        output,
        output_geometry.size,
        "focus-cycle",
        request.visuals.blur,
        request.visuals.shadows.overlay,
        request.resources.backdrop_blur_renderer,
        request.resources.shadow_renderer,
        &mut focus_cycle,
    )?;
    elements.extend(focus_cycle);
    if !request.desktop.fullscreen.covers_top(
        request.desktop.focused,
        output,
        request.frame.target_presentation_time,
    ) {
        elements.extend(layer_surface_scene_elements(
            renderer,
            output,
            output_geometry,
            Layer::Top,
            request.visuals.blur,
            request.resources.backdrop_blur_renderer,
        )?);
    }

    let mut hover_preview = hover_preview_elements(
        renderer,
        output,
        output_geometry,
        request.desktop.nodes,
        request.desktop.cameras,
        request.resources.overlay_previews,
        request.resources.node_renderer,
        request.resources.ui_text,
        request.resources.window_decoration_renderer,
        request.overlays.overlay_config,
        request.visuals.decorations,
        request.frame.target_presentation_time,
    )?;
    append_compositor_overlay_blur(
        renderer,
        output,
        output_geometry.size,
        "node-hover-preview",
        request.visuals.blur,
        request.visuals.shadows.overlay,
        request.resources.backdrop_blur_renderer,
        request.resources.shadow_renderer,
        &mut hover_preview,
    )?;
    elements.extend(hover_preview);

    let node_scene = node_elements(
        renderer,
        request.resources.node_renderer,
        request.resources.ui_text,
        NodeElementContext {
            output,
            output_geometry,
            nodes: request.desktop.nodes,
            node_grab_active: request.desktop.node_grab_active,
            cameras: request.desktop.cameras,
            decorations: request.visuals.decorations,
            shadow_config: request.visuals.shadows.node,
            shadow_renderer: request.resources.shadow_renderer,
            now: request.frame.target_presentation_time,
        },
    )?;
    elements.extend(node_scene.overlay);

    let mut stack = request
        .resources
        .window_close_animations
        .renders_for_output(
            renderer,
            output,
            output_geometry,
            request.desktop.cameras,
            request.frame.target_presentation_time,
        )
        .into_iter()
        .map(|closing| -> Result<StackGroup, Box<dyn Error>> {
            let mut elements = Vec::new();
            let shadow_alpha = closing.texture.alpha();
            let border_width = closing.border.map(|border| border.width).unwrap_or(0);
            let rounded = closing.content_radius > 0.0
                && request
                    .resources
                    .window_decoration_renderer
                    .available(renderer);
            if let Some(border) = closing.border {
                if rounded
                    && let Some(border) =
                        request.resources.window_decoration_renderer.border_element(
                            renderer,
                            closing.destination,
                            border.width,
                            closing.content_radius,
                            border.color,
                            1.0,
                        )
                {
                    elements.push(SceneElement::WindowBorder(border));
                } else {
                    elements.extend(
                        super::border_strips(closing.destination, border.width, border.color)
                            .into_iter()
                            .map(SceneElement::Border),
                    );
                }
            }
            if rounded {
                let texture = request
                    .resources
                    .window_decoration_renderer
                    .texture_element(
                        renderer,
                        closing.texture,
                        closing.source_texture,
                        closing.destination,
                        closing.content_radius,
                    )
                    .expect("rounded resources were checked above");
                elements.push(SceneElement::RoundedClosing(texture));
            } else {
                elements.push(SceneElement::Closing(closing.texture));
            }
            let caster = Rectangle::new(
                (
                    closing.destination.loc.x - border_width,
                    closing.destination.loc.y - border_width,
                )
                    .into(),
                (
                    closing.destination.size.w + border_width * 2,
                    closing.destination.size.h + border_width * 2,
                )
                    .into(),
            );
            if let Some(shadow) = request.resources.shadow_renderer.element(
                renderer,
                format!("{}:closing:{}", output.name(), closing.order),
                caster,
                if rounded {
                    closing.content_radius + border_width as f32
                } else {
                    0.0
                },
                shadow_alpha,
                request.visuals.shadows.window,
            )? {
                elements.push(SceneElement::Shadow(shadow));
            }
            Ok(StackGroup {
                stack_index: closing.stack_index,
                order: closing.order,
                elements,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    stack.extend(node_scene.groups);
    let context = LiveWindowContext {
        space: request.desktop.space,
        output,
        output_geometry,
        cameras: request.desktop.cameras,
        target_presentation_time: request.frame.target_presentation_time,
        focused: request.desktop.focused,
        decorations: request.visuals.decorations,
        blur: request.visuals.blur,
        shadow_config: request.visuals.shadows.window,
        window_open_animations: request.desktop.window_open_animations,
        fullscreen: request.desktop.fullscreen,
        maximize: request.desktop.maximize,
    };
    for (stack_index, window) in request.desktop.space.elements().enumerate() {
        if !crate::wayland::window_is_on_output(window, output, primary_output) {
            continue;
        }
        let window_elements = live_window_elements(
            renderer,
            window,
            context,
            request.resources.fullscreen_textures,
            request.resources.backdrop_blur_renderer,
            request.resources.shadow_renderer,
            request.resources.window_decoration_renderer,
        )?;
        if !window_elements.is_empty() {
            stack.push(StackGroup {
                stack_index,
                order: u64::MAX,
                elements: window_elements,
            });
        }
    }
    sort_stack_groups(&mut stack);
    elements.extend(stack.into_iter().rev().flat_map(|group| group.elements));

    elements.extend(layer_surface_scene_elements(
        renderer,
        output,
        output_geometry,
        Layer::Bottom,
        request.visuals.blur,
        request.resources.backdrop_blur_renderer,
    )?);
    elements.extend(layer_surface_scene_elements(
        renderer,
        output,
        output_geometry,
        Layer::Background,
        request.visuals.blur,
        request.resources.backdrop_blur_renderer,
    )?);

    let mut overlay_elements = super::overlays::shell::elements(
        renderer,
        output_geometry,
        overlay_snapshot,
        request.overlays.overlay_config,
        request.visuals.decorations,
        request.resources.node_renderer,
        request.resources.ui_text,
    )?;
    append_compositor_overlay_blur(
        renderer,
        output,
        output_geometry.size,
        "shell-overlay",
        request.visuals.blur,
        request.visuals.shadows.overlay,
        request.resources.backdrop_blur_renderer,
        request.resources.shadow_renderer,
        &mut overlay_elements,
    )?;
    elements.splice(0..0, overlay_elements);

    if request.cursor.show_cursor {
        let cursor = crate::cursor::render::elements(
            renderer,
            request.cursor.cursor,
            output,
            output_geometry,
            request.cursor.cursor_position,
            request.frame.target_presentation_time,
            request.cursor.cursor_override,
        )?;
        // Element lists are front-to-back, so cursor surface trees belong
        // before every compositor and client element.
        elements.splice(0..0, cursor.into_iter().map(SceneElement::Cursor));
    }

    Ok(elements)
}
