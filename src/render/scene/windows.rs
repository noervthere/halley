use super::*;

const JOIN_READY_TINT_ALPHA: f32 = 0.10;

pub(super) struct StackGroup {
    pub(super) stack_index: usize,
    pub(super) order: u64,
    pub(super) elements: Vec<SceneElement>,
}

pub(super) fn sort_stack_groups(groups: &mut [StackGroup]) {
    groups.sort_by_key(|group| (group.stack_index, group.order));
}

pub(super) struct LiveWindowScene {
    pub(super) elements: Vec<SceneElement>,
    pub(super) cluster_depth: Option<usize>,
    pub(super) cluster_floating: bool,
    pub(super) cluster_exclusive: bool,
}

#[derive(Clone, Copy)]
pub(super) struct LiveWindowContext<'a> {
    pub(super) space: &'a smithay::desktop::Space<smithay::desktop::Window>,
    pub(super) output: &'a Output,
    pub(super) output_geometry: Rectangle<i32, Logical>,
    pub(super) cameras: &'a crate::presentation::camera::OutputCameras,
    pub(super) clusters: &'a crate::clusters::ClusterSystem,
    pub(super) nodes: &'a crate::nodes::NodesState,
    pub(super) target_presentation_time: std::time::Duration,
    pub(super) focused:
        Option<&'a smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    pub(super) decorations: &'a halley_config::Decorations,
    pub(super) font: &'a halley_config::Font,
    pub(super) blur: halley_config::Blur,
    pub(super) shadow_config: halley_config::ShadowLayer,
    pub(super) window_open_animations: &'a crate::animation::WindowOpenAnimations,
    pub(super) fullscreen: &'a crate::wayland::fullscreen::FullscreenManager,
    pub(super) maximize: &'a crate::presentation::maximize::FieldMaximizeManager,
    pub(super) window_rules: &'a crate::window::rules::WindowRulesState,
    pub(super) cluster_presentation_override: Option<crate::clusters::WindowPresentation>,
    pub(super) instance_identity: Option<&'static str>,
    pub(super) titlebar_hovered: Option<&'a crate::titlebar::ButtonTarget>,
    pub(super) titlebar_pressed: Option<&'a crate::titlebar::ButtonTarget>,
}

/// Crossfade progress past which the captured textures stop contributing.
///
/// A spring's settle time is numeric, not perceptual: `spring_duration` hunts
/// for a displacement below 1e-4, which for the default fullscreen spring is
/// roughly 230ms after the motion has visually stopped. Holding the offscreen
/// texture path open across that tail puts the swap back to live surfaces well
/// clear of the animation, where it reads as a discrete flash rather than part
/// of it. Past this point the previous texture contributes under one part in
/// two hundred, so retiring the blend early is not visible - but the swap now
/// happens under the last pixels of motion, which is.
const CROSSFADE_COMPLETE: f64 = 0.995;

fn compositor_chrome_visible(logical_fullscreen: bool, x11_fullscreen: bool) -> bool {
    !logical_fullscreen && !x11_fullscreen
}

pub(super) fn live_window_elements(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    context: LiveWindowContext<'_>,
    fullscreen_textures: &mut crate::render::fullscreen_texture::FullscreenTextureTransitions,
    backdrop_blur_renderer: &mut crate::render::effects::backdrop_blur::BackdropBlurRenderer,
    shadow_renderer: &mut crate::render::effects::shadow::ShadowRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    titlebar_renderer: &mut crate::render::titlebar::TitlebarRenderer,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
) -> Result<LiveWindowScene, Box<dyn Error>> {
    let Some(location) = context.space.element_location(window) else {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: None,
            cluster_floating: false,
            cluster_exclusive: false,
        });
    };
    let Some(window_surface) = window.wl_surface() else {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: None,
            cluster_floating: false,
            cluster_exclusive: false,
        });
    };
    let join_ready = context
        .nodes
        .id_for_surface(window_surface.as_ref())
        .is_some_and(|member| {
            context
                .clusters
                .join_ready_for(member, &context.output.name())
        });
    let Some(visual) = window_visual_state_with_cluster_presentation(
        context.space,
        context.cameras,
        Some(context.clusters),
        Some(context.nodes),
        window,
        context.output,
        context.window_open_animations,
        context.fullscreen,
        context.maximize,
        context.target_presentation_time,
        context.cluster_presentation_override,
    ) else {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: None,
            cluster_floating: false,
            cluster_exclusive: false,
        });
    };
    if visual.animated_rect.size.w == 0 || visual.animated_rect.size.h == 0 {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: visual.cluster_depth,
            cluster_floating: visual.cluster_floating,
            cluster_exclusive: visual.cluster_exclusive,
        });
    }

    let mut elements = Vec::new();
    let managed = !crate::xwayland::is_override_redirect(window);
    let rule_opacity = if managed {
        context.window_rules.opacity(window_surface.as_ref())
    } else {
        1.0
    };
    let alpha = visual.opening_alpha * rule_opacity;
    // X11 clients can churn fullscreen requests while changing video modes.
    // Keep their advertised EWMH state as a render-time backstop so a missing
    // or temporarily retired presentation entry cannot expose compositor
    // chrome around an otherwise-fullscreen game.
    let chrome_visible = compositor_chrome_visible(
        context
            .fullscreen
            .suppresses_chrome(window_surface.as_ref()),
        crate::xwayland::is_fullscreen(window),
    );
    let chrome_alpha = if chrome_visible { alpha } else { 0.0 };
    let server_titlebar = managed
        && chrome_visible
        && crate::titlebar::uses_server_titlebar(window, &context.decorations.titlebars);
    let opening_scale_y = if visual.presentation_rect.size.h > 0 {
        visual.animated_rect.size.h as f32 / visual.presentation_rect.size.h as f32
    } else {
        1.0
    };
    let decoration_scale = visual.zoom_scale * opening_scale_y.max(0.0);
    let titlebar_height = crate::render::window_decoration::scaled_metric(
        crate::titlebar::effective_height(&context.decorations.titlebars, context.font.size),
        decoration_scale,
    );
    let content_radius = if chrome_visible {
        crate::render::window_decoration::scaled_metric(
            context.decorations.border_radius_px,
            visual.zoom_scale,
        ) as f32
    } else {
        0.0
    };
    let rounded = managed && content_radius > 0.0;
    let rounded_available = rounded && window_decoration_renderer.available(renderer);
    if join_ready {
        let tint_alpha = alpha * JOIN_READY_TINT_ALPHA;
        let focused = context.decorations.border_color_focused;
        let tint_color =
            smithay::backend::renderer::Color32F::new(focused.r, focused.g, focused.b, 1.0);
        let radii = if rounded_available && server_titlebar {
            crate::render::window_decoration::CornerRadii::bottom(content_radius)
        } else if rounded_available {
            crate::render::window_decoration::CornerRadii::all(content_radius)
        } else {
            crate::render::window_decoration::CornerRadii::default()
        };
        if let Some(tint) = window_decoration_renderer.tint_element_with_radii(
            renderer,
            visual.animated_rect,
            radii,
            tint_color,
            tint_alpha,
        ) {
            elements.push(SceneElement::RoundedTexture(tint));
        } else {
            elements.push(SceneElement::Border(SolidColorRenderElement::new(
                Id::new(),
                visual.animated_rect,
                CommitCounter::default(),
                tint_color * tint_alpha,
                Kind::Unspecified,
            )));
        }
    }
    let surface_location = crate::render::window_surface_location(location, window.geometry());
    let (popup_elements, surface_elements) =
        crate::render::window_surface_elements(renderer, window, surface_location, alpha);
    elements.extend(popup_elements.into_iter().map(|surface_element| {
        let native_geometry = surface_element.geometry(Scale::from(1.0));
        let destination = if visual.maps_from_source() {
            let destination = crate::animation::map_rect(
                native_geometry,
                visual.source_geometry.to_physical(1),
                visual.presentation_rect,
            );
            crate::animation::map_rect(destination, visual.presentation_rect, visual.animated_rect)
        } else {
            let final_destination = crate::render::camera_rect(
                native_geometry,
                visual.camera_center,
                context.output_geometry.size.to_physical(1),
                visual.zoom_scale,
            );
            crate::animation::map_rect(final_destination, visual.camera_rect, visual.animated_rect)
        };
        SceneElement::Rescaled(crate::render::rescale::RescaledElement::new(
            surface_element,
            destination,
        ))
    }));
    if server_titlebar && chrome_alpha > 0.0 {
        append_titlebar_elements(
            renderer,
            window,
            visual.animated_rect,
            titlebar_height,
            crate::render::window_decoration::scaled_metric(
                context.decorations.border_width_px,
                decoration_scale,
            ),
            crate::render::window_decoration::scaled_metric(
                context.decorations.titlebars.radius_px,
                decoration_scale,
            ) as f32,
            Some(window_surface.as_ref()) == context.focused,
            chrome_alpha,
            context.decorations,
            context.titlebar_hovered,
            context.titlebar_pressed,
            titlebar_renderer,
            window_decoration_renderer,
            node_renderer,
            ui_text,
            &mut elements,
        )?;
    }
    let texture_transition_completion = visual
        .fullscreen
        .map(|presentation| presentation.transition_completion)
        .or_else(|| {
            visual
                .maximize
                .map(|presentation| presentation.transition_completion)
        })
        .filter(|completion| *completion < CROSSFADE_COMPLETE);
    let texture_blend = if let Some(completion) = texture_transition_completion {
        match fullscreen_textures.blend_element(
            renderer,
            window,
            visual.animated_rect,
            completion,
            alpha,
            if rounded_available && server_titlebar {
                crate::render::window_decoration::CornerRadii::bottom(content_radius)
            } else if rounded_available {
                crate::render::window_decoration::CornerRadii::all(content_radius)
            } else {
                crate::render::window_decoration::CornerRadii::default()
            },
        ) {
            Ok(blend) => blend,
            Err(err) => {
                eventline::warn!("window transition: failed to blend textures: {err}");
                None
            }
        }
    } else {
        None
    };
    if let Some(blend) = texture_blend {
        elements.push(SceneElement::FullscreenBlend(blend));
    } else {
        for surface_element in surface_elements {
            let native_geometry = surface_element.geometry(Scale::from(1.0));
            let destination = if visual.maps_from_source() {
                let destination = crate::animation::map_rect(
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
                let final_destination = crate::render::camera_rect(
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
            if rounded_available {
                let radii = if server_titlebar {
                    crate::render::window_decoration::CornerRadii::bottom(content_radius)
                } else {
                    crate::render::window_decoration::CornerRadii::all(content_radius)
                };
                let element = window_decoration_renderer
                    .surface_element_with_radii(
                        renderer,
                        surface_element,
                        destination,
                        visual.animated_rect,
                        radii,
                    )
                    .expect("rounded resources were checked above");
                if let Some(element) =
                    CropRenderElement::from_element(element, 1.0, visual.animated_rect)
                {
                    elements.push(SceneElement::RoundedCropped(element));
                }
                continue;
            }
            let element =
                crate::render::rescale::RescaledElement::new(surface_element, destination);
            if let Some(element) =
                CropRenderElement::from_element(element, 1.0, visual.animated_rect)
            {
                elements.push(SceneElement::Cropped(element));
            }
        }
    }

    let surface_size =
        with_renderer_surface_state(window_surface.as_ref(), |state| state.surface_size())
            .flatten();
    if let Some(surface_size) = surface_size {
        let output_bounds =
            Rectangle::<i32, Physical>::from_size(context.output_geometry.size.to_physical(1));
        let mut requested =
            crate::wayland::background_effect::blur_rects(window_surface.as_ref(), surface_size);
        let global_blur_allowed = context
            .fullscreen
            .allows_global_blur(window_surface.as_ref());
        let policy_blur = managed
            && halley_config::window_blur_enabled(
                context.blur,
                context.window_rules.blur(window_surface.as_ref()),
                rule_opacity,
                !global_blur_allowed,
            );
        if requested.is_empty() && policy_blur {
            requested.push(Rectangle::from_size(surface_size));
        }
        let patches = requested
            .into_iter()
            .filter_map(|rect| {
                let native = Rectangle::<i32, Physical>::new(
                    surface_location + rect.loc.to_physical(1),
                    rect.size.to_physical(1),
                );
                let destination = if visual.maps_from_source() {
                    let destination = crate::animation::map_rect(
                        native,
                        visual.source_geometry.to_physical(1),
                        visual.presentation_rect,
                    );
                    crate::animation::map_rect(
                        destination,
                        visual.presentation_rect,
                        visual.animated_rect,
                    )
                } else {
                    let final_destination = crate::render::camera_rect(
                        native,
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
                destination
                    .intersection(output_bounds)
                    .and_then(|rect| rect.intersection(visual.animated_rect))
                    .map(|rect| crate::render::effects::backdrop_blur::BlurPatch {
                        rect,
                        radius: 0.0,
                        alpha,
                        clip: rounded_available.then_some((
                            visual.animated_rect,
                            if server_titlebar {
                                crate::render::window_decoration::CornerRadii::bottom(
                                    content_radius,
                                )
                            } else {
                                crate::render::window_decoration::CornerRadii::all(content_radius)
                            },
                        )),
                    })
            })
            .collect::<Vec<_>>();
        if let Some(blur) = backdrop_blur_renderer.blur_element(
            renderer,
            &context.output.name(),
            crate::render::effects::backdrop_blur::BlurIdentity::Window {
                surface: Id::from_wayland_resource(window_surface.as_ref()),
                instance: context.instance_identity.unwrap_or("canonical").to_string(),
            },
            context.output_geometry.size,
            patches,
            context.blur,
        )? {
            elements.push(SceneElement::BackdropBlur(blur));
        }
    }

    let is_focused = Some(window_surface.as_ref()) == context.focused;
    let border_color = crate::render::window_border_color(context.decorations, is_focused);
    let border_width = crate::render::window_decoration::scaled_metric(
        context.decorations.border_width_px,
        visual.zoom_scale,
    );
    if managed && border_width > 0 && chrome_alpha > 0.0 {
        if rounded_available
            && let Some(border) = if server_titlebar {
                window_decoration_renderer.body_border_element(
                    renderer,
                    visual.animated_rect,
                    border_width,
                    content_radius,
                    border_color,
                    chrome_alpha,
                )
            } else {
                window_decoration_renderer.border_element(
                    renderer,
                    visual.animated_rect,
                    border_width,
                    content_radius,
                    border_color,
                    chrome_alpha,
                )
            }
        {
            elements.push(SceneElement::WindowBorder(border));
        } else {
            let strips: Vec<_> = if server_titlebar {
                crate::render::body_border_strips(
                    visual.animated_rect,
                    border_width,
                    border_color * chrome_alpha,
                )
                .into_iter()
                .collect()
            } else {
                crate::render::border_strips(
                    visual.animated_rect,
                    border_width,
                    border_color * chrome_alpha,
                )
                .into_iter()
                .collect()
            };
            elements.extend(strips.into_iter().map(SceneElement::Border));
        }
    }
    if managed && chrome_alpha > 0.0 {
        let border_outset = border_width.max(0);
        let caster = if server_titlebar {
            crate::titlebar::DecorationLayout::new(
                visual.animated_rect,
                border_outset,
                titlebar_height,
                &context.decorations.titlebars,
            )
            .outer
        } else {
            Rectangle::new(
                (
                    visual.animated_rect.loc.x - border_outset,
                    visual.animated_rect.loc.y - border_outset,
                )
                    .into(),
                (
                    (visual.animated_rect.size.w + border_outset * 2).max(1),
                    (visual.animated_rect.size.h + border_outset * 2).max(1),
                )
                    .into(),
            )
        };
        let caster_radii = if rounded_available && server_titlebar {
            crate::render::window_decoration::CornerRadii {
                top: crate::render::window_decoration::scaled_metric(
                    context.decorations.titlebars.radius_px,
                    decoration_scale,
                ) as f32,
                bottom: content_radius + border_outset as f32,
            }
        } else if rounded_available {
            crate::render::window_decoration::CornerRadii::all(
                content_radius + border_outset as f32,
            )
        } else {
            crate::render::window_decoration::CornerRadii::default()
        };
        if let Some(shadow) = shadow_renderer.element_with_radii(
            renderer,
            format!(
                "{}:window:{:?}:{}",
                context.output.name(),
                window_surface.id(),
                context.instance_identity.unwrap_or("canonical")
            ),
            caster,
            caster_radii,
            chrome_alpha,
            context.shadow_config,
        )? {
            elements.push(SceneElement::Shadow(shadow));
        }
    }
    Ok(LiveWindowScene {
        elements,
        cluster_depth: visual.cluster_depth,
        cluster_floating: visual.cluster_floating,
        cluster_exclusive: visual.cluster_exclusive,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_titlebar_elements(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    content: Rectangle<i32, Physical>,
    height: i32,
    border_width: i32,
    radius: f32,
    focused: bool,
    alpha: f32,
    decorations: &halley_config::Decorations,
    hovered: Option<&crate::titlebar::ButtonTarget>,
    pressed: Option<&crate::titlebar::ButtonTarget>,
    titlebar_renderer: &mut crate::render::titlebar::TitlebarRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
    elements: &mut Vec<SceneElement>,
) -> Result<(), Box<dyn Error>> {
    let config = &decorations.titlebars;
    let layout = crate::titlebar::DecorationLayout::new(content, border_width, height, config);
    let background = if focused {
        config.color_focused
    } else {
        config.color_unfocused
    };
    let foreground = if focused {
        config.foreground_color_focused
    } else {
        config.foreground_color_unfocused
    };

    for control in &layout.controls {
        let enabled = crate::titlebar::control_enabled(window, control.control);
        let is_hovered = hovered
            .is_some_and(|target| target.window == *window && target.control == control.control)
            && enabled;
        let is_pressed = pressed
            .is_some_and(|target| target.window == *window && target.control == control.control)
            && enabled;
        let state_color = if is_pressed {
            config.button_pressed_color
        } else if is_hovered {
            config.button_hover_color
        } else {
            foreground
        };
        let backplate = if is_hovered || is_pressed {
            let backplate_alpha = if is_pressed { 0.30 } else { 0.18 };
            window_decoration_renderer.tint_element_with_radii(
                renderer,
                control.rect,
                crate::render::window_decoration::CornerRadii::all(
                    (control.rect.size.h as f32 * 0.20).max(1.0),
                ),
                crate::render::decoration_color(state_color),
                alpha * backplate_alpha,
            )
        } else {
            None
        };
        let glyph_side = crate::titlebar::glyph_size(control.rect.size.h);
        let glyph = Rectangle::new(
            (
                control.rect.loc.x + (control.rect.size.w - glyph_side) / 2,
                control.rect.loc.y + (control.rect.size.h - glyph_side) / 2,
            )
                .into(),
            (glyph_side, glyph_side).into(),
        );
        if let Some(icon) = titlebar_renderer.control_element(
            renderer,
            window_decoration_renderer,
            control.control,
            glyph,
            state_color,
            alpha * if enabled { 1.0 } else { 0.4 },
        ) {
            elements.push(SceneElement::RoundedTexture(icon));
        }
        if let Some(backplate) = backplate {
            elements.push(SceneElement::RoundedTexture(backplate));
        }
    }

    let identity = crate::window::rules::identity(window);
    if let Some(icon_rect) = layout.app_icon
        && let Some(app_id) = identity.app_id.as_deref()
        && let Some(icon) = node_renderer.app_icon_element(renderer, app_id, icon_rect, alpha)
    {
        elements.push(SceneElement::NodeTexture(icon));
    }

    if config.show_title
        && layout.title_clip.size.w > 0
        && let Some(title) = identity.title.as_deref()
    {
        let rgb = color_bytes(foreground);
        if let Some((title, size)) =
            fitted_title(renderer, ui_text, title, rgb, layout.title_clip.size.w)?
            && let Some(prepared) = ui_text.element(
                renderer,
                (
                    layout.titlebar.loc.x + (layout.titlebar.size.w - size.w) / 2,
                    layout.titlebar.loc.y + (layout.titlebar.size.h - size.h) / 2,
                )
                    .into(),
                &title,
                rgb,
                alpha,
            )?
        {
            elements.push(SceneElement::UiText(prepared.element));
        }
    }

    if let Some(background_element) = window_decoration_renderer.tint_element_with_radii(
        renderer,
        layout.titlebar,
        crate::render::window_decoration::CornerRadii::top(radius),
        crate::render::decoration_color(background),
        alpha,
    ) {
        elements.push(SceneElement::RoundedTexture(background_element));
    } else {
        elements.push(SceneElement::Border(SolidColorRenderElement::new(
            Id::new(),
            layout.titlebar,
            CommitCounter::default(),
            crate::render::decoration_color(background) * alpha,
            Kind::Unspecified,
        )));
    }
    Ok(())
}

fn color_bytes(color: halley_config::BorderColor) -> [u8; 3] {
    [
        (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn fitted_title(
    renderer: &mut GlesRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
    title: &str,
    rgb: [u8; 3],
    max_width: i32,
) -> Result<Option<(String, smithay::utils::Size<i32, smithay::utils::Buffer>)>, Box<dyn Error>> {
    if max_width <= 0 || title.is_empty() {
        return Ok(None);
    }
    if let Some(size) = ui_text.measure(renderer, title, rgb)?
        && size.w <= max_width
    {
        return Ok(Some((title.to_string(), size)));
    }
    let characters = title.chars().collect::<Vec<_>>();
    let mut low = 0;
    let mut high = characters.len();
    let mut best = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        let candidate = characters[..middle]
            .iter()
            .chain(std::iter::once(&'…'))
            .collect::<String>();
        let Some(size) = ui_text.measure(renderer, &candidate, rgb)? else {
            return Ok(None);
        };
        if size.w <= max_width {
            best = Some((candidate, size));
            low = middle + 1;
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::compositor_chrome_visible;

    #[test]
    fn either_fullscreen_signal_suppresses_compositor_chrome() {
        assert!(compositor_chrome_visible(false, false));
        assert!(!compositor_chrome_visible(true, false));
        assert!(!compositor_chrome_visible(false, true));
        assert!(!compositor_chrome_visible(true, true));
    }
}
