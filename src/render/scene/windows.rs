use super::*;

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
    pub(super) blur: halley_config::Blur,
    pub(super) shadow_config: halley_config::ShadowLayer,
    pub(super) window_open_animations: &'a crate::animation::WindowOpenAnimations,
    pub(super) fullscreen: &'a crate::wayland::fullscreen::FullscreenManager,
    pub(super) maximize: &'a crate::presentation::maximize::FieldMaximizeManager,
    pub(super) window_rules: &'a crate::window::rules::WindowRulesState,
    pub(super) cluster_presentation_override: Option<crate::clusters::WindowPresentation>,
    pub(super) instance_identity: Option<&'static str>,
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

fn fullscreen_chrome_visibility(progress: Option<f64>) -> f32 {
    progress
        .map(|progress| 1.0 - progress.clamp(0.0, 1.0) as f32)
        .unwrap_or(1.0)
}

pub(super) fn live_window_elements(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    context: LiveWindowContext<'_>,
    fullscreen_textures: &mut crate::render::fullscreen_texture::FullscreenTextureTransitions,
    backdrop_blur_renderer: &mut crate::render::effects::backdrop_blur::BackdropBlurRenderer,
    shadow_renderer: &mut crate::render::effects::shadow::ShadowRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
) -> Result<LiveWindowScene, Box<dyn Error>> {
    let Some(location) = context.space.element_location(window) else {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: None,
            cluster_exclusive: false,
        });
    };
    let Some(window_surface) = window.wl_surface() else {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: None,
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
            cluster_exclusive: false,
        });
    };
    if visual.animated_rect.size.w == 0 || visual.animated_rect.size.h == 0 {
        return Ok(LiveWindowScene {
            elements: Vec::new(),
            cluster_depth: visual.cluster_depth,
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
    let chrome_visibility =
        fullscreen_chrome_visibility(visual.fullscreen.map(|presentation| presentation.progress));
    let chrome_alpha = alpha * chrome_visibility;
    let content_radius = crate::render::window_decoration::scaled_metric(
        context.decorations.border_radius_px,
        visual.zoom_scale,
    ) as f32
        * chrome_visibility;
    let rounded = managed && content_radius > 0.0;
    let rounded_available = rounded && window_decoration_renderer.available(renderer);
    if join_ready {
        let tint_alpha = alpha * 0.14;
        let tint_color = smithay::backend::renderer::Color32F::new(0.0, 0.0, 0.0, 1.0);
        if let Some(tint) = window_decoration_renderer.tint_element(
            renderer,
            visual.animated_rect,
            if rounded_available {
                content_radius
            } else {
                0.0
            },
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
            if rounded_available {
                content_radius
            } else {
                0.0
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
                let element = window_decoration_renderer
                    .surface_element(
                        renderer,
                        surface_element,
                        destination,
                        visual.animated_rect,
                        content_radius,
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
                        clip: rounded_available.then_some((visual.animated_rect, content_radius)),
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
            && let Some(border) = window_decoration_renderer.border_element(
                renderer,
                visual.animated_rect,
                border_width,
                content_radius,
                border_color,
                chrome_alpha,
            )
        {
            elements.push(SceneElement::WindowBorder(border));
        } else {
            elements.extend(
                crate::render::border_strips(
                    visual.animated_rect,
                    border_width,
                    border_color * chrome_alpha,
                )
                .into_iter()
                .map(SceneElement::Border),
            );
        }
    }
    if managed && chrome_alpha > 0.0 {
        let border_outset = border_width.max(0);
        let caster = Rectangle::new(
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
        );
        let caster_radius = if rounded_available {
            content_radius + border_outset as f32
        } else {
            0.0
        };
        if let Some(shadow) = shadow_renderer.element(
            renderer,
            format!(
                "{}:window:{:?}:{}",
                context.output.name(),
                window_surface.id(),
                context.instance_identity.unwrap_or("canonical")
            ),
            caster,
            caster_radius,
            chrome_alpha,
            context.shadow_config,
        )? {
            elements.push(SceneElement::Shadow(shadow));
        }
    }
    Ok(LiveWindowScene {
        elements,
        cluster_depth: visual.cluster_depth,
        cluster_exclusive: visual.cluster_exclusive,
    })
}

#[cfg(test)]
mod tests {
    use super::fullscreen_chrome_visibility;

    #[test]
    fn fullscreen_chrome_fades_without_a_cleanup_step() {
        assert_eq!(fullscreen_chrome_visibility(None), 1.0);
        assert_eq!(fullscreen_chrome_visibility(Some(0.0)), 1.0);
        assert_eq!(fullscreen_chrome_visibility(Some(0.5)), 0.5);
        assert_eq!(fullscreen_chrome_visibility(Some(1.0)), 0.0);
    }

    #[test]
    fn fullscreen_chrome_visibility_clamps_motion_overshoot() {
        assert_eq!(fullscreen_chrome_visibility(Some(-0.2)), 1.0);
        assert_eq!(fullscreen_chrome_visibility(Some(1.2)), 0.0);
    }
}
