use super::nodes::{ease_in_out_cubic, fit_ui_text};
use super::*;

pub(super) fn preview_content_radius(decorations: &halley_config::Decorations) -> f32 {
    decorations.border_radius_px.max(0) as f32
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apogee_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    state: &crate::shell::apogee::ApogeeState,
    config: halley_config::Apogee,
    overlay_config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    cameras: &crate::presentation::camera::OutputCameras,
    nodes: &crate::nodes::NodesState,
    node_renderer: &mut crate::render::node::NodeRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
    window_open_animations: &crate::animation::WindowOpenAnimations,
    fullscreen: &crate::wayland::fullscreen::FullscreenManager,
    maximize: &crate::presentation::maximize::FieldMaximizeManager,
    overlay_previews: &mut crate::render::overlays::preview::OverlayPreviewCache,
    now: std::time::Duration,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let Some(session) = state.session() else {
        return Ok(Vec::new());
    };
    let progress = session.progress(now).clamp(0.0, 1.0);
    let visuals = apogee_transition_visuals(progress);
    let output_local = Rectangle::<i32, Physical>::from_size(output_geometry.size.to_physical(1));
    let overlay_visuals =
        crate::render::overlays::shell::resolve_visuals(overlay_config, decorations);
    let mut tiles = session
        .tiles
        .iter()
        .filter(|tile| tile.output == output.name())
        .collect::<Vec<_>>();
    sort_apogee_tiles(&mut tiles, session.selected);

    let mut elements = Vec::new();
    overlay_previews.retain(session.tiles.iter().map(|tile| tile.id));
    for tile in tiles {
        let Some(record) = nodes.record(tile.id) else {
            continue;
        };
        let target = Rectangle::<i32, Physical>::new(
            (tile.target.loc - output_geometry.loc).to_physical(1),
            tile.target.size.to_physical(1),
        );
        let source = if record.collapsed {
            let Some(camera) = cameras.get(&output.name()) else {
                continue;
            };
            let Some(node) = nodes.field.node(tile.id) else {
                continue;
            };
            let center = crate::nodes::screen_from_world(node.pos, camera, output_geometry)
                - output_geometry.loc;
            Rectangle::new(
                (
                    center.x - crate::nodes::NODE_DIAMETER_PX.round() as i32 / 2,
                    center.y - crate::nodes::NODE_DIAMETER_PX.round() as i32 / 2,
                )
                    .into(),
                (
                    crate::nodes::NODE_DIAMETER_PX.round() as i32,
                    crate::nodes::NODE_DIAMETER_PX.round() as i32,
                )
                    .into(),
            )
        } else {
            window_visual_state(
                space,
                cameras,
                &record.window,
                output,
                window_open_animations,
                fullscreen,
                maximize,
                now,
            )
            .map(|visual| visual.animated_rect)
            .unwrap_or_else(|| record.geometry.to_physical(1))
        };
        let body = lerp_rect(source, target, progress);
        let selected = session.selected == Some(tile.id);
        let hovered = session.hovered == Some(tile.id);
        let chrome_alpha = visuals.chrome_alpha;
        // Old Halley kept the caption inside the preview. Growing the card by
        // a fixed footer made its backing look like an enlarged second window,
        // especially for short and wide Apogee tiles.
        // Keep a real color gutter between the overlay border and the preview.
        // The old fixed four-pixel outset was entirely consumed by common
        // three/four-pixel borders, making the border and white preview appear
        // stacked on top of each other.
        let card = outset_physical(body, overlay_visuals.border_px.ceil() as i32 + 4);
        let caption = apogee_caption_rect(body);
        if let Some(caption) = caption {
            let (title, size) = fit_ui_text(
                renderer,
                ui_text,
                &record.title,
                2,
                overlay_visuals.text.bytes(),
                caption.size.w - 16,
            )?;
            if !title.is_empty()
                && let Some(text) = ui_text.element(
                    renderer,
                    (
                        caption.loc.x + (caption.size.w - size.w).max(0) / 2,
                        caption.loc.y + (caption.size.h - size.h).max(0) / 2,
                    )
                        .into(),
                    &title,
                    2,
                    overlay_visuals.text.bytes(),
                    chrome_alpha,
                )?
            {
                elements.push(SceneElement::UiText(text.element));
            }
            let fill = if selected || hovered {
                overlay_visuals.fill.mix(overlay_visuals.border, 0.16)
            } else {
                overlay_visuals.fill
            };
            elements.push(SceneElement::NodeLabel(
                crate::render::overlays::shell::label_card_element(
                    renderer,
                    node_renderer,
                    caption,
                    overlay_visuals,
                    fill,
                    if selected || hovered {
                        0.96 * chrome_alpha
                    } else {
                        0.88 * chrome_alpha
                    },
                )?,
            ));
        }
        if record.collapsed {
            let badge = "NODE";
            if let Some(size) = ui_text.measure(renderer, badge, 1, [151, 205, 255])?
                && let Some(text) = ui_text.element(
                    renderer,
                    (card.loc.x + card.size.w - size.w - 10, card.loc.y + 8).into(),
                    badge,
                    1,
                    [151, 205, 255],
                    chrome_alpha,
                )?
            {
                elements.push(SceneElement::UiText(text.element));
            }
        }

        let preview_radius = preview_content_radius(decorations);
        match overlay_previews.element_with_texture(
            renderer,
            tile.id,
            &record.window,
            body,
            visuals.preview_alpha,
            config.live_previews,
        ) {
            Ok((preview, texture))
                if preview_radius > 0.0 && window_decoration_renderer.available(renderer) =>
            {
                let preview = window_decoration_renderer
                    .texture_element(renderer, preview, texture, body, preview_radius)
                    .expect("rounded resources were checked above");
                elements.push(SceneElement::RoundedClosing(preview));
            }
            Ok((preview, _)) => elements.push(SceneElement::Closing(preview)),
            Err(_) => {
                if let Some(app_id) = record.app_id.as_deref()
                    && let Some(icon) = node_renderer.app_icon_element(
                        renderer,
                        app_id,
                        body,
                        visuals.preview_alpha,
                    )
                {
                    elements.push(SceneElement::NodeTexture(icon));
                }
            }
        }
        let card_fill = if selected || hovered {
            overlay_visuals.fill.mix(overlay_visuals.border, 0.12)
        } else {
            overlay_visuals.fill
        };
        elements.push(SceneElement::NodeLabel(
            crate::render::overlays::shell::card_element(
                renderer,
                node_renderer,
                card,
                overlay_visuals,
                card_fill,
                0.96 * visuals.overlay_alpha,
            )?,
        ));
    }
    elements.push(SceneElement::Border(SolidColorRenderElement::new(
        Id::new(),
        output_local,
        CommitCounter::default(),
        smithay::backend::renderer::Color32F::new(
            0.01,
            0.018,
            0.03,
            config.background_dim * visuals.overlay_alpha,
        ),
        Kind::Unspecified,
    )));
    Ok(elements)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ApogeeTransitionVisuals {
    pub(super) preview_alpha: f32,
    pub(super) overlay_alpha: f32,
    pub(super) chrome_alpha: f32,
}

pub(super) fn apogee_transition_visuals(progress: f32) -> ApogeeTransitionVisuals {
    let overlay_alpha = progress.clamp(0.0, 1.0);
    ApogeeTransitionVisuals {
        // The preview is the handoff surface: at progress zero it occupies the
        // live window's exact rect, so fading it would expose a wallpaper-only
        // frame on both entry and exit.
        preview_alpha: 1.0,
        overlay_alpha,
        chrome_alpha: ((overlay_alpha - 0.18) / 0.62).clamp(0.0, 1.0),
    }
}

pub(super) fn sort_apogee_tiles(
    tiles: &mut [&crate::shell::apogee::Tile],
    selected: Option<halley_core::field::NodeId>,
) {
    tiles.sort_by_key(|tile| {
        (
            usize::from(selected == Some(tile.id)),
            tile.source_stack_index,
            tile.source_stack_order,
        )
    });
    // Smithay render-element lists are front-to-back.
    tiles.reverse();
}

pub(super) fn lerp_rect(
    from: Rectangle<i32, Physical>,
    to: Rectangle<i32, Physical>,
    progress: f32,
) -> Rectangle<i32, Physical> {
    let lerp = |a: i32, b: i32| (a as f32 + (b - a) as f32 * progress).round() as i32;
    Rectangle::new(
        (lerp(from.loc.x, to.loc.x), lerp(from.loc.y, to.loc.y)).into(),
        (
            lerp(from.size.w, to.size.w).max(1),
            lerp(from.size.h, to.size.h).max(1),
        )
            .into(),
    )
}

pub(super) fn outset_physical(
    rect: Rectangle<i32, Physical>,
    pad: i32,
) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (rect.loc.x - pad, rect.loc.y - pad).into(),
        (rect.size.w + pad * 2, rect.size.h + pad * 2).into(),
    )
}

pub(super) fn apogee_caption_rect(
    body: Rectangle<i32, Physical>,
) -> Option<Rectangle<i32, Physical>> {
    if body.size.w < 96 || body.size.h < 72 {
        return None;
    }
    let height = ((body.size.h as f32 * 0.13).round() as i32).clamp(22, 34);
    Some(Rectangle::new(
        (body.loc.x + 8, body.loc.y + body.size.h - height - 8).into(),
        ((body.size.w - 16).max(1), height).into(),
    ))
}

pub(super) struct FocusCycleRenderContext<'a> {
    pub state: &'a crate::shell::focus_cycle::FocusCycleState,
    pub nodes: &'a crate::nodes::NodesState,
    pub overlay_config: &'a halley_config::Overlays,
    pub decorations: &'a halley_config::Decorations,
    pub now: std::time::Duration,
}

pub(super) fn focus_cycle_elements(
    renderer: &mut GlesRenderer,
    output_geometry: Rectangle<i32, Logical>,
    context: FocusCycleRenderContext<'_>,
    overlay_previews: &mut crate::render::overlays::preview::OverlayPreviewCache,
    node_renderer: &mut crate::render::node::NodeRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let Some(session) = context.state.session() else {
        return Ok(Vec::new());
    };
    let open = session.open_progress(context.now);
    let close = session.close_progress(context.now);
    let alpha = (open * (1.0 - close)).clamp(0.0, 1.0);
    if alpha <= 0.001 {
        return Ok(Vec::new());
    }

    let screen = output_geometry.size.to_physical(1);
    let overlay_visuals = crate::render::overlays::shell::resolve_visuals(
        context.overlay_config,
        context.decorations,
    );
    let rail_step = (screen.w as f32 * 0.28).clamp(260.0, 440.0) + 9.0;
    let center_y = screen.h as f32 * 0.5;
    let mut cards = session
        .visible_slots(crate::shell::focus_cycle::VISIBLE_RADIUS)
        .into_iter()
        .filter_map(|(_, id)| {
            let record = context.nodes.record(id)?;
            let index = session
                .candidates
                .iter()
                .position(|candidate| *candidate == id)?;
            let offset = session.visual_offset(index, context.now);
            let distance = offset.abs().min(2.0);
            let base_h = (screen.h as f32 * 0.46).clamp(240.0, 480.0);
            let scale = if distance <= 1.0 {
                1.0 + (0.82 - 1.0) * distance
            } else {
                0.82 + (0.64 - 0.82) * (distance - 1.0)
            };
            let preview_h = (base_h * scale).round().max(1.0) as i32;
            let aspect = (record.geometry.size.w.max(1) as f32
                / record.geometry.size.h.max(1) as f32)
                .clamp(0.7, 2.0);
            let preview_w = (preview_h as f32 * aspect).round().max(1.0) as i32;
            let cx = screen.w as f32 * 0.5 + offset * rail_step;
            let cy = center_y + distance * 22.0 + (1.0 - open) * 22.0 + close * 18.0;
            let pose_scale = 0.88 + 0.12 * open - 0.08 * close;
            let card_w = (preview_w as f32 * pose_scale).round().max(1.0) as i32;
            let card_h = (preview_h as f32 * pose_scale).round().max(1.0) as i32;
            Some((
                distance,
                id,
                Rectangle::<i32, Physical>::new(
                    (
                        (cx - card_w as f32 * 0.5).round() as i32,
                        (cy - card_h as f32 * 0.5).round() as i32,
                    )
                        .into(),
                    (card_w, card_h).into(),
                ),
            ))
        })
        .collect::<Vec<_>>();
    cards.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut elements = Vec::new();
    overlay_previews.retain(session.candidates.iter().copied());
    for (distance, id, card) in cards {
        let Some(record) = context.nodes.record(id) else {
            continue;
        };
        let selected = distance < 0.45;
        let distance_step = distance.round().clamp(0.0, 2.0) as i32;
        let pad = if distance_step >= 2 { 4 } else { 6 };
        let body_bounds = Rectangle::<i32, Physical>::new(
            (card.loc.x + pad, card.loc.y + pad).into(),
            (
                (card.size.w - pad * 2).max(1),
                (card.size.h - pad * 2).max(1),
            )
                .into(),
        );
        let (source_width, source_height) = overlay_previews
            .source_dimensions(id)
            .unwrap_or((record.geometry.size.w, record.geometry.size.h));
        let body = aspect_fit_rect(body_bounds, source_width, source_height);
        let card = outset_rect(body, pad);

        // Top-right monitor badge, over the preview.
        let monitor = truncate_chars(&record.output, 10);
        if let Some(size) = ui_text.measure(renderer, &monitor, 1, overlay_visuals.text.bytes())? {
            let badge = Rectangle::<i32, Physical>::new(
                (body.loc.x + body.size.w - size.w - 22, body.loc.y + 8).into(),
                (size.w + 14, size.h + 8).into(),
            );
            if let Some(text) = ui_text.element(
                renderer,
                (badge.loc.x + 7, badge.loc.y + 4).into(),
                &monitor,
                1,
                overlay_visuals.text.bytes(),
                alpha,
            )? {
                elements.push(SceneElement::UiText(text.element));
            }
            elements.push(SceneElement::NodeLabel(
                crate::render::overlays::shell::label_card_element(
                    renderer,
                    node_renderer,
                    badge,
                    overlay_visuals,
                    overlay_visuals.fill.mix(overlay_visuals.border, 0.12),
                    if selected { 0.95 * alpha } else { 0.78 * alpha },
                )?,
            ));
        }

        if record.collapsed
            && let Some(size) =
                ui_text.measure(renderer, "NODE", 1, overlay_visuals.border.bytes())?
        {
            let badge = Rectangle::<i32, Physical>::new(
                (body.loc.x + 8, body.loc.y + 8).into(),
                (size.w + 14, size.h + 8).into(),
            );
            if let Some(text) = ui_text.element(
                renderer,
                (badge.loc.x + 7, badge.loc.y + 4).into(),
                "NODE",
                1,
                overlay_visuals.border.bytes(),
                alpha,
            )? {
                elements.push(SceneElement::UiText(text.element));
            }
            elements.push(SceneElement::NodeLabel(
                crate::render::overlays::shell::label_card_element(
                    renderer,
                    node_renderer,
                    badge,
                    overlay_visuals,
                    overlay_visuals.fill,
                    0.94 * alpha,
                )?,
            ));
        }

        // Old Halley put the title and app icon in a caption band over the
        // bottom of the thumbnail instead of growing a giant footer.
        let title_scale = match distance_step {
            0 => 3,
            1 => 2,
            _ => 1,
        };
        let title = truncate_chars(
            &record.title,
            match distance_step {
                0 => 42,
                1 => 30,
                _ => 20,
            },
        );
        let text_size = ui_text
            .measure(renderer, &title, title_scale, overlay_visuals.text.bytes())?
            .unwrap_or_default();
        let band_margin = 6;
        let band_h = (text_size.h + 10)
            .max(24)
            .min((body.size.h - band_margin * 2).max(1));
        let band = Rectangle::<i32, Physical>::new(
            (
                body.loc.x + band_margin,
                body.loc.y + body.size.h - band_h - band_margin,
            )
                .into(),
            ((body.size.w - band_margin * 2).max(1), band_h).into(),
        );
        let mut text_x = band.loc.x + 10;
        if distance_step < 2 {
            let icon_size = (band_h - 6).clamp(14, 30);
            let icon_rect = Rectangle::<i32, Physical>::new(
                (band.loc.x + 5, band.loc.y + (band_h - icon_size) / 2).into(),
                (icon_size, icon_size).into(),
            );
            if let Some(app_id) = record.app_id.as_deref()
                && let Some(icon) =
                    node_renderer.app_icon_element(renderer, app_id, icon_rect, alpha)
            {
                elements.push(SceneElement::NodeTexture(icon));
                text_x = icon_rect.loc.x + icon_rect.size.w + 8;
            }
        }
        if let Some(text) = ui_text.element(
            renderer,
            (text_x, band.loc.y + (band_h - text_size.h) / 2).into(),
            &title,
            title_scale,
            overlay_visuals.text.bytes(),
            alpha,
        )? {
            elements.push(SceneElement::UiText(text.element));
        }
        elements.push(SceneElement::NodeLabel(
            crate::render::overlays::shell::label_card_element(
                renderer,
                node_renderer,
                band,
                overlay_visuals,
                if selected {
                    overlay_visuals.fill.mix(overlay_visuals.border, 0.18)
                } else {
                    overlay_visuals.fill
                },
                if selected { 0.96 * alpha } else { 0.88 * alpha },
            )?,
        ));

        let preview_radius = preview_content_radius(context.decorations);
        match overlay_previews.element_with_texture(
            renderer,
            id,
            &record.window,
            body,
            alpha,
            selected,
        ) {
            Ok((preview, texture))
                if preview_radius > 0.0 && window_decoration_renderer.available(renderer) =>
            {
                let preview = window_decoration_renderer
                    .texture_element(renderer, preview, texture, body, preview_radius)
                    .expect("rounded resources were checked above");
                elements.push(SceneElement::RoundedClosing(preview));
            }
            Ok((preview, _)) => elements.push(SceneElement::Closing(preview)),
            Err(_) => {
                if let Some(app_id) = record.app_id.as_deref()
                    && let Some(icon) =
                        node_renderer.app_icon_element(renderer, app_id, body, alpha)
                {
                    elements.push(SceneElement::NodeTexture(icon));
                }
            }
        }
        elements.push(SceneElement::NodeLabel(
            crate::render::overlays::shell::card_element(
                renderer,
                node_renderer,
                card,
                overlay_visuals,
                if selected {
                    overlay_visuals.fill.mix(overlay_visuals.border, 0.12)
                } else {
                    overlay_visuals.fill
                },
                if selected { 0.99 * alpha } else { 0.82 * alpha },
            )?,
        ));
    }

    let hints = "Tab  next     Shift+Tab  previous     Esc  cancel";
    if let Some(size) = ui_text.measure(renderer, hints, 1, overlay_visuals.subtext.bytes())?
        && let Some(text) = ui_text.element(
            renderer,
            ((screen.w - size.w) / 2, screen.h - size.h - 28).into(),
            hints,
            1,
            overlay_visuals.subtext.bytes(),
            alpha,
        )?
    {
        elements.push(SceneElement::UiText(text.element));
    }
    elements.push(SceneElement::Border(SolidColorRenderElement::new(
        Id::new(),
        Rectangle::from_size(screen),
        CommitCounter::default(),
        smithay::backend::renderer::Color32F::new(0.02, 0.03, 0.05, 0.55 * alpha),
        Kind::Unspecified,
    )));
    Ok(elements)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn hover_preview_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    nodes: &crate::nodes::NodesState,
    cameras: &crate::presentation::camera::OutputCameras,
    overlay_previews: &mut crate::render::overlays::preview::OverlayPreviewCache,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    overlay_config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
    now: std::time::Duration,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let Some((id, raw_mix)) = nodes.preview_hover_mix(&output.name(), now) else {
        return Ok(Vec::new());
    };
    let Some(record) = nodes
        .record(id)
        .filter(|record| record.collapsed && record.attached && record.output == output.name())
    else {
        return Ok(Vec::new());
    };
    let Some(node) = nodes.field.node(id) else {
        return Ok(Vec::new());
    };
    let Some(camera) = cameras.get(&output.name()) else {
        return Ok(Vec::new());
    };

    let preview_mix = ease_in_out_cubic(raw_mix.clamp(0.0, 1.0));
    let alpha = (preview_mix * preview_mix).clamp(0.0, 1.0);
    if alpha <= 0.01 {
        return Ok(Vec::new());
    }

    let screen = output_geometry.size.to_physical(1);
    let preview_size_base = ((screen.w.min(screen.h) as f32) * 0.30)
        .round()
        .clamp(220.0, 360.0) as i32;
    let source = record.geometry.size.to_physical(1);
    let source_side = source.w.max(source.h).max(1);
    let base_side = (source_side + 24).clamp(120, preview_size_base);
    let preview_size = ((base_side as f32) * (0.94 + 0.06 * preview_mix))
        .round()
        .max(120.0) as i32;

    let landmark = nodes.landmark_position(id, node.pos, now);
    let center =
        crate::nodes::screen_from_world(landmark, camera, output_geometry) - output_geometry.loc;
    let card = Rectangle::<i32, Physical>::new(
        (
            (center.x - preview_size / 2).clamp(10, (screen.w - preview_size - 10).max(10)),
            (center.y - preview_size / 2).clamp(10, (screen.h - preview_size - 10).max(10)),
        )
            .into(),
        (preview_size, preview_size).into(),
    );
    let pad = ((preview_size as f32) * 0.045).round() as i32;
    let label_h = 30.min((preview_size / 5).max(22));
    let body_bounds = Rectangle::<i32, Physical>::new(
        (card.loc.x + pad, card.loc.y + pad).into(),
        (
            (card.size.w - pad * 2).max(1),
            (card.size.h - pad * 2 - label_h).max(1),
        )
            .into(),
    );
    let body = aspect_fit_rect(body_bounds, record.geometry.size.w, record.geometry.size.h);
    let visuals = crate::render::overlays::shell::resolve_visuals(overlay_config, decorations);
    let preview_radius = preview_content_radius(decorations);
    let mut elements = Vec::new();

    let title = truncate_chars(&record.title, 24);
    if !title.is_empty()
        && let Some(text_size) = ui_text.measure(renderer, &title, 2, visuals.text.bytes())?
        && let Some(text) = ui_text.element(
            renderer,
            (
                card.loc.x + (card.size.w - text_size.w).max(0) / 2,
                card.loc.y + card.size.h - pad - label_h + (label_h - text_size.h).max(0) / 2,
            )
                .into(),
            &title,
            2,
            visuals.text.bytes(),
            0.94 * alpha,
        )?
    {
        elements.push(SceneElement::UiText(text.element));
    }

    match overlay_previews.element_with_texture(renderer, id, &record.window, body, alpha, true) {
        Ok((preview, texture))
            if preview_radius > 0.0 && window_decoration_renderer.available(renderer) =>
        {
            let preview = window_decoration_renderer
                .texture_element(renderer, preview, texture, body, preview_radius)
                .expect("rounded resources were checked above");
            elements.push(SceneElement::RoundedClosing(preview));
        }
        Ok((preview, _)) => elements.push(SceneElement::Closing(preview)),
        Err(_) => elements.push(SceneElement::Border(SolidColorRenderElement::new(
            Id::new(),
            body,
            CommitCounter::default(),
            smithay::backend::renderer::Color32F::new(0.02, 0.03, 0.05, 0.76 * alpha),
            Kind::Unspecified,
        ))),
    }

    elements.push(SceneElement::NodeLabel(
        crate::render::overlays::shell::card_element(
            renderer,
            node_renderer,
            card,
            visuals,
            visuals.fill,
            0.86 * alpha,
        )?,
    ));
    Ok(elements)
}

pub(super) fn aspect_fit_rect(
    bounds: Rectangle<i32, Physical>,
    source_width: i32,
    source_height: i32,
) -> Rectangle<i32, Physical> {
    let source_width = source_width.max(1) as f32;
    let source_height = source_height.max(1) as f32;
    let scale = (bounds.size.w as f32 / source_width).min(bounds.size.h as f32 / source_height);
    let width = (source_width * scale).round().max(1.0) as i32;
    let height = (source_height * scale).round().max(1.0) as i32;
    Rectangle::new(
        (
            bounds.loc.x + (bounds.size.w - width) / 2,
            bounds.loc.y + (bounds.size.h - height) / 2,
        )
            .into(),
        (width, height).into(),
    )
}

pub(super) fn outset_rect(
    body: Rectangle<i32, Physical>,
    padding: i32,
) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (body.loc.x - padding, body.loc.y - padding).into(),
        (
            body.size.w + padding.saturating_mul(2),
            body.size.h + padding.saturating_mul(2),
        )
            .into(),
    )
}

pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.trim().chars();
    let prefix = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
