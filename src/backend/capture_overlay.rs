use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Logical, Physical, Rectangle};

pub fn menu_elements(
    output: Rectangle<i32, Logical>,
    selected: usize,
    hovered: Option<usize>,
    window_available: bool,
    highlight: Color32F,
) -> Vec<SolidColorRenderElement> {
    let layout = crate::capture::menu::layout(output);
    let localize = |rectangle: Rectangle<i32, Logical>| {
        Rectangle::<i32, Physical>::new(
            (rectangle.loc - output.loc).to_physical(1),
            rectangle.size.to_physical(1),
        )
    };
    let make = |geometry, color| {
        SolidColorRenderElement::new(
            Id::new(),
            geometry,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
    };
    let mut elements = Vec::new();
    let bar = localize(layout.bar);
    elements.push(make(bar, Color32F::new(0.055, 0.065, 0.075, 0.96)));
    elements.extend(super::border_strips(bar, 2, highlight));

    for (index, item) in layout.items.into_iter().enumerate() {
        let disabled = index == 2 && !window_available;
        let active = !disabled && (selected == index || hovered == Some(index));
        let item = localize(item);
        let fill = if disabled {
            Color32F::new(0.10, 0.11, 0.12, 0.50)
        } else if active {
            Color32F::new(0.14, 0.16, 0.18, 0.98)
        } else {
            Color32F::new(0.09, 0.105, 0.12, 0.94)
        };
        let accent = if disabled {
            Color32F::new(0.35, 0.37, 0.39, 0.42)
        } else if active {
            highlight
        } else {
            Color32F::new(0.60, 0.63, 0.66, 0.72)
        };
        elements.push(make(item, fill));
        elements.extend(super::border_strips(item, 2, accent));
        push_icon(&mut elements, item, index, accent, &make);
    }
    elements
}

fn push_icon(
    elements: &mut Vec<SolidColorRenderElement>,
    item: Rectangle<i32, Physical>,
    index: usize,
    color: Color32F,
    make: &impl Fn(Rectangle<i32, Physical>, Color32F) -> SolidColorRenderElement,
) {
    let size = 38.min(item.size.w - 12).min(item.size.h - 12).max(12);
    let icon = Rectangle::<i32, Physical>::new(
        (
            item.loc.x + (item.size.w - size) / 2,
            item.loc.y + (item.size.h - size) / 2,
        )
            .into(),
        (size, size).into(),
    );
    let line = 3;
    let add = |elements: &mut Vec<_>, rectangle| elements.push(make(rectangle, color));
    match index {
        0 => {
            let arm = size / 3;
            for rectangle in [
                Rectangle::new(icon.loc, (arm, line).into()),
                Rectangle::new(icon.loc, (line, arm).into()),
                Rectangle::new(
                    (icon.loc.x + size - arm, icon.loc.y).into(),
                    (arm, line).into(),
                ),
                Rectangle::new(
                    (icon.loc.x + size - line, icon.loc.y).into(),
                    (line, arm).into(),
                ),
                Rectangle::new(
                    (icon.loc.x, icon.loc.y + size - line).into(),
                    (arm, line).into(),
                ),
                Rectangle::new(
                    (icon.loc.x, icon.loc.y + size - arm).into(),
                    (line, arm).into(),
                ),
                Rectangle::new(
                    (icon.loc.x + size - arm, icon.loc.y + size - line).into(),
                    (arm, line).into(),
                ),
                Rectangle::new(
                    (icon.loc.x + size - line, icon.loc.y + size - arm).into(),
                    (line, arm).into(),
                ),
            ] {
                add(elements, rectangle);
            }
        }
        1 => {
            elements.extend(super::border_strips(
                Rectangle::new(icon.loc, (size, size - 8).into()),
                line,
                color,
            ));
            add(
                elements,
                Rectangle::new(
                    (icon.loc.x + size / 2 - line / 2, icon.loc.y + size - 8).into(),
                    (line, 6).into(),
                ),
            );
            add(
                elements,
                Rectangle::new(
                    (icon.loc.x + size / 4, icon.loc.y + size - 3).into(),
                    (size / 2, line).into(),
                ),
            );
        }
        _ => {
            elements.extend(super::border_strips(icon, line, color));
            add(
                elements,
                Rectangle::new(
                    (icon.loc.x + line, icon.loc.y + size / 4).into(),
                    (size - line * 2, line).into(),
                ),
            );
        }
    }
}
