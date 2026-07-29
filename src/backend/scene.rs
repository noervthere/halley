use std::error::Error;

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, render_elements};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer;

use super::RenderRequest;

render_elements! {
    /// The complete front-to-back scene consumed by both presentation
    /// backends. Keeping one element type and one builder prevents nested and
    /// real-hardware sessions from drifting in z-order or visual policy.
    pub SceneElement<=GlesRenderer>;
    Cursor=crate::cursor::render::CursorRenderElement,
    Rescaled=super::rescale::RescaledElement,
    Cropped=CropRenderElement<super::rescale::RescaledElement>,
    FullscreenBlend=super::fullscreen_texture::FullscreenBlendElement,
    Node=super::node::NodeRenderElement,
    NodeLabel=super::node::LabelRenderElement,
    NodeTexture=super::node::NodeTextureElement,
    UiText=super::text::UiTextElement,
    Closing=smithay::backend::renderer::element::texture::TextureRenderElement<
        smithay::backend::renderer::gles::GlesTexture
    >,
    CaptureOverlay=super::capture_overlay::CaptureOverlayElement,
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
    request
        .cameras
        .view(&output.name())
        .ok_or_else(|| format!("output {:?} has no camera", output.name()))?;

    let mut elements = capture_overlay_elements(
        renderer,
        output,
        output_geometry,
        request.capture_overlay,
        request.decorations,
    )?;
    elements.extend(
        super::layer_surface_elements(renderer, output, Layer::Overlay)
            .into_iter()
            .map(SceneElement::Layer),
    );
    if !request
        .fullscreen
        .covers_top(request.focused, output, request.target_presentation_time)
    {
        elements.extend(
            super::layer_surface_elements(renderer, output, Layer::Top)
                .into_iter()
                .map(SceneElement::Layer),
        );
    }

    elements.extend(node_elements(
        renderer,
        request.node_renderer,
        request.ui_text,
        NodeElementContext {
            output,
            output_geometry,
            nodes: request.nodes,
            cameras: request.cameras,
            decorations: request.decorations,
            now: request.target_presentation_time,
        },
    )?);

    let mut stack = request
        .window_close_animations
        .renders_for_output(
            renderer,
            output,
            output_geometry,
            request.cameras,
            request.target_presentation_time,
        )
        .into_iter()
        .map(|closing| {
            let mut elements = Vec::new();
            if let Some(border) = closing.border {
                elements.extend(
                    super::border_strips(closing.destination, border.width, border.color)
                        .into_iter()
                        .map(SceneElement::Border),
                );
            }
            elements.push(SceneElement::Closing(closing.texture));
            StackGroup {
                stack_index: closing.stack_index,
                order: closing.order,
                elements,
            }
        })
        .collect::<Vec<_>>();
    let context = LiveWindowContext {
        space: request.space,
        output,
        output_geometry,
        cameras: request.cameras,
        target_presentation_time: request.target_presentation_time,
        focused: request.focused,
        decorations: request.decorations,
        window_open_animations: request.window_open_animations,
        fullscreen: request.fullscreen,
    };
    for (stack_index, window) in request.space.elements().enumerate() {
        if !crate::wayland::window_is_on_output(window, output, primary_output) {
            continue;
        }
        let window_elements =
            live_window_elements(renderer, window, context, request.fullscreen_textures)?;
        if !window_elements.is_empty() {
            stack.push(StackGroup {
                stack_index,
                order: u64::MAX,
                elements: window_elements,
            });
        }
    }
    stack.sort_by_key(|group| (group.stack_index, group.order));
    elements.extend(stack.into_iter().rev().flat_map(|group| group.elements));

    elements.extend(
        super::layer_surface_elements(renderer, output, Layer::Bottom)
            .into_iter()
            .map(SceneElement::Layer),
    );
    elements.extend(
        super::layer_surface_elements(renderer, output, Layer::Background)
            .into_iter()
            .map(SceneElement::Layer),
    );

    if request.show_cursor {
        let cursor = crate::cursor::render::elements(
            renderer,
            request.cursor,
            output,
            output_geometry,
            request.cursor_position,
            request.target_presentation_time,
        )?;
        // Element lists are front-to-back, so cursor surface trees belong
        // before every compositor and client element.
        elements.splice(0..0, cursor.into_iter().map(SceneElement::Cursor));
    }

    Ok(elements)
}

struct NodeElementContext<'a> {
    output: &'a Output,
    output_geometry: Rectangle<i32, Logical>,
    nodes: &'a crate::nodes::NodesState,
    cameras: &'a crate::camera::OutputCameras,
    decorations: &'a halley_config::Decorations,
    now: std::time::Duration,
}

fn node_elements(
    renderer: &mut GlesRenderer,
    node_renderer: &mut super::node::NodeRenderer,
    ui_text: &mut super::text::UiTextRenderer,
    context: NodeElementContext<'_>,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let NodeElementContext {
        output,
        output_geometry,
        nodes,
        cameras,
        decorations,
        now,
    } = context;
    let Some(camera) = cameras.get(&output.name()) else {
        return Ok(Vec::new());
    };
    let focused = decorations.border_color_focused;
    let make_solid = |rect, color| {
        SolidColorRenderElement::new(
            Id::new(),
            rect,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
    };
    let mut elements = Vec::new();

    if nodes.debug.show_focus_ring || nodes.ring_is_previewed(&output.name(), now) {
        let focus_ring = nodes.focus_ring_for_output(&output.name());
        let scale = crate::camera::scale(camera);
        let center = (
            output_geometry.size.w as f32 / 2.0 + focus_ring.offset_x * scale,
            output_geometry.size.h as f32 / 2.0 + focus_ring.offset_y * scale,
        );
        let rx = focus_ring.radius_x * scale;
        let ry = focus_ring.radius_y * scale;
        const SEGMENTS: usize = 160;
        for index in 0..SEGMENTS {
            let angle = index as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let x = center.0 + angle.cos() * rx;
            let y = center.1 + angle.sin() * ry;
            let rect = Rectangle::<i32, Physical>::new(
                (x.round() as i32 - 1, y.round() as i32 - 1).into(),
                (3, 3).into(),
            );
            elements.push(SceneElement::Border(make_solid(
                rect,
                smithay::backend::renderer::Color32F::new(focused.r, focused.g, focused.b, 0.82),
            )));
        }
    }

    let mut records = nodes
        .collapsed_on_output(&output.name())
        .collect::<Vec<_>>();
    records.sort_by_key(|record| std::cmp::Reverse(record.id.as_u64()));
    let mut label_text = Vec::new();
    let mut label_backgrounds = Vec::new();
    let mut icons = Vec::new();
    let mut markers = Vec::new();

    for record in records {
        let Some(node) = nodes.field.node(record.id) else {
            continue;
        };
        let landmark_position = nodes.landmark_position(record.id, node.pos, now);
        let center = crate::nodes::screen_from_world(landmark_position, camera, output_geometry);
        let local = center - output_geometry.loc;
        let progress = if nodes.animations_enabled
            && nodes.animation.enabled
            && nodes.animation.duration_ms > 0
        {
            (now.saturating_sub(record.collapsed_at).as_secs_f32()
                / (nodes.animation.duration_ms as f32 / 1_000.0))
                .clamp(0.0, 1.0)
        } else {
            1.0
        };
        let eased = 1.0 - (1.0 - progress).powi(3);
        let side = (crate::nodes::NODE_DIAMETER_PX * (0.24 + 0.76 * eased))
            .round()
            .max(1.0) as i32;
        let destination = Rectangle::<i32, Physical>::new(
            (local.x - side / 2, local.y - side / 2).into(),
            (side, side).into(),
        );
        let hovered = nodes.hovered == Some(record.id);
        let highlighted = hovered || nodes.focused() == Some(record.id);
        let ring = node_ring_color(nodes.config, decorations, highlighted);
        let fill = node_fill_color(nodes.config, ring);
        markers.push(SceneElement::Node(node_renderer.element(
            renderer,
            destination,
            nodes.config.shape,
            super::node::NodeStyle {
                border_rgb: ring,
                fill_rgb: fill,
                opacity: nodes.config.opacity,
                flat_fill: !matches!(
                    nodes.config.background_color,
                    halley_config::NodeBackgroundColor::Auto
                ),
            },
        )?));

        let elapsed_ms = now.saturating_sub(record.collapsed_at).as_millis() as f32;
        let icon_alpha = (((elapsed_ms - 1_000.0) / 220.0).clamp(0.0, 1.0) * nodes.config.opacity)
            .clamp(0.0, 1.0);
        let allow_real = nodes.config.show_app_icons == halley_config::NodeDisplayPolicy::Always
            || (nodes.config.show_app_icons == halley_config::NodeDisplayPolicy::Hover
                && highlighted);
        if allow_real && let Some(app_id) = record.app_id.as_deref() {
            node_renderer.request_app_icon(renderer, app_id);
        }
        if icon_alpha > 0.001 {
            let icon_side = ((crate::nodes::NODE_DIAMETER_PX * nodes.config.icon_size).round()
                as i32)
                .clamp(16, 42);
            let real_icon = allow_real
                .then_some(record.app_id.as_deref())
                .flatten()
                .and_then(|app_id| {
                    node_renderer.app_icon_element(
                        renderer,
                        app_id,
                        Rectangle::new(
                            (local.x - icon_side / 2, local.y - icon_side / 2).into(),
                            (icon_side, icon_side).into(),
                        ),
                        icon_alpha,
                    )
                });
            if let Some(icon) = real_icon {
                icons.push(SceneElement::NodeTexture(icon));
            }
        }

        let hover_mix = match nodes.config.show_labels {
            halley_config::NodeDisplayPolicy::Off => 0.0,
            halley_config::NodeDisplayPolicy::Hover => {
                nodes.label_hover_mix(record.id, highlighted)
            }
            halley_config::NodeDisplayPolicy::Always => 1.0,
        };
        let reveal = ease_in_out_cubic(hover_mix * hover_mix * hover_mix);
        let fade = ((reveal - 0.30) / 0.55).clamp(0.0, 1.0);
        if fade > 0.01 {
            let slide = ((reveal - 0.15) / 0.65).clamp(0.0, 1.0);
            let grow = ((reveal - 0.40) / 0.55).clamp(0.0, 1.0);
            let base_width =
                ((node.label.chars().count() as f32 * 9.5).round() as i32).clamp(72, 420);
            let width =
                even(((base_width as f32 * (1.0 + 0.80 * grow)).round() as i32).clamp(72, 240));
            let height = even((26.0 * (1.0 + 0.55 * grow)).round() as i32);
            let gap = (14.0 * (1.0 + 0.45 * grow)).round() as i32;
            let target_width = even(((base_width as f32 * 1.80).round() as i32).clamp(72, 240));
            let margin = 12;
            let side_gap = side / 2 + gap.max(10);
            let prefer_left = local.x + side_gap + target_width + margin > output_geometry.size.w;
            let target_x = if prefer_left {
                local.x - side_gap - width
            } else {
                local.x + side_gap
            };
            let start_x = if prefer_left {
                target_x + 44
            } else {
                target_x - 44
            };
            let label_x = (start_x as f32 + (target_x - start_x) as f32 * slide).round() as i32;
            let label_y =
                (local.y as f32 - height as f32 / 2.0 + (1.0 - slide) * 10.0).round() as i32;
            let label = Rectangle::<i32, Physical>::new(
                (
                    label_x.clamp(
                        margin,
                        (output_geometry.size.w - width - margin).max(margin),
                    ),
                    label_y.clamp(
                        margin,
                        (output_geometry.size.h - height - margin).max(margin),
                    ),
                )
                    .into(),
                (width, height).into(),
            );
            let label_fill = label_fill_color(fill, ring);
            label_backgrounds.push(SceneElement::NodeLabel(node_renderer.label_element(
                renderer,
                label,
                nodes.config.label_shape,
                label_fill,
                1.0,
            )?));

            let text_rgb = contrast_text_rgb(label_fill);
            let (text, text_size) =
                fit_node_label(renderer, ui_text, &node.label, text_rgb, width - 20)?;
            if !text.is_empty()
                && let Some(prepared) = ui_text.element(
                    renderer,
                    (
                        label.loc.x + (width - text_size.w).max(0) / 2,
                        label.loc.y + (height - text_size.h).max(0) / 2,
                    )
                        .into(),
                    &text,
                    2,
                    text_rgb,
                    0.94 * eased * fade,
                )?
            {
                label_text.push(SceneElement::UiText(prepared.element));
            }
        }
    }
    elements.extend(label_text);
    elements.extend(label_backgrounds);
    elements.extend(icons);
    elements.extend(markers);
    Ok(elements)
}

fn fit_node_label(
    renderer: &mut GlesRenderer,
    ui_text: &mut super::text::UiTextRenderer,
    source: &str,
    rgb: [u8; 3],
    available: i32,
) -> Result<(String, smithay::utils::Size<i32, smithay::utils::Buffer>), Box<dyn Error>> {
    let text = source.trim();
    let Some(size) = ui_text.measure(renderer, text, 2, rgb)? else {
        return Ok((String::new(), (0, 0).into()));
    };
    if size.w <= available {
        return Ok((text.to_string(), size));
    }

    let characters = text.chars().collect::<Vec<_>>();
    for keep in (0..characters.len()).rev() {
        let candidate = characters[..keep]
            .iter()
            .copied()
            .chain(std::iter::once('…'))
            .collect::<String>();
        let Some(size) = ui_text.measure(renderer, &candidate, 2, rgb)? else {
            continue;
        };
        if size.w <= available {
            return Ok((candidate, size));
        }
    }
    Ok((String::new(), (0, 0).into()))
}

fn node_ring_color(
    config: halley_config::Nodes,
    decorations: &halley_config::Decorations,
    hovered: bool,
) -> (f32, f32, f32) {
    let policy = if hovered {
        config.border_color_hover
    } else {
        config.border_color_inactive
    };
    let color = match policy {
        halley_config::NodeBorderColor::UseWindowActive
        | halley_config::NodeBorderColor::UseWindowSecondaryActive => {
            decorations.border_color_focused
        }
        halley_config::NodeBorderColor::UseWindowInactive
        | halley_config::NodeBorderColor::UseWindowSecondaryInactive => {
            decorations.border_color_unfocused
        }
    };
    (color.r, color.g, color.b)
}

fn node_fill_color(config: halley_config::Nodes, ring: (f32, f32, f32)) -> (f32, f32, f32) {
    match config.background_color {
        halley_config::NodeBackgroundColor::Auto => (
            0.94 * 0.86 + ring.0 * 0.14,
            0.96 * 0.86 + ring.1 * 0.14,
            0.985 * 0.86 + ring.2 * 0.14,
        ),
        halley_config::NodeBackgroundColor::Light => (0.92, 0.95, 0.98),
        halley_config::NodeBackgroundColor::Dark => (0.15, 0.18, 0.22),
        halley_config::NodeBackgroundColor::Fixed(r, g, b) => (r, g, b),
    }
}

fn label_fill_color(fill: (f32, f32, f32), ring: (f32, f32, f32)) -> (f32, f32, f32) {
    (
        fill.0 * 0.90 + ring.0 * 0.10,
        fill.1 * 0.90 + ring.1 * 0.10,
        fill.2 * 0.90 + ring.2 * 0.10,
    )
}

fn contrast_text_rgb(fill: (f32, f32, f32)) -> [u8; 3] {
    let luminance = fill.0 * 0.2126 + fill.1 * 0.7152 + fill.2 * 0.0722;
    let rgb = if luminance >= 0.45 {
        (0.08, 0.10, 0.12)
    } else {
        (0.96, 0.98, 1.0)
    };
    [
        (rgb.0 * 255.0) as u8,
        (rgb.1 * 255.0) as u8,
        (rgb.2 * 255.0) as u8,
    ]
}

fn ease_in_out_cubic(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value < 0.5 {
        4.0 * value * value * value
    } else {
        1.0 - (-2.0 * value + 2.0).powi(3) / 2.0
    }
}

fn even(value: i32) -> i32 {
    (value + 1) & !1
}

struct StackGroup {
    stack_index: usize,
    order: u64,
    elements: Vec<SceneElement>,
}

#[derive(Clone, Copy)]
struct LiveWindowContext<'a> {
    space: &'a smithay::desktop::Space<smithay::desktop::Window>,
    output: &'a Output,
    output_geometry: Rectangle<i32, Logical>,
    cameras: &'a crate::camera::OutputCameras,
    target_presentation_time: std::time::Duration,
    focused: Option<&'a smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    decorations: &'a halley_config::Decorations,
    window_open_animations: &'a crate::animation::WindowOpenAnimations,
    fullscreen: &'a crate::wayland::fullscreen::FullscreenManager,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowVisualState {
    pub(crate) source_geometry: Rectangle<i32, Logical>,
    pub(crate) camera_rect: Rectangle<i32, Physical>,
    pub(crate) presentation_rect: Rectangle<i32, Physical>,
    pub(crate) animated_rect: Rectangle<i32, Physical>,
    pub(crate) opening_alpha: f32,
    pub(crate) opening_is_animating: bool,
    pub(crate) fullscreen: Option<crate::wayland::fullscreen::FullscreenPresentation>,
    pub(crate) camera_center: Point<f32, Physical>,
    pub(crate) zoom_scale: f32,
}

pub(crate) fn window_visual_state(
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    cameras: &crate::camera::OutputCameras,
    window: &smithay::desktop::Window,
    output: &Output,
    window_open_animations: &crate::animation::WindowOpenAnimations,
    fullscreen: &crate::wayland::fullscreen::FullscreenManager,
    now: std::time::Duration,
) -> Option<WindowVisualState> {
    let output_geometry = space.output_geometry(output)?;
    let output_size = output_geometry.size.to_physical(1);
    let view = cameras.view(&output.name())?;
    let camera_center = crate::camera::global_center(view.center, output_geometry);
    let source_geometry = space.element_geometry(window)?;
    let window_surface = window.wl_surface()?;
    let camera_rect = super::camera_rect(
        source_geometry.to_physical(1),
        camera_center,
        output_size,
        view.scale,
    );
    let opening_is_animating = window_open_animations.is_animating(window_surface.as_ref(), now);
    let opening_visual = window_open_animations
        .visual(window_surface.as_ref(), now, camera_rect)
        .unwrap_or_default();
    let fullscreen = fullscreen.presentation(window_surface.as_ref(), output, now);
    let presentation_rect = fullscreen
        .map(|presentation| {
            let windowed = presentation
                .windowed_geometry
                .map(|geometry| {
                    super::camera_rect(
                        geometry.to_physical(1),
                        camera_center,
                        output_size,
                        view.scale,
                    )
                })
                .unwrap_or_else(|| presentation.fullscreen_rect(output_size));
            presentation.client_rect(windowed, output_size)
        })
        .unwrap_or(camera_rect);
    let animated_rect = opening_visual.transform_rect(presentation_rect, presentation_rect);

    Some(WindowVisualState {
        source_geometry,
        camera_rect,
        presentation_rect,
        animated_rect,
        opening_alpha: opening_visual.alpha(),
        opening_is_animating,
        fullscreen,
        camera_center,
        zoom_scale: view.scale,
    })
}

fn live_window_elements(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    context: LiveWindowContext<'_>,
    fullscreen_textures: &mut super::fullscreen_texture::FullscreenTextureTransitions,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let Some(location) = context.space.element_location(window) else {
        return Ok(Vec::new());
    };
    let Some(window_surface) = window.wl_surface() else {
        return Ok(Vec::new());
    };
    let Some(visual) = window_visual_state(
        context.space,
        context.cameras,
        window,
        context.output,
        context.window_open_animations,
        context.fullscreen,
        context.target_presentation_time,
    ) else {
        return Ok(Vec::new());
    };
    if visual.animated_rect.size.w == 0 || visual.animated_rect.size.h == 0 {
        return Ok(Vec::new());
    }

    let mut elements = Vec::new();
    let surface_location = super::window_surface_location(location, window.geometry());
    let (popup_elements, surface_elements) =
        super::window_surface_elements(renderer, window, surface_location, visual.opening_alpha);
    elements.extend(popup_elements.into_iter().map(|surface_element| {
        let native_geometry = surface_element.geometry(Scale::from(1.0));
        let destination = if visual.fullscreen.is_some() {
            let destination = map_rect(
                native_geometry,
                visual.source_geometry.to_physical(1),
                visual.presentation_rect,
            );
            crate::animation::map_rect(destination, visual.presentation_rect, visual.animated_rect)
        } else {
            let final_destination = super::camera_rect(
                native_geometry,
                visual.camera_center,
                context.output_geometry.size.to_physical(1),
                visual.zoom_scale,
            );
            crate::animation::map_rect(final_destination, visual.camera_rect, visual.animated_rect)
        };
        SceneElement::Rescaled(super::rescale::RescaledElement::new(
            surface_element,
            destination,
        ))
    }));
    let fullscreen_blend = if let Some(presentation) = visual.fullscreen {
        match fullscreen_textures.blend_element(
            renderer,
            window,
            visual.animated_rect,
            presentation.transition_completion,
            visual.opening_alpha,
        ) {
            Ok(blend) => blend,
            Err(err) => {
                eventline::warn!("fullscreen: failed to blend window textures: {err}");
                None
            }
        }
    } else {
        None
    };
    if let Some(blend) = fullscreen_blend {
        elements.push(SceneElement::FullscreenBlend(blend));
    } else {
        elements.extend(surface_elements.into_iter().filter_map(|surface_element| {
            let native_geometry = surface_element.geometry(Scale::from(1.0));
            let destination = if visual.fullscreen.is_some() {
                let destination = map_rect(
                    native_geometry,
                    visual.source_geometry.to_physical(1),
                    visual.presentation_rect,
                );
                crate::animation::map_rect(
                    destination,
                    visual.presentation_rect,
                    visual.animated_rect,
                )
            } else {
                let final_destination = super::camera_rect(
                    native_geometry,
                    visual.camera_center,
                    context.output_geometry.size.to_physical(1),
                    visual.zoom_scale,
                );
                crate::animation::map_rect(
                    final_destination,
                    visual.camera_rect,
                    visual.animated_rect,
                )
            };
            let element = super::rescale::RescaledElement::new(surface_element, destination);
            CropRenderElement::from_element(element, 1.0, visual.animated_rect)
                .map(SceneElement::Cropped)
        }));
    }

    let is_focused = Some(window_surface.as_ref()) == context.focused;
    let border_color = super::window_border_color(context.decorations, is_focused)
        * visual.opening_alpha
        * visual
            .fullscreen
            .map(|presentation| (1.0 - presentation.progress) as f32)
            .unwrap_or(1.0);
    let border_width = ((context.decorations.border_width_px as f64 * visual.zoom_scale as f64)
        .round() as i32)
        .max(1);
    if !crate::xwayland::is_override_redirect(window) {
        elements.extend(
            super::border_strips(visual.animated_rect, border_width, border_color)
                .into_iter()
                .map(SceneElement::Border),
        );
    }
    if let Some(backdrop_alpha) =
        fullscreen_backdrop_alpha(visual.fullscreen, visual.opening_is_animating)
    {
        elements.push(SceneElement::Border(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new((0, 0).into(), context.output_geometry.size.to_physical(1)),
            CommitCounter::default(),
            smithay::backend::renderer::Color32F::new(0.0, 0.0, 0.0, backdrop_alpha),
            Kind::Unspecified,
        )));
    }
    Ok(elements)
}

fn fullscreen_backdrop_alpha(
    fullscreen: Option<crate::wayland::fullscreen::FullscreenPresentation>,
    opening_is_animating: bool,
) -> Option<f32> {
    // Opening geometry owns the initial presentation. A separate fullscreen
    // plane would expose transient client request churn as an output-wide flash.
    if opening_is_animating {
        return None;
    }
    fullscreen.map(|presentation| presentation.progress.clamp(0.0, 1.0) as f32)
}

fn capture_overlay_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    overlay: crate::capture::CaptureOverlay<'_>,
    decorations: &halley_config::Decorations,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    match overlay {
        crate::capture::CaptureOverlay::None => Ok(Vec::new()),
        crate::capture::CaptureOverlay::Region(region) => {
            Ok(capture_picker_elements(output_geometry, region, true)
                .into_iter()
                .rev()
                .map(SceneElement::Border)
                .collect())
        }
        crate::capture::CaptureOverlay::Highlight(region) => {
            Ok(capture_picker_elements(output_geometry, region, false)
                .into_iter()
                .rev()
                .map(SceneElement::Border)
                .collect())
        }
        crate::capture::CaptureOverlay::Menu {
            output_name,
            selected,
            hovered,
            window_available,
        } if output.name() == output_name => Ok(super::capture_overlay::menu_elements(
            renderer,
            output_geometry,
            selected,
            hovered,
            window_available,
            super::window_border_color(decorations, true),
        )?
        .into_iter()
        .rev()
        .map(SceneElement::CaptureOverlay)
        .collect()),
        crate::capture::CaptureOverlay::Menu { .. } => Ok(Vec::new()),
    }
}

fn capture_picker_elements(
    output: Rectangle<i32, Logical>,
    selection: Rectangle<i32, Logical>,
    region_style: bool,
) -> Vec<SolidColorRenderElement> {
    let output_local = Rectangle::<i32, Physical>::from_size(output.size.to_physical(1));
    let selected = output.intersection(selection).map(|intersection| {
        Rectangle::<i32, Physical>::new(
            (intersection.loc - output.loc).to_physical(1),
            intersection.size.to_physical(1),
        )
    });
    let dim = smithay::backend::renderer::Color32F::new(0.0, 0.0, 0.0, 0.48);
    let white = smithay::backend::renderer::Color32F::new(1.0, 1.0, 1.0, 1.0);
    let make = |geometry, color| {
        SolidColorRenderElement::new(
            Id::new(),
            geometry,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
    };

    let Some(selected) = selected else {
        return vec![make(output_local, dim)];
    };
    let mut elements = Vec::with_capacity(12);
    let right = selected.loc.x + selected.size.w;
    let bottom = selected.loc.y + selected.size.h;
    for rect in [
        Rectangle::new(
            (0, 0).into(),
            (output_local.size.w, selected.loc.y.max(0)).into(),
        ),
        Rectangle::new(
            (0, bottom).into(),
            (output_local.size.w, (output_local.size.h - bottom).max(0)).into(),
        ),
        Rectangle::new(
            (0, selected.loc.y).into(),
            (selected.loc.x.max(0), selected.size.h).into(),
        ),
        Rectangle::new(
            (right, selected.loc.y).into(),
            ((output_local.size.w - right).max(0), selected.size.h).into(),
        ),
    ] {
        if rect.size.w > 0 && rect.size.h > 0 {
            elements.push(make(rect, dim));
        }
    }
    if region_style {
        elements.extend(
            dashed_border_rects(selected)
                .into_iter()
                .map(|rect| make(rect, white)),
        );
        let handle_size = 12;
        for point in [
            selected.loc,
            (right, selected.loc.y).into(),
            (selected.loc.x, bottom).into(),
            (right, bottom).into(),
        ] {
            elements.push(make(
                Rectangle::new(
                    (point.x - handle_size / 2, point.y - handle_size / 2).into(),
                    (handle_size, handle_size).into(),
                ),
                white,
            ));
        }
    } else {
        elements.extend(
            inner_border_rects(selected, 2)
                .into_iter()
                .map(|rect| make(rect, white)),
        );
    }
    elements
}

fn inner_border_rects(rect: Rectangle<i32, Physical>, width: i32) -> [Rectangle<i32, Physical>; 4] {
    let width = width.max(0).min(rect.size.w).min(rect.size.h);
    let right = rect.loc.x + rect.size.w;
    let bottom = rect.loc.y + rect.size.h;
    [
        Rectangle::new(rect.loc, (rect.size.w, width).into()),
        Rectangle::new(
            (rect.loc.x, bottom - width).into(),
            (rect.size.w, width).into(),
        ),
        Rectangle::new(rect.loc, (width, rect.size.h).into()),
        Rectangle::new(
            (right - width, rect.loc.y).into(),
            (width, rect.size.h).into(),
        ),
    ]
}

fn dashed_border_rects(rect: Rectangle<i32, Physical>) -> Vec<Rectangle<i32, Physical>> {
    const THICKNESS: i32 = 2;
    const DASH_LENGTH: i32 = 10;
    const GAP_LENGTH: i32 = 6;

    let right = rect.loc.x + rect.size.w;
    let bottom = rect.loc.y + rect.size.h;
    let mut strips = Vec::new();

    let mut x = rect.loc.x;
    while x < right {
        let length = (right - x).min(DASH_LENGTH);
        strips.push(Rectangle::new(
            (x, rect.loc.y).into(),
            (length, THICKNESS).into(),
        ));
        strips.push(Rectangle::new(
            (x, bottom - THICKNESS).into(),
            (length, THICKNESS).into(),
        ));
        x += DASH_LENGTH + GAP_LENGTH;
    }

    let mut y = rect.loc.y;
    while y < bottom {
        let length = (bottom - y).min(DASH_LENGTH);
        strips.push(Rectangle::new(
            (rect.loc.x, y).into(),
            (THICKNESS, length).into(),
        ));
        strips.push(Rectangle::new(
            (right - THICKNESS, y).into(),
            (THICKNESS, length).into(),
        ));
        y += DASH_LENGTH + GAP_LENGTH;
    }

    strips
}

fn map_rect(
    rect: Rectangle<i32, Physical>,
    source: Rectangle<i32, Physical>,
    destination: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    if source.size.w == 0 || source.size.h == 0 {
        return destination;
    }
    let scale_x = f64::from(destination.size.w) / f64::from(source.size.w);
    let scale_y = f64::from(destination.size.h) / f64::from(source.size.h);
    let left = f64::from(destination.loc.x) + f64::from(rect.loc.x - source.loc.x) * scale_x;
    let top = f64::from(destination.loc.y) + f64::from(rect.loc.y - source.loc.y) * scale_y;
    let right = left + f64::from(rect.size.w) * scale_x;
    let bottom = top + f64::from(rect.size.h) * scale_y;
    Rectangle::new(
        (left.round() as i32, top.round() as i32).into(),
        (
            (right - left).round().max(0.0) as i32,
            (bottom - top).round().max(0.0) as i32,
        )
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Physical, Rectangle};

    #[test]
    fn resize_handles_are_reserved_for_region_selection() {
        let output = Rectangle::<i32, Logical>::new((0, 0).into(), (1920, 1080).into());
        let selection = Rectangle::<i32, Logical>::new((320, 180).into(), (1280, 720).into());

        let region = capture_picker_elements(output, selection, true);
        let highlight = capture_picker_elements(output, selection, false);

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
        let camera_center = crate::camera::global_center(Point::from((1060.0, 550.0)), secondary);
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
            map_rect(
                Rectangle::new((120, 70).into(), (200, 100).into()),
                Rectangle::new((100, 50).into(), (800, 600).into()),
                Rectangle::new((0, 0).into(), (1600, 1200).into()),
            ),
            Rectangle::new((40, 40).into(), (400, 200).into())
        );
    }

    fn fullscreen_presentation(
        progress: f64,
    ) -> crate::wayland::fullscreen::FullscreenPresentation {
        crate::wayland::fullscreen::FullscreenPresentation {
            progress,
            transition_completion: progress,
            windowed_geometry: None,
            fullscreen_size: (1920, 1080).into(),
        }
    }

    #[test]
    fn opening_fullscreen_does_not_create_an_independent_backdrop() {
        let startup_churn = [
            Some(fullscreen_presentation(1.0)),
            None,
            Some(fullscreen_presentation(1.0)),
        ];

        assert!(
            startup_churn
                .into_iter()
                .all(|fullscreen| fullscreen_backdrop_alpha(fullscreen, true).is_none())
        );
    }

    #[test]
    fn fullscreen_without_an_active_opening_keeps_normal_backdrop_policy() {
        assert_eq!(
            fullscreen_backdrop_alpha(Some(fullscreen_presentation(1.0)), false),
            Some(1.0)
        );
        assert_eq!(
            fullscreen_backdrop_alpha(Some(fullscreen_presentation(0.4)), false),
            Some(0.4)
        );
        assert_eq!(fullscreen_backdrop_alpha(None, false), None);
    }
}
