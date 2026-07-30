use std::error::Error;

use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Buffer, Logical, Physical, Rectangle};

use super::node::{LabelRenderElement, NodeRenderer};
use super::scene::SceneElement;
use super::text::UiTextRenderer;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayRgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl OverlayRgb {
    pub fn tuple(self) -> (f32, f32, f32, f32) {
        (self.r, self.g, self.b, self.a)
    }

    pub fn bytes(self) -> [u8; 3] {
        [
            (self.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        ]
    }

    pub fn mix(self, other: Self, amount: f32) -> Self {
        let t = amount.clamp(0.0, 1.0);
        Self {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    fn luminance(self) -> f32 {
        self.r * 0.2126 + self.g * 0.7152 + self.b * 0.0722
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OverlayVisuals {
    pub fill: OverlayRgb,
    pub text: OverlayRgb,
    pub error: OverlayRgb,
    pub subtext: OverlayRgb,
    pub key_fill: OverlayRgb,
    pub border: OverlayRgb,
    pub border_px: f32,
    pub shape: halley_config::OverlayShape,
}

impl OverlayVisuals {
    fn label_chrome(mut self) -> Self {
        self.border_px = 0.0;
        self
    }
}

const LIGHT_FILL: OverlayRgb = OverlayRgb {
    r: 0.92,
    g: 0.95,
    b: 0.98,
    a: 1.0,
};
const DARK_FILL: OverlayRgb = OverlayRgb {
    r: 0.15,
    g: 0.18,
    b: 0.22,
    a: 1.0,
};
const LIGHT_TEXT: OverlayRgb = OverlayRgb {
    r: 0.08,
    g: 0.10,
    b: 0.12,
    a: 1.0,
};
const DARK_TEXT: OverlayRgb = OverlayRgb {
    r: 0.94,
    g: 0.96,
    b: 0.98,
    a: 1.0,
};

pub fn resolve_visuals(
    config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
) -> OverlayVisuals {
    let fill = resolve_fill(config.background_color);
    let text = resolve_text(config.text_color, fill);
    let error = resolve_error(config.error_color);
    let border_color = decorations.border_color_focused;
    let border = OverlayRgb {
        r: border_color.r,
        g: border_color.g,
        b: border_color.b,
        a: 1.0,
    };
    OverlayVisuals {
        fill,
        text,
        error,
        subtext: text.mix(fill, 0.20),
        key_fill: fill.mix(text, 0.10),
        border,
        border_px: if config.borders {
            decorations.border_width_px.max(0) as f32
        } else {
            0.0
        },
        shape: config.shape,
    }
}

fn resolve_fill(mode: halley_config::OverlayColorMode) -> OverlayRgb {
    match mode {
        halley_config::OverlayColorMode::Auto | halley_config::OverlayColorMode::Light => {
            LIGHT_FILL
        }
        halley_config::OverlayColorMode::Dark => DARK_FILL,
        halley_config::OverlayColorMode::Fixed { r, g, b, a } => OverlayRgb { r, g, b, a },
    }
}

fn resolve_text(mode: halley_config::OverlayColorMode, fill: OverlayRgb) -> OverlayRgb {
    match mode {
        halley_config::OverlayColorMode::Auto => {
            if fill.luminance() < 0.45 {
                DARK_TEXT
            } else {
                LIGHT_TEXT
            }
        }
        halley_config::OverlayColorMode::Light => LIGHT_TEXT,
        halley_config::OverlayColorMode::Dark => DARK_TEXT,
        halley_config::OverlayColorMode::Fixed { r, g, b, a } => OverlayRgb { r, g, b, a },
    }
}

fn resolve_error(mode: halley_config::OverlayColorMode) -> OverlayRgb {
    match mode {
        halley_config::OverlayColorMode::Fixed { r, g, b, a } => OverlayRgb { r, g, b, a },
        halley_config::OverlayColorMode::Auto
        | halley_config::OverlayColorMode::Light
        | halley_config::OverlayColorMode::Dark => OverlayRgb {
            r: 0xfb as f32 / 255.0,
            g: 0x49 as f32 / 255.0,
            b: 0x34 as f32 / 255.0,
            a: 1.0,
        },
    }
}

pub fn card_element(
    renderer: &mut GlesRenderer,
    node_renderer: &mut NodeRenderer,
    destination: Rectangle<i32, Physical>,
    visuals: OverlayVisuals,
    fill: OverlayRgb,
    alpha: f32,
) -> Result<LabelRenderElement, Box<dyn Error>> {
    node_renderer.overlay_card_element(
        renderer,
        destination,
        visuals.shape,
        fill.tuple(),
        visuals.border.tuple(),
        visuals.border_px,
        alpha,
    )
}

/// Internal caption bands and badges were borderless in old Halley. They use
/// the shared overlay palette and shape, but never inherit the container
/// border setting.
pub fn label_card_element(
    renderer: &mut GlesRenderer,
    node_renderer: &mut NodeRenderer,
    destination: Rectangle<i32, Physical>,
    visuals: OverlayVisuals,
    fill: OverlayRgb,
    alpha: f32,
) -> Result<LabelRenderElement, Box<dyn Error>> {
    card_element(
        renderer,
        node_renderer,
        destination,
        visuals.label_chrome(),
        fill,
        alpha,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn elements(
    renderer: &mut GlesRenderer,
    output_geometry: Rectangle<i32, Logical>,
    snapshot: crate::overlay::OverlaySnapshot,
    config: &halley_config::Overlays,
    decorations: &halley_config::Decorations,
    node_renderer: &mut NodeRenderer,
    ui_text: &mut UiTextRenderer,
) -> Result<Vec<SceneElement>, Box<dyn Error>> {
    let visuals = resolve_visuals(config, decorations);
    let screen = Rectangle::<i32, Physical>::from_size(output_geometry.size.to_physical(1));
    let mut elements = Vec::new();
    if let Some(mix) = snapshot.exit_mix {
        exit_elements(
            renderer,
            screen,
            mix,
            visuals,
            node_renderer,
            ui_text,
            &mut elements,
        )?;
    }
    if let Some(notification) = snapshot.notification {
        notification_elements(
            renderer,
            screen,
            notification,
            config.notifications.position,
            visuals,
            node_renderer,
            ui_text,
            &mut elements,
        )?;
    }
    Ok(elements)
}

#[allow(clippy::too_many_arguments)]
fn exit_elements(
    renderer: &mut GlesRenderer,
    screen: Rectangle<i32, Physical>,
    mix: f32,
    visuals: OverlayVisuals,
    node_renderer: &mut NodeRenderer,
    ui_text: &mut UiTextRenderer,
    elements: &mut Vec<SceneElement>,
) -> Result<(), Box<dyn Error>> {
    const TITLE: &str = "Are you sure you want to leave?";
    const ACTIONS: [(&str, &str); 2] = [("Enter", "leave"), ("Esc", "cancel")];
    let title_size = ui_text
        .measure(renderer, TITLE, 2, visuals.text.bytes())?
        .unwrap_or((0, 0).into());
    let mut action_width = 0;
    let mut action_height = 0;
    let mut action_sizes = Vec::new();
    for (key, label) in ACTIONS {
        let key_size = ui_text
            .measure(renderer, key, 1, visuals.text.bytes())?
            .unwrap_or((0, 0).into());
        let label_size = ui_text
            .measure(renderer, label, 1, visuals.subtext.bytes())?
            .unwrap_or((0, 0).into());
        let chip_width = key_size.w + 16;
        action_width += chip_width + 8 + label_size.w + 22;
        action_height = action_height.max(key_size.h.max(label_size.h) + 8);
        action_sizes.push((key, label, key_size, label_size, chip_width));
    }
    action_width = action_width.saturating_sub(22);
    let card_width = (title_size.w.max(action_width) + 48)
        .max(280)
        .min((screen.size.w - 36).max(1));
    let card_height = title_size.h + action_height + 54;
    let card = Rectangle::new(
        (
            screen.loc.x + (screen.size.w - card_width) / 2,
            screen.loc.y + (screen.size.h - card_height) / 2,
        )
            .into(),
        (card_width, card_height).into(),
    );
    let title_x = card.loc.x + 24;
    let title_y = card.loc.y + 18;
    if let Some(text) = ui_text.element(
        renderer,
        (title_x, title_y).into(),
        TITLE,
        2,
        visuals.text.bytes(),
        mix,
    )? {
        elements.push(SceneElement::UiText(text.element));
    }
    let mut x = card.loc.x + 24;
    let action_y = card.loc.y + card.size.h - action_height - 16;
    for (key, label, key_size, label_size, chip_width) in action_sizes {
        let chip = Rectangle::new((x, action_y).into(), (chip_width, action_height).into());
        if let Some(text) = ui_text.element(
            renderer,
            (chip.loc.x + 8, chip.loc.y + (chip.size.h - key_size.h) / 2).into(),
            key,
            1,
            visuals.text.bytes(),
            mix,
        )? {
            elements.push(SceneElement::UiText(text.element));
        }
        elements.push(SceneElement::NodeLabel(card_element(
            renderer,
            node_renderer,
            chip,
            OverlayVisuals {
                border_px: 0.0,
                ..visuals
            },
            visuals.key_fill,
            0.96 * mix,
        )?));
        x += chip_width + 8;
        if let Some(text) = ui_text.element(
            renderer,
            (x, action_y + (action_height - label_size.h) / 2).into(),
            label,
            1,
            visuals.subtext.bytes(),
            mix,
        )? {
            elements.push(SceneElement::UiText(text.element));
        }
        x += label_size.w + 22;
    }
    elements.push(SceneElement::NodeLabel(card_element(
        renderer,
        node_renderer,
        card,
        visuals,
        visuals.fill,
        0.97 * mix,
    )?));
    elements.push(SceneElement::Border(SolidColorRenderElement::new(
        Id::new(),
        screen,
        CommitCounter::default(),
        Color32F::new(0.02, 0.03, 0.05, 0.62 * mix),
        smithay::backend::renderer::element::Kind::Unspecified,
    )));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn notification_elements(
    renderer: &mut GlesRenderer,
    screen: Rectangle<i32, Physical>,
    notification: crate::overlay::NotificationSnapshot,
    position: halley_config::NotificationPosition,
    visuals: OverlayVisuals,
    node_renderer: &mut NodeRenderer,
    ui_text: &mut UiTextRenderer,
    elements: &mut Vec<SceneElement>,
) -> Result<(), Box<dyn Error>> {
    let max_text_width = ((screen.size.w as f32 * 0.70).round() as i32 - 32).max(80);
    let color = match notification.kind {
        crate::overlay::NotificationKind::Success => visuals.text,
        crate::overlay::NotificationKind::Error => visuals.error,
    };
    let (message, text_size) = fit_middle(
        renderer,
        ui_text,
        &notification.message,
        color.bytes(),
        max_text_width,
    )?;
    let card =
        Rectangle::<i32, Physical>::new((0, 0).into(), (text_size.w + 32, text_size.h + 16).into());
    let margin = 24;
    let slide = ((1.0 - notification.mix) * 8.0).round() as i32;
    let x = match position {
        halley_config::NotificationPosition::TopLeft
        | halley_config::NotificationPosition::BottomLeft => margin,
        halley_config::NotificationPosition::TopCenter
        | halley_config::NotificationPosition::BottomCenter => (screen.size.w - card.size.w) / 2,
        halley_config::NotificationPosition::TopRight
        | halley_config::NotificationPosition::BottomRight => screen.size.w - card.size.w - margin,
    };
    let y = match position {
        halley_config::NotificationPosition::TopLeft
        | halley_config::NotificationPosition::TopCenter
        | halley_config::NotificationPosition::TopRight => margin - slide,
        halley_config::NotificationPosition::BottomLeft
        | halley_config::NotificationPosition::BottomCenter
        | halley_config::NotificationPosition::BottomRight => {
            screen.size.h - card.size.h - margin + slide
        }
    };
    let card = Rectangle::new((x, y).into(), card.size);
    if let Some(text) = ui_text.element(
        renderer,
        (
            card.loc.x + 16,
            card.loc.y + (card.size.h - text_size.h) / 2,
        )
            .into(),
        &message,
        1,
        color.bytes(),
        notification.mix,
    )? {
        elements.push(SceneElement::UiText(text.element));
    }
    elements.push(SceneElement::NodeLabel(card_element(
        renderer,
        node_renderer,
        card,
        visuals,
        visuals.fill,
        0.97 * notification.mix,
    )?));
    Ok(())
}

fn fit_middle(
    renderer: &mut GlesRenderer,
    ui_text: &mut UiTextRenderer,
    value: &str,
    color: [u8; 3],
    max_width: i32,
) -> Result<(String, smithay::utils::Size<i32, Buffer>), Box<dyn Error>> {
    let measured = ui_text
        .measure(renderer, value, 1, color)?
        .unwrap_or((0, 0).into());
    if measured.w <= max_width {
        return Ok((value.to_string(), measured));
    }
    let chars = value.chars().collect::<Vec<_>>();
    for keep in (1..chars.len()).rev() {
        let left = keep.div_ceil(2);
        let right = keep / 2;
        let candidate = format!(
            "{}…{}",
            chars[..left].iter().collect::<String>(),
            chars[chars.len() - right..].iter().collect::<String>()
        );
        let measured = ui_text
            .measure(renderer, &candidate, 1, color)?
            .unwrap_or((0, 0).into());
        if measured.w <= max_width {
            return Ok((candidate, measured));
        }
    }
    Ok(("…".to_string(), (max_width.min(8), measured.h).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_auto_palette_is_light_with_dark_text() {
        let visuals = resolve_visuals(
            &halley_config::Overlays::default(),
            &halley_config::Decorations::default(),
        );
        assert_eq!(visuals.fill, LIGHT_FILL);
        assert_eq!(visuals.text, LIGHT_TEXT);
        assert_eq!(visuals.shape, halley_config::OverlayShape::Square);
    }

    #[test]
    fn dark_background_makes_auto_text_light() {
        let config = halley_config::Overlays {
            background_color: halley_config::OverlayColorMode::Dark,
            ..halley_config::Overlays::default()
        };
        assert_eq!(
            resolve_visuals(&config, &halley_config::Decorations::default()).text,
            DARK_TEXT
        );
    }

    #[test]
    fn internal_label_chrome_never_inherits_container_borders() {
        let visuals = resolve_visuals(
            &halley_config::Overlays::default(),
            &halley_config::Decorations::default(),
        );

        assert!(visuals.border_px > 0.0);
        assert_eq!(visuals.label_chrome().border_px, 0.0);
    }
}
