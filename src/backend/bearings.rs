use std::cmp::Ordering;
use std::error::Error;

use halley_core::bearings::{Bearing, bearing_to_point};
use halley_core::field::{NodeId, Vec2};
use halley_core::viewport::Viewport;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Color32F, element::Kind};
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Rectangle};

use super::scene::SceneElement;

const CHIP_PAD_X: i32 = 10;
const CHIP_PAD_Y: i32 = 7;
const EDGE_PAD: i32 = 16;
const ICON_SIZE: i32 = 16;
const ICON_TEXT_GAP: i32 = 6;
const DISTANCE_GAP: i32 = 4;
const META_PAD_X: i32 = 7;
const META_PAD_Y: i32 = 4;
const GROUP_GAP: i32 = 10;
const MAX_LABEL_CHARS: usize = 24;
const MIN_DISTANCE_ALPHA: f32 = 0.34;
const DISTANCE_HIDE_MULTIPLIER: f32 = 1.5;
const TEXT_RGB: [u8; 3] = [238, 242, 249];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    NW,
    N,
    NE,
    W,
    E,
    SW,
    S,
    SE,
}

impl Lane {
    const ALL: [Self; 8] = [
        Self::NW,
        Self::N,
        Self::NE,
        Self::W,
        Self::E,
        Self::SW,
        Self::S,
        Self::SE,
    ];

    fn horizontal(self) -> bool {
        matches!(self, Self::N | Self::S)
    }
}

impl From<Bearing> for Lane {
    fn from(value: Bearing) -> Self {
        match value {
            Bearing::NW => Self::NW,
            Bearing::N => Self::N,
            Bearing::NE => Self::NE,
            Bearing::W => Self::W,
            Bearing::E => Self::E,
            Bearing::SW => Self::SW,
            Bearing::S => Self::S,
            Bearing::SE => Self::SE,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Size {
    chip_w: i32,
    chip_h: i32,
    distance_w: i32,
    distance_h: i32,
    show_icon: bool,
}

impl Size {
    fn total_h(self) -> i32 {
        self.chip_h
            + if self.distance_h > 0 {
                self.distance_h + DISTANCE_GAP
            } else {
                0
            }
    }

    fn extent(self, lane: Lane) -> i32 {
        if lane.horizontal() {
            self.chip_w
        } else {
            self.total_h()
        }
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    id: NodeId,
    lane: Lane,
    projected: f32,
    distance: f32,
    label: String,
    app_id: Option<String>,
    pinned: bool,
    size: Size,
}

#[derive(Clone, Debug)]
struct Group {
    id: NodeId,
    lane: Lane,
    projected: f32,
    distance: f32,
    label: String,
    app_id: Option<String>,
    pinned: bool,
    alpha: f32,
    size: Size,
}

#[derive(Clone, Debug)]
struct Layout {
    id: NodeId,
    chip: Rectangle<i32, Physical>,
    icon: Option<Rectangle<i32, Physical>>,
    label: String,
    distance: Option<(String, Rectangle<i32, Physical>)>,
    app_id: Option<String>,
    pinned: bool,
    alpha: f32,
}

#[derive(Clone, Copy)]
struct LayoutContext<'a> {
    output_name: &'a str,
    output_geometry: Rectangle<i32, Logical>,
    config: halley_config::Bearings,
    mix: f32,
    nodes: &'a crate::nodes::NodesState,
    camera: &'a halley_core::camera::Camera,
}

#[allow(clippy::too_many_arguments)]
pub fn elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    bearings: &crate::bearings::BearingsState,
    nodes: &crate::nodes::NodesState,
    cameras: &crate::camera::OutputCameras,
    blur_config: halley_config::Blur,
    backdrop_blur_renderer: &mut super::backdrop_blur::BackdropBlurRenderer,
    node_renderer: &mut super::node::NodeRenderer,
    ui_text: &mut super::text::UiTextRenderer,
    overlay_config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let output_name = output.name();
    let mix = bearings.mix(&output_name);
    if mix <= 0.002 {
        bearings.set_hitboxes(&output_name, Vec::new());
        return Ok(Vec::new());
    }
    let Some(camera) = cameras.get(&output_name) else {
        return Ok(Vec::new());
    };
    let overlay_visuals = super::overlay::resolve_visuals(overlay_config, decorations);
    let layouts = collect_layouts(
        renderer,
        LayoutContext {
            output_name: &output_name,
            output_geometry,
            config: bearings.config,
            mix,
            nodes,
            camera,
        },
        ui_text,
    )?;
    bearings.set_hitboxes(
        &output_name,
        layouts
            .iter()
            .map(|layout| crate::bearings::BearingHitbox {
                id: layout.id,
                rect: layout.chip.to_logical(1),
            })
            .collect(),
    );

    let blur_patches = if bearings.config.blur
        && blur_config.enabled
        && blur_config.overlays
        && overlay_config.blur
    {
        layouts
            .iter()
            .filter(|layout| layout.alpha >= 0.04)
            .map(|layout| super::backdrop_blur::BlurPatch {
                rect: layout.chip,
                radius: match overlay_visuals.shape {
                    halley_config::OverlayShape::Square => 0.0,
                    halley_config::OverlayShape::Rounded => 11.0,
                },
                alpha: layout.alpha,
                clip: None,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut foreground = Vec::new();
    let mut backgrounds = Vec::new();
    for layout in layouts {
        let label_size = ui_text
            .measure(renderer, &layout.label, 2, overlay_visuals.text.bytes())?
            .unwrap_or((0, 0).into());
        let text_x = layout.chip.loc.x
            + CHIP_PAD_X
            + if layout.icon.is_some() {
                ICON_SIZE + ICON_TEXT_GAP
            } else {
                0
            };
        if let Some(text) = ui_text.element(
            renderer,
            (
                text_x,
                layout.chip.loc.y + (layout.chip.size.h - label_size.h) / 2,
            )
                .into(),
            &layout.label,
            2,
            overlay_visuals.text.bytes(),
            layout.alpha,
        )? {
            foreground.push(SceneElement::UiText(text.element));
        }

        if let Some(icon) = layout.icon
            && let Some(app_id) = layout.app_id.as_deref()
        {
            node_renderer.request_app_icon(renderer, app_id);
            if let Some(icon) = node_renderer.app_icon_element(renderer, app_id, icon, layout.alpha)
            {
                foreground.push(SceneElement::NodeTexture(icon));
            }
        }

        if layout.pinned {
            foreground.push(SceneElement::Border(SolidColorRenderElement::new(
                Id::new(),
                Rectangle::new(
                    (
                        layout.chip.loc.x + layout.chip.size.w - 12,
                        layout.chip.loc.y + 5,
                    )
                        .into(),
                    (7, 7).into(),
                ),
                CommitCounter::default(),
                Color32F::new(0.30, 0.86, 0.96, layout.alpha),
                Kind::Unspecified,
            )));
        }

        if let Some((distance, rect)) = layout.distance {
            let text_size = ui_text
                .measure(renderer, &distance, 2, overlay_visuals.text.bytes())?
                .unwrap_or((0, 0).into());
            if let Some(text) = ui_text.element(
                renderer,
                (
                    rect.loc.x + (rect.size.w - text_size.w) / 2,
                    rect.loc.y + (rect.size.h - text_size.h) / 2,
                )
                    .into(),
                &distance,
                2,
                overlay_visuals.text.bytes(),
                layout.alpha * 0.96,
            )? {
                foreground.push(SceneElement::UiText(text.element));
            }
            backgrounds.push(SceneElement::NodeLabel(super::overlay::card_element(
                renderer,
                node_renderer,
                rect,
                overlay_visuals,
                overlay_visuals.fill,
                layout.alpha * 0.88,
            )?));
        }
        backgrounds.push(SceneElement::NodeLabel(super::overlay::card_element(
            renderer,
            node_renderer,
            layout.chip,
            overlay_visuals,
            overlay_visuals.fill,
            layout.alpha * 0.92,
        )?));
    }
    foreground.extend(backgrounds);
    if let Some(blur) = backdrop_blur_renderer.blur_element(
        renderer,
        &output_name,
        "bearings",
        output_geometry.size,
        blur_patches,
        blur_config,
    )? {
        foreground.push(SceneElement::BackdropBlur(blur));
    }
    Ok(foreground)
}

fn collect_layouts(
    renderer: &mut GlesRenderer,
    context: LayoutContext<'_>,
    ui_text: &mut super::text::UiTextRenderer,
) -> Result<Vec<Layout>, Box<dyn Error>> {
    let LayoutContext {
        output_name,
        output_geometry,
        config,
        mix,
        nodes,
        camera,
    } = context;
    let center =
        crate::camera::global_center((camera.center.x, camera.center.y).into(), output_geometry);
    let viewport = Viewport::new(
        Vec2 {
            x: center.x,
            y: center.y,
        },
        camera.view_size,
    );
    let screen_w = output_geometry.size.w;
    let screen_h = output_geometry.size.h;
    let mut candidates = Vec::new();

    for record in nodes
        .records()
        .filter(|record| record.attached && record.output == output_name)
    {
        let Some(node) = nodes.field.node(record.id) else {
            continue;
        };
        if !nodes.field.is_visible(record.id)
            || intersects_view(node.pos, node.footprint, &viewport)
        {
            continue;
        }
        let Some(bearing) = bearing_to_point(&viewport, node.pos) else {
            continue;
        };
        let lane = Lane::from(bearing);
        let distance = offscreen_distance(node.pos, node.footprint, &viewport);
        let pinned = node.pinned;
        let alpha = if pinned && config.show_pinned {
            mix
        } else {
            mix * distance_alpha(config.fade_distance, distance)
        };
        if alpha <= 0.002 {
            continue;
        }
        let label = bearing_label(record);
        let distance_text = config
            .show_distance
            .then(|| format!("{:.0}px", distance.round()));
        let show_icon = config.show_icons && record.app_id.is_some();
        let size = measure_size(
            renderer,
            ui_text,
            &label,
            show_icon,
            distance_text.as_deref(),
        )?;
        candidates.push(Candidate {
            id: record.id,
            lane,
            projected: projected_anchor(node.pos, &viewport, lane, screen_w, screen_h),
            distance,
            label,
            app_id: record.app_id.clone(),
            pinned,
            size,
        });
    }

    let mut layouts = Vec::new();
    for lane in Lane::ALL {
        let mut lane_candidates = candidates
            .iter()
            .filter(|candidate| candidate.lane == lane)
            .cloned()
            .collect::<Vec<_>>();
        lane_candidates.sort_by(candidate_order);
        let groups = group_candidates(renderer, ui_text, config, mix, lane_candidates)?;
        layouts.extend(layout_groups(lane, groups, screen_w, screen_h));
    }
    Ok(layouts)
}

fn candidate_order(left: &Candidate, right: &Candidate) -> Ordering {
    left.projected
        .partial_cmp(&right.projected)
        .unwrap_or(Ordering::Equal)
        .then(left.id.as_u64().cmp(&right.id.as_u64()))
}

fn group_candidates(
    renderer: &mut GlesRenderer,
    ui_text: &mut super::text::UiTextRenderer,
    config: halley_config::Bearings,
    mix: f32,
    candidates: Vec<Candidate>,
) -> Result<Vec<Group>, Box<dyn Error>> {
    let mut groups = Vec::new();
    let mut current: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if let Some(previous) = current.last()
            && candidate.projected - previous.projected
                > separation(candidate.lane, previous.size, candidate.size)
        {
            groups.push(finalize_group(
                renderer,
                ui_text,
                config,
                mix,
                std::mem::take(&mut current),
            )?);
        }
        current.push(candidate);
    }
    if !current.is_empty() {
        groups.push(finalize_group(renderer, ui_text, config, mix, current)?);
    }
    Ok(groups)
}

fn finalize_group(
    renderer: &mut GlesRenderer,
    ui_text: &mut super::text::UiTextRenderer,
    config: halley_config::Bearings,
    mix: f32,
    members: Vec<Candidate>,
) -> Result<Group, Box<dyn Error>> {
    let nearest = members
        .iter()
        .min_by(|left, right| {
            left.distance
                .partial_cmp(&right.distance)
                .unwrap_or(Ordering::Equal)
                .then(left.id.as_u64().cmp(&right.id.as_u64()))
        })
        .expect("non-empty bearing group");
    let count = members.len();
    let label = if count == 1 {
        nearest.label.clone()
    } else {
        format!("{count} nodes")
    };
    let pinned = members.iter().any(|member| member.pinned);
    let distance_text = config
        .show_distance
        .then(|| format!("{:.0}px", nearest.distance.round()));
    let show_icon = count == 1 && config.show_icons && nearest.app_id.is_some();
    let size = measure_size(
        renderer,
        ui_text,
        &label,
        show_icon,
        distance_text.as_deref(),
    )?;
    Ok(Group {
        id: nearest.id,
        lane: nearest.lane,
        projected: members.iter().map(|member| member.projected).sum::<f32>() / count as f32,
        distance: nearest.distance,
        label,
        app_id: (count == 1).then(|| nearest.app_id.clone()).flatten(),
        pinned,
        alpha: if pinned && config.show_pinned {
            mix
        } else {
            mix * distance_alpha(config.fade_distance, nearest.distance)
        },
        size,
    })
}

fn layout_groups(lane: Lane, mut groups: Vec<Group>, screen_w: i32, screen_h: i32) -> Vec<Layout> {
    groups.sort_by(|left, right| {
        left.projected
            .partial_cmp(&right.projected)
            .unwrap_or(Ordering::Equal)
            .then(left.id.as_u64().cmp(&right.id.as_u64()))
    });
    let mut centers = groups
        .iter()
        .map(|group| {
            let (min, max) = center_bounds(lane, group.size, screen_w, screen_h);
            group.projected.clamp(min, max)
        })
        .collect::<Vec<_>>();
    for index in 1..centers.len() {
        centers[index] = centers[index]
            .max(centers[index - 1] + separation(lane, groups[index - 1].size, groups[index].size));
    }
    for index in (0..centers.len().saturating_sub(1)).rev() {
        let (_, max) = center_bounds(lane, groups[index].size, screen_w, screen_h);
        centers[index] = centers[index]
            .min(max)
            .min(centers[index + 1] - separation(lane, groups[index].size, groups[index + 1].size));
    }
    for index in 1..centers.len() {
        centers[index] = centers[index]
            .max(centers[index - 1] + separation(lane, groups[index - 1].size, groups[index].size));
    }
    groups
        .into_iter()
        .zip(centers)
        .map(|(group, center)| build_layout(group, center, screen_w, screen_h))
        .collect()
}

fn build_layout(group: Group, center: f32, screen_w: i32, screen_h: i32) -> Layout {
    let distance_block = if group.size.distance_h > 0 {
        group.size.distance_h + DISTANCE_GAP
    } else {
        0
    };
    let vertical_y = center.round() as i32 - group.size.total_h() / 2 + distance_block;
    let (x, y) = match group.lane {
        Lane::N => (
            center.round() as i32 - group.size.chip_w / 2,
            EDGE_PAD + distance_block,
        ),
        Lane::S => (
            center.round() as i32 - group.size.chip_w / 2,
            screen_h - EDGE_PAD - group.size.chip_h,
        ),
        Lane::W | Lane::NW | Lane::SW => (EDGE_PAD, vertical_y),
        Lane::E | Lane::NE | Lane::SE => (screen_w - EDGE_PAD - group.size.chip_w, vertical_y),
    };
    let chip = Rectangle::new((x, y).into(), (group.size.chip_w, group.size.chip_h).into());
    let icon = group.size.show_icon.then(|| {
        Rectangle::new(
            (x + CHIP_PAD_X, y + (group.size.chip_h - ICON_SIZE) / 2).into(),
            (ICON_SIZE, ICON_SIZE).into(),
        )
    });
    let distance = (group.size.distance_h > 0).then(|| {
        let text = format!("{:.0}px", group.distance.round());
        let rect = Rectangle::new(
            (
                x + (group.size.chip_w - group.size.distance_w) / 2,
                y - DISTANCE_GAP - group.size.distance_h,
            )
                .into(),
            (group.size.distance_w, group.size.distance_h).into(),
        );
        (text, rect)
    });
    Layout {
        id: group.id,
        chip,
        icon,
        label: group.label,
        distance,
        app_id: group.app_id,
        pinned: group.pinned,
        alpha: group.alpha.clamp(0.0, 1.0),
    }
}

fn measure_size(
    renderer: &mut GlesRenderer,
    ui_text: &mut super::text::UiTextRenderer,
    label: &str,
    show_icon: bool,
    distance: Option<&str>,
) -> Result<Size, Box<dyn Error>> {
    let label_size = ui_text
        .measure(renderer, label, 2, TEXT_RGB)?
        .unwrap_or((0, 0).into());
    let chip_w = (CHIP_PAD_X * 2
        + label_size.w
        + if show_icon {
            ICON_SIZE + ICON_TEXT_GAP
        } else {
            0
        })
    .max(44);
    let chip_h = (CHIP_PAD_Y * 2 + label_size.h.max(if show_icon { ICON_SIZE } else { 0 })).max(24);
    let distance_size = match distance {
        Some(distance) => ui_text
            .measure(renderer, distance, 2, TEXT_RGB)?
            .unwrap_or((0, 0).into()),
        None => (0, 0).into(),
    };
    Ok(Size {
        chip_w,
        chip_h,
        distance_w: if distance.is_some() {
            distance_size.w + META_PAD_X * 2
        } else {
            0
        },
        distance_h: if distance.is_some() {
            distance_size.h + META_PAD_Y * 2
        } else {
            0
        },
        show_icon,
    })
}

fn separation(lane: Lane, left: Size, right: Size) -> f32 {
    (left.extent(lane) + right.extent(lane)) as f32 * 0.5 + GROUP_GAP as f32
}

fn center_bounds(lane: Lane, size: Size, screen_w: i32, screen_h: i32) -> (f32, f32) {
    let (extent, available) = if lane.horizontal() {
        (size.chip_w, screen_w)
    } else {
        (size.total_h(), screen_h)
    };
    let half = extent as f32 * 0.5;
    (
        EDGE_PAD as f32 + half,
        available as f32 - EDGE_PAD as f32 - half,
    )
}

fn projected_anchor(
    pos: Vec2,
    viewport: &Viewport,
    lane: Lane,
    screen_w: i32,
    screen_h: i32,
) -> f32 {
    let dx = pos.x - viewport.center.x;
    let dy = pos.y - viewport.center.y;
    let half_w = viewport.size.x * 0.5;
    let half_h = viewport.size.y * 0.5;
    let tx = if dx.abs() <= f32::EPSILON {
        f32::INFINITY
    } else {
        half_w / dx.abs()
    };
    let ty = if dy.abs() <= f32::EPSILON {
        f32::INFINITY
    } else {
        half_h / dy.abs()
    };
    let t = tx.min(ty);
    if lane.horizontal() {
        ((viewport.center.x + dx * t - (viewport.center.x - half_w)) / viewport.size.x.max(1.0))
            * screen_w as f32
    } else {
        ((viewport.center.y + dy * t - (viewport.center.y - half_h)) / viewport.size.y.max(1.0))
            * screen_h as f32
    }
}

fn intersects_view(pos: Vec2, footprint: Vec2, viewport: &Viewport) -> bool {
    let view = viewport.rect();
    let half = Vec2 {
        x: footprint.x * 0.5,
        y: footprint.y * 0.5,
    };
    pos.x + half.x > view.min.x
        && pos.x - half.x < view.max.x
        && pos.y + half.y > view.min.y
        && pos.y - half.y < view.max.y
}

fn offscreen_distance(pos: Vec2, footprint: Vec2, viewport: &Viewport) -> f32 {
    let view = viewport.rect();
    let half = Vec2 {
        x: footprint.x * 0.5,
        y: footprint.y * 0.5,
    };
    let overflow_x = if pos.x + half.x <= view.min.x {
        view.min.x - (pos.x + half.x)
    } else if pos.x - half.x >= view.max.x {
        pos.x - half.x - view.max.x
    } else {
        0.0
    };
    let overflow_y = if pos.y + half.y <= view.min.y {
        view.min.y - (pos.y + half.y)
    } else if pos.y - half.y >= view.max.y {
        pos.y - half.y - view.max.y
    } else {
        0.0
    };
    overflow_x.hypot(overflow_y)
}

fn distance_alpha(fade_distance: f32, distance: f32) -> f32 {
    let fade_end = fade_distance * DISTANCE_HIDE_MULTIPLIER;
    if distance <= fade_distance {
        let t = (distance / fade_distance).clamp(0.0, 1.0);
        return MIN_DISTANCE_ALPHA + (1.0 - t) * (1.0 - MIN_DISTANCE_ALPHA);
    }
    if distance >= fade_end {
        return 0.0;
    }
    MIN_DISTANCE_ALPHA * (1.0 - (distance - fade_distance) / (fade_end - fade_distance))
}

fn bearing_label(record: &crate::nodes::NodeRecord) -> String {
    let base = if !record.title.trim().is_empty() {
        record.title.trim()
    } else if let Some(app_id) = record.app_id.as_deref() {
        app_id
    } else {
        return format!("Node {}", record.id.as_u64());
    };
    let mut chars = base.chars();
    let prefix = chars.by_ref().take(MAX_LABEL_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_fade_matches_old_halley_thresholds() {
        assert_eq!(distance_alpha(1_200.0, 0.0), 1.0);
        assert!((distance_alpha(1_200.0, 1_200.0) - MIN_DISTANCE_ALPHA).abs() < 0.0001);
        assert_eq!(distance_alpha(1_200.0, 1_800.0), 0.0);
    }

    #[test]
    fn long_labels_are_unicode_safe_and_truncated() {
        let source = "abcdefghijklmnopqrstuvwxYZ";
        let mut chars = source.chars();
        let prefix = chars.by_ref().take(MAX_LABEL_CHARS).collect::<String>();
        assert_eq!(format!("{prefix}…"), "abcdefghijklmnopqrstuvwx…");
    }
}
