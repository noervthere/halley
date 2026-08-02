use halley_config::{TitlebarButtonPosition, Titlebars};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::utils::Rectangle;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;

pub const MIN_CONTENT_HEIGHT: i32 = 24;
pub const TITLE_VERTICAL_PADDING: i32 = 8;
pub const TITLE_HORIZONTAL_PADDING: i32 = 8;
pub const APP_ICON_SIZE: i32 = 16;
pub const APP_ICON_SLOT: i32 = 24;
pub const BUTTON_GLYPH_MAX: i32 = 16;
pub const BUTTON_GLYPH_PADDING: i32 = 6;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Control {
    Close,
    Minimize,
    Maximize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hit {
    Drag,
    Control(Control),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ButtonTarget {
    pub window: Window,
    pub control: Control,
}

#[derive(Clone, Debug)]
pub struct LastClick {
    pub surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    pub at: std::time::Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlGeometry<K> {
    pub control: Control,
    pub rect: Rectangle<i32, K>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecorationLayout<K> {
    pub content: Rectangle<i32, K>,
    pub titlebar: Rectangle<i32, K>,
    pub body_outer: Rectangle<i32, K>,
    pub outer: Rectangle<i32, K>,
    pub controls: Vec<ControlGeometry<K>>,
    pub app_icon: Option<Rectangle<i32, K>>,
    pub title_clip: Rectangle<i32, K>,
    pub border_width: i32,
    pub titlebar_height: i32,
}

impl<K> DecorationLayout<K> {
    pub fn new(
        content: Rectangle<i32, K>,
        border_width: i32,
        titlebar_height: i32,
        config: &Titlebars,
    ) -> Self {
        let border_width = border_width.max(0);
        let titlebar_height = titlebar_height.max(1);
        let outer_width = (content.size.w + border_width * 2).max(1);
        let titlebar = Rectangle::new(
            (
                content.loc.x - border_width,
                content.loc.y - titlebar_height,
            )
                .into(),
            (outer_width, titlebar_height).into(),
        );
        let body_outer = Rectangle::new(
            (content.loc.x - border_width, content.loc.y).into(),
            (outer_width, (content.size.h + border_width).max(1)).into(),
        );
        let outer = titlebar.merge(body_outer);

        let controls = control_geometry(titlebar, config);
        let left_controls_width =
            if config.show_buttons && config.button_position == TitlebarButtonPosition::Left {
                titlebar_height * 3
            } else {
                0
            };
        let right_controls_width =
            if config.show_buttons && config.button_position == TitlebarButtonPosition::Right {
                titlebar_height * 3
            } else {
                0
            };
        let app_icon = config.show_icons.then(|| {
            let slot_x = titlebar.loc.x + left_controls_width;
            Rectangle::new(
                (
                    slot_x + (APP_ICON_SLOT - APP_ICON_SIZE) / 2,
                    titlebar.loc.y + (titlebar_height - APP_ICON_SIZE) / 2,
                )
                    .into(),
                (APP_ICON_SIZE, APP_ICON_SIZE).into(),
            )
        });
        let left_occupied = left_controls_width + if config.show_icons { APP_ICON_SLOT } else { 0 };
        let exclusion = left_occupied.max(right_controls_width) + TITLE_HORIZONTAL_PADDING;
        let title_width = (titlebar.size.w - exclusion * 2).max(0);
        let title_clip = Rectangle::new(
            (titlebar.loc.x + exclusion, titlebar.loc.y).into(),
            (title_width, titlebar_height).into(),
        );

        Self {
            content,
            titlebar,
            body_outer,
            outer,
            controls,
            app_icon,
            title_clip,
            border_width,
            titlebar_height,
        }
    }

    pub fn hit(&self, point: smithay::utils::Point<f64, K>) -> Option<Hit> {
        if !self.titlebar.to_f64().contains(point) {
            return None;
        }
        self.controls
            .iter()
            .find(|control| control.rect.to_f64().contains(point))
            .map(|control| Hit::Control(control.control))
            .or(Some(Hit::Drag))
    }
}

fn control_geometry<K>(titlebar: Rectangle<i32, K>, config: &Titlebars) -> Vec<ControlGeometry<K>> {
    if !config.show_buttons {
        return Vec::new();
    }
    let controls = match config.button_position {
        TitlebarButtonPosition::Left => [Control::Close, Control::Maximize, Control::Minimize],
        TitlebarButtonPosition::Right => [Control::Minimize, Control::Maximize, Control::Close],
    };
    controls
        .into_iter()
        .enumerate()
        .map(|(index, control)| {
            let x = match config.button_position {
                TitlebarButtonPosition::Left => titlebar.loc.x + index as i32 * titlebar.size.h,
                TitlebarButtonPosition::Right => {
                    titlebar.loc.x + titlebar.size.w
                        - (controls.len() as i32 - index as i32) * titlebar.size.h
                }
            };
            ControlGeometry {
                control,
                rect: Rectangle::new(
                    (x, titlebar.loc.y).into(),
                    (titlebar.size.h, titlebar.size.h).into(),
                ),
            }
        })
        .collect()
}

pub fn effective_height(config: &Titlebars, font_size_px: u16) -> i32 {
    let mut required = 1;
    if config.show_buttons || config.show_icons {
        required = required.max(MIN_CONTENT_HEIGHT);
    }
    if config.show_title {
        let line_height = (f32::from(font_size_px.max(1)) * 1.25).ceil() as i32;
        required = required.max(line_height + TITLE_VERTICAL_PADDING);
    }
    config.height_px.max(required).clamp(1, 96)
}

pub fn glyph_size(titlebar_height: i32) -> i32 {
    BUTTON_GLYPH_MAX
        .min(titlebar_height - BUTTON_GLYPH_PADDING * 2)
        .max(1)
}

pub fn uses_server_titlebar(window: &Window, config: &Titlebars) -> bool {
    if !config.enabled || crate::xwayland::is_override_redirect(window) {
        return false;
    }
    if let Some(toplevel) = window.toplevel() {
        return toplevel.with_committed_state(|state| {
            state.and_then(|state| state.decoration_mode) == Some(Mode::ServerSide)
        });
    }
    crate::xwayland::uses_server_decorations(window)
}

pub fn control_enabled(window: &Window, control: Control) -> bool {
    if control != Control::Maximize {
        return true;
    }
    if let Some(toplevel) = window.toplevel() {
        return with_states(toplevel.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let state = cached.current();
            state.min_size != state.max_size
                || state.min_size.w == 0
                || state.min_size.h == 0
                || state.max_size.w == 0
                || state.max_size.h == 0
        });
    }
    crate::xwayland::can_maximize(window)
}

pub fn client_rect_for_outer(
    window: &Window,
    outer: Rectangle<i32, smithay::utils::Logical>,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
) -> Rectangle<i32, smithay::utils::Logical> {
    let border = decorations.border_width_px.max(0);
    if uses_server_titlebar(window, &decorations.titlebars) {
        let height = effective_height(&decorations.titlebars, font.size);
        Rectangle::new(
            (outer.loc.x + border, outer.loc.y + height).into(),
            (
                outer.size.w.saturating_sub(border.saturating_mul(2)).max(1),
                outer
                    .size
                    .h
                    .saturating_sub(height.saturating_add(border))
                    .max(1),
            )
                .into(),
        )
    } else {
        Rectangle::new(
            (outer.loc.x + border, outer.loc.y + border).into(),
            (
                outer.size.w.saturating_sub(border.saturating_mul(2)).max(1),
                outer.size.h.saturating_sub(border.saturating_mul(2)).max(1),
            )
                .into(),
        )
    }
}

pub fn outer_size_for_client(
    window: &Window,
    client: smithay::utils::Size<i32, smithay::utils::Logical>,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
) -> smithay::utils::Size<i32, smithay::utils::Logical> {
    let border = decorations.border_width_px.max(0);
    let vertical = if uses_server_titlebar(window, &decorations.titlebars) {
        effective_height(&decorations.titlebars, font.size).saturating_add(border)
    } else {
        border.saturating_mul(2)
    };
    (
        client.w.saturating_add(border.saturating_mul(2)).max(1),
        client.h.saturating_add(vertical).max(1),
    )
        .into()
}

pub fn client_location_for_outer(
    window: &Window,
    outer: smithay::utils::Point<i32, smithay::utils::Logical>,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
) -> smithay::utils::Point<i32, smithay::utils::Logical> {
    let border = decorations.border_width_px.max(0);
    let top = if uses_server_titlebar(window, &decorations.titlebars) {
        effective_height(&decorations.titlebars, font.size)
    } else {
        border
    };
    (outer.x.saturating_add(border), outer.y.saturating_add(top)).into()
}

#[cfg(test)]
mod tests {
    use smithay::utils::{Logical, Point, Rectangle};

    use super::*;

    #[test]
    fn left_buttons_keep_close_at_the_outer_edge() {
        let config = Titlebars::default();
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((100, 100).into(), (800, 600).into()),
            3,
            32,
            &config,
        );

        assert_eq!(
            layout.outer,
            Rectangle::new((97, 68).into(), (806, 635).into())
        );
        assert_eq!(layout.controls[0].control, Control::Close);
        assert_eq!(layout.controls[1].control, Control::Maximize);
        assert_eq!(layout.controls[2].control, Control::Minimize);
        assert_eq!(layout.controls[0].rect.loc, Point::from((97, 68)));
        assert_eq!(layout.content.loc, Point::from((100, 100)));
    }

    #[test]
    fn right_buttons_keep_close_at_the_outer_edge() {
        let config = Titlebars {
            button_position: TitlebarButtonPosition::Right,
            ..Titlebars::default()
        };
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((0, 32).into(), (300, 200).into()),
            0,
            32,
            &config,
        );

        assert_eq!(layout.controls[2].control, Control::Close);
        assert_eq!(
            layout.controls[2].rect,
            Rectangle::new((268, 0).into(), (32, 32).into())
        );
    }

    #[test]
    fn enabled_content_raises_but_never_exceeds_height_cap() {
        let compact = Titlebars {
            height_px: 1,
            show_title: false,
            ..Titlebars::default()
        };
        assert_eq!(effective_height(&compact, 11), 24);
        let text_only = Titlebars {
            height_px: 1,
            show_buttons: false,
            show_icons: false,
            ..Titlebars::default()
        };
        assert_eq!(effective_height(&text_only, 40), 58);
        assert_eq!(effective_height(&text_only, 200), 96);
    }

    #[test]
    fn empty_titlebar_may_use_the_raw_minimum() {
        let config = Titlebars {
            height_px: 1,
            show_buttons: false,
            show_icons: false,
            show_title: false,
            ..Titlebars::default()
        };
        assert_eq!(effective_height(&config, 80), 1);
    }

    #[test]
    fn controls_win_hit_testing_over_drag_region() {
        let config = Titlebars::default();
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((0, 32).into(), (300, 200).into()),
            0,
            32,
            &config,
        );
        assert_eq!(
            layout.hit(Point::from((10.0, 10.0))),
            Some(Hit::Control(Control::Close))
        );
        assert_eq!(layout.hit(Point::from((200.0, 10.0))), Some(Hit::Drag));
        assert_eq!(layout.hit(Point::from((200.0, 40.0))), None);
    }
}
