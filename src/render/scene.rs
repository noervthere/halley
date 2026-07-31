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
use crate::presentation::window::window_visual_state;
mod capture_ui;
mod clusters;
mod effects;
mod nodes;
mod overview;
mod windows;

use capture_ui::capture_overlay_elements;
use clusters::{ClusterElementContext, cluster_elements};
use effects::{
    OverlayEffectStyle, append_compositor_overlay_blur, append_overlay_shadows,
    layer_surface_scene_elements,
};
use nodes::{NodeElementContext, node_elements};
use overview::{
    FocusCycleRenderContext, apogee_elements, focus_cycle_elements, hover_preview_elements,
};
use windows::{LiveWindowContext, StackGroup, live_window_elements, sort_stack_groups};

#[cfg(test)]
use capture_ui::{capture_picker_elements, dashed_border_rects, inner_border_rects};
#[cfg(test)]
use overview::{
    apogee_caption_rect, apogee_transition_visuals, aspect_fit_rect, outset_physical, outset_rect,
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
    Background=super::background::BackgroundElement,
    Rescaled=super::rescale::RescaledElement,
    Cropped=CropRenderElement<super::rescale::RescaledElement>,
    RoundedCropped=CropRenderElement<super::window_decoration::RoundedSurfaceElement>,
    FullscreenBlend=super::fullscreen_texture::FullscreenBlendElement,
    WindowBorder=super::window_decoration::RoundedBorderElement,
    RoundedClosing=super::window_decoration::RoundedTextureElement,
    ClusterCore=crate::clusters::render::ClusterCoreElement,
    ClusterIcon=crate::clusters::render::ClusterIconElement,
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
    SourceChooser=super::overlays::source_chooser::SourceChooserElement,
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
        append_background(
            renderer,
            request.resources.background_renderer,
            BackgroundSceneRequest {
                output,
                output_geometry,
                cameras: request.desktop.cameras,
                config: request.visuals.background,
                config_dir: request.visuals.background_base,
                now: request.frame.target_presentation_time,
            },
            &mut elements,
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
            OverlayEffectStyle {
                output_size: output_geometry.size,
                identity: "shell-overlay",
                blur: request.visuals.blur,
                shadow: request.visuals.shadows.overlay,
            },
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
    let mut cluster_creation = super::overlays::cluster_creation::elements(
        renderer,
        request.resources.cluster_creation_overlay,
        output,
        output_geometry,
        request.desktop.clusters.creation(),
        request.desktop.nodes,
        request.desktop.cameras,
        request.cursor.cursor_position,
        request.overlays.overlay_config,
        request.visuals.decorations,
        request.resources.node_renderer,
        request.resources.ui_text,
    )?;
    append_compositor_overlay_blur(
        renderer,
        output,
        OverlayEffectStyle {
            output_size: output_geometry.size,
            identity: "cluster-creation",
            blur: request.visuals.blur,
            shadow: request.visuals.shadows.overlay,
        },
        request.resources.backdrop_blur_renderer,
        request.resources.shadow_renderer,
        &mut cluster_creation,
    )?;
    elements.extend(cluster_creation);
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
        FocusCycleRenderContext {
            state: request.overlays.focus_cycle,
            nodes: request.desktop.nodes,
            overlay_config: request.overlays.overlay_config,
            decorations: request.visuals.decorations,
            now: request.frame.target_presentation_time,
        },
        request.resources.overlay_previews,
        request.resources.node_renderer,
        request.resources.window_decoration_renderer,
        request.resources.ui_text,
    )?;
    append_compositor_overlay_blur(
        renderer,
        output,
        OverlayEffectStyle {
            output_size: output_geometry.size,
            identity: "focus-cycle",
            blur: request.visuals.blur,
            shadow: request.visuals.shadows.overlay,
        },
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
        OverlayEffectStyle {
            output_size: output_geometry.size,
            identity: "node-hover-preview",
            blur: request.visuals.blur,
            shadow: request.visuals.shadows.overlay,
        },
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
    let cluster_scene = cluster_elements(
        renderer,
        request.resources.cluster_renderer,
        ClusterElementContext {
            output,
            output_geometry,
            clusters: request.desktop.clusters,
            nodes: request.desktop.nodes,
            cameras: request.desktop.cameras,
            decorations: request.visuals.decorations,
            shadow_config: request.visuals.shadows.node,
            shadow_renderer: request.resources.shadow_renderer,
            node_grab_active: request.desktop.node_grab_active,
            node_renderer: request.resources.node_renderer,
            ui_text: request.resources.ui_text,
        },
    )?;
    let cluster_overflow = super::overlays::cluster_overflow::elements(
        renderer,
        super::overlays::cluster_overflow::OverflowElementContext {
            output,
            clusters: request.desktop.clusters,
            nodes: request.desktop.nodes,
            config: request.overlays.overlay_config,
            decorations: request.visuals.decorations,
            node_renderer: request.resources.node_renderer,
            ui_text: request.resources.ui_text,
        },
    )?;
    elements.extend(cluster_overflow);
    stack.extend(cluster_scene);
    let context = LiveWindowContext {
        space: request.desktop.space,
        output,
        output_geometry,
        cameras: request.desktop.cameras,
        clusters: request.desktop.clusters,
        nodes: request.desktop.nodes,
        target_presentation_time: request.frame.target_presentation_time,
        focused: request.desktop.focused,
        decorations: request.visuals.decorations,
        blur: request.visuals.blur,
        shadow_config: request.visuals.shadows.window,
        window_open_animations: request.desktop.window_open_animations,
        fullscreen: request.desktop.fullscreen,
        maximize: request.desktop.maximize,
        window_rules: request.desktop.window_rules,
    };
    let mut live_windows = Vec::new();
    for (stack_index, window) in request.desktop.space.elements().enumerate() {
        if !crate::wayland::window_is_on_output(window, output, primary_output) {
            continue;
        }
        let window_scene = live_window_elements(
            renderer,
            window,
            context,
            request.resources.fullscreen_textures,
            request.resources.backdrop_blur_renderer,
            request.resources.shadow_renderer,
            request.resources.window_decoration_renderer,
        )?;
        if !window_scene.elements.is_empty() {
            live_windows.push((stack_index, window_scene));
        }
    }
    // A cluster workspace is one coherent stack. Preserve its position
    // relative to non-cluster windows at the topmost member's existing Space
    // slot, then use the layout's explicit depth for member overlap.
    let cluster_anchor = live_windows
        .iter()
        .filter_map(|(stack_index, scene)| scene.cluster_depth.map(|_| *stack_index))
        .max();
    stack.extend(live_windows.into_iter().map(|(stack_index, window_scene)| {
        StackGroup {
            stack_index: window_scene
                .cluster_depth
                .and(cluster_anchor)
                .unwrap_or(stack_index),
            order: window_scene
                .cluster_depth
                .map_or(u64::MAX, |depth| depth as u64),
            elements: window_scene.elements,
        }
    }));
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
    append_background(
        renderer,
        request.resources.background_renderer,
        BackgroundSceneRequest {
            output,
            output_geometry,
            cameras: request.desktop.cameras,
            config: request.visuals.background,
            config_dir: request.visuals.background_base,
            now: request.frame.target_presentation_time,
        },
        &mut elements,
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
        OverlayEffectStyle {
            output_size: output_geometry.size,
            identity: "shell-overlay",
            blur: request.visuals.blur,
            shadow: request.visuals.shadows.overlay,
        },
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

struct BackgroundSceneRequest<'a> {
    output: &'a Output,
    output_geometry: Rectangle<i32, Logical>,
    cameras: &'a crate::presentation::camera::OutputCameras,
    config: &'a halley_config::Background,
    config_dir: Option<&'a std::path::Path>,
    now: std::time::Duration,
}

fn append_background(
    renderer: &mut GlesRenderer,
    background_renderer: &mut super::background::BackgroundRenderer,
    request: BackgroundSceneRequest<'_>,
    elements: &mut Vec<SceneElement>,
) {
    let output_name = request.output.name();
    let Some(camera) = request.cameras.get(&output_name) else {
        return;
    };
    let center = crate::presentation::camera::global_center(
        smithay::utils::Point::from((camera.center.x, camera.center.y)),
        request.output_geometry,
    );
    if let Some(background) = background_renderer.element(
        renderer,
        super::background::BackgroundRequest {
            output_name: &output_name,
            output_size: request.output_geometry.size.to_physical(1),
            camera_center: center,
            camera_size: (camera.view_size.x, camera.view_size.y),
            now: request.now,
            config: request.config,
            config_dir: request.config_dir,
        },
    ) {
        // Scene lists are front-to-back. Appending after the protocol
        // background layer makes compositor backgrounds the absolute back.
        elements.push(SceneElement::Background(background));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Physical, Rectangle};

    #[test]
    fn hover_preview_aspect_fit_centers_wide_and_tall_windows() {
        let bounds = Rectangle::<i32, Physical>::new((10, 20).into(), (200, 160).into());
        assert_eq!(
            aspect_fit_rect(bounds, 1_600, 900),
            Rectangle::new((10, 43).into(), (200, 113).into())
        );
        assert_eq!(
            aspect_fit_rect(bounds, 900, 1_600),
            Rectangle::new((65, 20).into(), (90, 160).into())
        );
    }

    #[test]
    fn focus_cycle_chrome_tracks_a_centered_thin_preview() {
        let slot = Rectangle::<i32, Physical>::new((40, 20).into(), (300, 240).into());
        let available = Rectangle::new((46, 26).into(), (288, 228).into());
        let body = aspect_fit_rect(available, 600, 1_600);
        let chrome = outset_rect(body, 6);

        assert_eq!(body.loc.x + body.size.w / 2, slot.loc.x + slot.size.w / 2);
        assert_eq!(chrome.loc.x, body.loc.x - 6);
        assert_eq!(chrome.size.w, body.size.w + 12);
        assert!(chrome.size.w < slot.size.w);
    }

    #[test]
    fn preview_rounding_uses_the_exact_configured_window_border_radius() {
        let mut decorations = halley_config::Decorations {
            border_radius_px: 7,
            ..halley_config::Decorations::default()
        };
        assert_eq!(preview_content_radius(&decorations), 7.0);
        decorations.border_radius_px = 0;
        assert_eq!(preview_content_radius(&decorations), 0.0);
    }

    #[test]
    fn resize_handles_are_reserved_for_region_selection() {
        let output = Rectangle::<i32, Logical>::new((0, 0).into(), (1920, 1080).into());
        let selection = Rectangle::<i32, Logical>::new((320, 180).into(), (1280, 720).into());

        let visuals = super::super::overlays::shell::resolve_visuals(
            &halley_config::Overlays::default(),
            &halley_config::Decorations::default(),
        );
        let region = capture_picker_elements(output, selection, true, visuals);
        let highlight = capture_picker_elements(output, selection, false, visuals);

        let handles = |elements: &[SolidColorRenderElement]| {
            elements
                .iter()
                .filter(|element| element.geometry(Scale::from(1.0)).size == (12, 12).into())
                .count()
        };
        assert_eq!(handles(&region), 4);
        assert_eq!(handles(&highlight), 0);
    }

    #[test]
    fn region_border_uses_ten_pixel_dashes_with_six_pixel_gaps() {
        let selection = Rectangle::<i32, Physical>::new((320, 180).into(), (100, 80).into());
        let strips = dashed_border_rects(selection);

        assert!(strips.contains(&Rectangle::new((320, 180).into(), (10, 2).into())));
        assert!(strips.contains(&Rectangle::new((336, 180).into(), (10, 2).into())));
        assert!(!strips.iter().any(|strip| {
            strip.loc.y == 180 && strip.loc.x < 336 && strip.loc.x + strip.size.w > 330
        }));
    }

    #[test]
    fn full_screen_highlight_border_stays_inside_the_output() {
        let output = Rectangle::<i32, Physical>::new((0, 0).into(), (1920, 1080).into());
        let border = inner_border_rects(output, 2);

        assert!(border.into_iter().all(|strip| output.contains_rect(strip)));
        assert_eq!(border[0], Rectangle::new((0, 0).into(), (1920, 2).into()));
        assert_eq!(
            border[1],
            Rectangle::new((0, 1078).into(), (1920, 2).into())
        );
    }

    #[test]
    fn secondary_window_geometry_becomes_output_local() {
        let secondary = Rectangle::<i32, Logical>::new((2560, 0).into(), (1920, 1200).into());
        let camera_center =
            crate::presentation::camera::global_center(Point::from((1060.0, 550.0)), secondary);
        let world_rect = Rectangle::<i32, Physical>::new((3520, 600).into(), (200, 100).into());

        assert_eq!(
            super::super::camera_rect(
                world_rect,
                camera_center,
                secondary.size.to_physical(1),
                0.5,
            ),
            Rectangle::new((910, 625).into(), (100, 50).into())
        );
    }

    #[test]
    fn fullscreen_rect_interpolates_position_and_size() {
        let windowed = Rectangle::new((100, 50).into(), (800, 600).into());
        let start = crate::wayland::fullscreen::FullscreenPresentation {
            progress: 0.0,
            transition_completion: 0.0,
            windowed_geometry: None,
            windowed_output_rect: None,
            fullscreen_size: (1920, 1080).into(),
        };
        let end = crate::wayland::fullscreen::FullscreenPresentation {
            progress: 1.0,
            ..start
        };
        let middle = crate::wayland::fullscreen::FullscreenPresentation {
            progress: 0.5,
            ..start
        };

        assert_eq!(start.client_rect(windowed, (1920, 1080).into()), windowed);
        assert_eq!(
            end.client_rect(windowed, (1920, 1080).into()),
            Rectangle::new((0, 0).into(), (1920, 1080).into())
        );
        assert_eq!(
            middle.client_rect(windowed, (1920, 1080).into()),
            Rectangle::new((50, 25).into(), (1360, 840).into())
        );
    }

    #[test]
    fn surface_rects_map_into_animated_client_bounds() {
        assert_eq!(
            crate::animation::map_rect(
                Rectangle::new((120, 70).into(), (200, 100).into()),
                Rectangle::new((100, 50).into(), (800, 600).into()),
                Rectangle::new((0, 0).into(), (1600, 1200).into()),
            ),
            Rectangle::new((40, 40).into(), (400, 200).into())
        );
    }

    #[test]
    fn apogee_caption_overlays_the_preview_instead_of_growing_a_footer() {
        let body = Rectangle::<i32, Physical>::new((100, 80).into(), (480, 240).into());
        let caption = apogee_caption_rect(body).expect("large preview has a caption");

        assert_eq!(caption, Rectangle::new((108, 281).into(), (464, 31).into()));
        assert!(body.contains_rect(caption));
        assert_eq!(outset_physical(body, 4).size, (488, 248).into());
    }

    #[test]
    fn apogee_caption_waits_until_node_transition_is_large_enough() {
        let node_sized = Rectangle::<i32, Physical>::new((100, 80).into(), (64, 64).into());
        assert_eq!(apogee_caption_rect(node_sized), None);
    }

    #[test]
    fn apogee_transition_never_fades_the_handoff_preview() {
        for progress in [0.0, 0.0005, 0.5, 1.0] {
            let visuals = apogee_transition_visuals(progress);
            assert_eq!(visuals.preview_alpha, 1.0);
            assert_eq!(visuals.overlay_alpha, progress);
        }
        assert_eq!(apogee_transition_visuals(0.0).chrome_alpha, 0.0);
        assert_eq!(apogee_transition_visuals(1.0).chrome_alpha, 1.0);
    }

    #[test]
    fn apogee_transition_preserves_source_stack_and_promotes_selection() {
        let back = crate::shell::apogee::Tile {
            id: halley_core::field::NodeId::new(1),
            output: "DP-1".into(),
            target: Rectangle::new((0, 0).into(), (100, 100).into()),
            source_stack_index: 0,
            source_stack_order: u64::MAX,
        };
        let middle = crate::shell::apogee::Tile {
            id: halley_core::field::NodeId::new(2),
            output: "DP-1".into(),
            target: Rectangle::new((100, 0).into(), (100, 100).into()),
            source_stack_index: 1,
            source_stack_order: 0,
        };
        let front = crate::shell::apogee::Tile {
            id: halley_core::field::NodeId::new(3),
            output: "DP-1".into(),
            target: Rectangle::new((200, 0).into(), (100, 100).into()),
            source_stack_index: 2,
            source_stack_order: u64::MAX,
        };
        let mut tiles = vec![&middle, &front, &back];

        sort_apogee_tiles(&mut tiles, None);
        assert_eq!(
            tiles.iter().map(|tile| tile.id).collect::<Vec<_>>(),
            vec![front.id, middle.id, back.id]
        );

        sort_apogee_tiles(&mut tiles, Some(middle.id));
        assert_eq!(
            tiles.iter().map(|tile| tile.id).collect::<Vec<_>>(),
            vec![middle.id, front.id, back.id]
        );
    }

    #[test]
    fn collapsed_node_keeps_front_middle_and_back_stack_depth() {
        let presented = |collapsed_index: usize, remaining: &[(usize, u64)]| {
            let mut groups = remaining
                .iter()
                .copied()
                .map(|(stack_index, order)| StackGroup {
                    stack_index,
                    order,
                    elements: Vec::new(),
                })
                .chain([
                    StackGroup {
                        stack_index: collapsed_index,
                        order: 1,
                        elements: Vec::new(),
                    },
                    StackGroup {
                        stack_index: collapsed_index,
                        order: 0,
                        elements: Vec::new(),
                    },
                ])
                .collect::<Vec<_>>();
            sort_stack_groups(&mut groups);
            groups
                .into_iter()
                .rev()
                .map(|group| (group.stack_index, group.order))
                .collect::<Vec<_>>()
        };

        // u64::MAX identifies a live window. The close snapshot (1) and
        // resulting node (0) remain on the collapsed window's side of it.
        assert_eq!(
            presented(2, &[(0, u64::MAX), (1, u64::MAX)])[..2],
            [(2, 1), (2, 0)]
        );
        assert_eq!(
            presented(1, &[(0, u64::MAX), (1, u64::MAX)])[..3],
            [(1, u64::MAX), (1, 1), (1, 0)]
        );
        assert_eq!(
            presented(0, &[(0, u64::MAX), (1, u64::MAX)])[..4],
            [(1, u64::MAX), (0, u64::MAX), (0, 1), (0, 0)]
        );
    }
}
