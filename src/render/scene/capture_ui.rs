use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn capture_overlay_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    overlay: crate::capture::CaptureOverlay<'_>,
    node_renderer: &mut crate::render::node::NodeRenderer,
    overlay_config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let visuals = crate::render::overlays::shell::resolve_visuals(overlay_config, decorations);
    match overlay {
        crate::capture::CaptureOverlay::None => Ok(Vec::new()),
        crate::capture::CaptureOverlay::Region(region) => {
            Ok(
                capture_picker_elements(output_geometry, region, true, visuals)
                    .into_iter()
                    .rev()
                    .map(SceneElement::Border)
                    .collect(),
            )
        }
        crate::capture::CaptureOverlay::Highlight(region) => {
            Ok(
                capture_picker_elements(output_geometry, region, false, visuals)
                    .into_iter()
                    .rev()
                    .map(SceneElement::Border)
                    .collect(),
            )
        }
        crate::capture::CaptureOverlay::Menu {
            output_name,
            selected,
            hovered,
            window_available,
        } if output.name() == output_name => Ok(crate::render::overlays::capture::menu_elements(
            renderer,
            node_renderer,
            output_geometry,
            selected,
            hovered,
            window_available,
            visuals,
        )?
        .into_iter()
        .rev()
        .map(SceneElement::CaptureOverlay)
        .collect()),
        crate::capture::CaptureOverlay::Menu { .. } => Ok(Vec::new()),
    }
}

pub(super) fn capture_picker_elements(
    output: Rectangle<i32, Logical>,
    selection: Rectangle<i32, Logical>,
    region_style: bool,
    visuals: crate::render::overlays::shell::OverlayVisuals,
) -> Vec<SolidColorRenderElement> {
    let output_local = Rectangle::<i32, Physical>::from_size(output.size.to_physical(1));
    let selected = output.intersection(selection).map(|intersection| {
        Rectangle::<i32, Physical>::new(
            (intersection.loc - output.loc).to_physical(1),
            intersection.size.to_physical(1),
        )
    });
    let dim = smithay::backend::renderer::Color32F::new(
        visuals.fill.r,
        visuals.fill.g,
        visuals.fill.b,
        0.38 * visuals.fill.a,
    );
    let accent = smithay::backend::renderer::Color32F::new(
        visuals.border.r,
        visuals.border.g,
        visuals.border.b,
        visuals.border.a,
    );
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
                .map(|rect| make(rect, accent)),
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
                accent,
            ));
        }
    } else {
        elements.extend(
            inner_border_rects(selected, 2)
                .into_iter()
                .map(|rect| make(rect, accent)),
        );
    }
    elements
}

pub(super) fn inner_border_rects(
    rect: Rectangle<i32, Physical>,
    width: i32,
) -> [Rectangle<i32, Physical>; 4] {
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

pub(super) fn dashed_border_rects(rect: Rectangle<i32, Physical>) -> Vec<Rectangle<i32, Physical>> {
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
