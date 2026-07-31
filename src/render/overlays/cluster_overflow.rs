use std::error::Error;

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::utils::Rectangle;

use crate::render::node::NodeRenderer;
use crate::render::scene::SceneElement;
use crate::render::text::UiTextRenderer;

use super::shell::{card_element, label_card_element, resolve_visuals};

pub(crate) struct OverflowElementContext<'a> {
    pub(crate) output: &'a Output,
    pub(crate) clusters: &'a crate::clusters::ClusterSystem,
    pub(crate) nodes: &'a crate::nodes::NodesState,
    pub(crate) config: &'a halley_config::Overlays,
    pub(crate) decorations: &'a halley_config::Decorations,
    pub(crate) node_renderer: &'a mut NodeRenderer,
    pub(crate) ui_text: &'a mut UiTextRenderer,
}

pub(crate) fn elements(
    renderer: &mut GlesRenderer,
    context: OverflowElementContext<'_>,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let OverflowElementContext {
        output,
        clusters,
        nodes,
        config,
        decorations,
        node_renderer,
        ui_text,
    } = context;
    let work_area = smithay::desktop::layer_map_for_output(output).non_exclusive_zone();
    let Some(layout) = clusters.overflow_layout(&output.name(), work_area) else {
        return Ok(Vec::new());
    };
    let visuals = resolve_visuals(config, decorations);
    let mut contents = Vec::new();
    let mut chips = Vec::new();
    let allow_icons = clusters.config().tiling.overflow_show_icons
        && matches!(
            nodes.config.show_app_icons,
            halley_config::NodeDisplayPolicy::Always
        );

    for item in layout.items {
        let rect = item.rect.to_physical(1);
        chips.push(SceneElement::NodeLabel(label_card_element(
            renderer,
            node_renderer,
            rect,
            visuals,
            visuals.border,
            0.98,
        )?));
        let record = nodes.record(item.node_id);
        let real_icon = allow_icons
            .then(|| record.and_then(|record| record.app_id.as_deref()))
            .flatten()
            .and_then(|app_id| {
                node_renderer.request_app_icon(renderer, app_id);
                node_renderer.app_icon_element(
                    renderer,
                    app_id,
                    Rectangle::new((rect.loc.x + 4, rect.loc.y + 4).into(), (36, 36).into()),
                    1.0,
                )
            });
        if let Some(icon) = real_icon {
            contents.push(SceneElement::NodeTexture(icon));
            continue;
        }
        let glyph = record
            .and_then(|record| record.app_id.as_deref())
            .or_else(|| {
                nodes
                    .field
                    .node(item.node_id)
                    .map(|node| node.label.as_str())
            })
            .and_then(|label| label.chars().find(char::is_ascii_alphanumeric))
            .map(|ch| ch.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        if let Some(size) = ui_text.measure(renderer, &glyph, 2, visuals.text.bytes())?
            && let Some(text) = ui_text.element(
                renderer,
                (
                    rect.loc.x + (rect.size.w - size.w) / 2,
                    rect.loc.y + (rect.size.h - size.h) / 2,
                )
                    .into(),
                &glyph,
                2,
                visuals.text.bytes(),
                1.0,
            )?
        {
            contents.push(SceneElement::UiText(text.element));
        }
    }

    // Scene elements are front-to-back: content, then its chip, then the
    // enclosing strip. Keeping this explicit prevents cards from masking the
    // icons they contain.
    contents.extend(chips);
    contents.push(SceneElement::NodeLabel(card_element(
        renderer,
        node_renderer,
        layout.strip.to_physical(1),
        visuals,
        visuals.fill,
        0.94,
    )?));
    Ok(contents)
}
