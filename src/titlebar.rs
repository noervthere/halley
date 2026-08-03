use halley_config::{TitlebarButtonPosition, TitlebarContentPosition, Titlebars};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::utils::Rectangle;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;

pub const MIN_CONTENT_HEIGHT: i32 = 24;
pub const TITLE_VERTICAL_PADDING: i32 = 8;
pub const TITLE_HORIZONTAL_PADDING: i32 = 8;
pub const APP_ICON_SIZE: i32 = 16;
pub const APP_ICON_GAP: i32 = 8;
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
    pub identity_area: Rectangle<i32, K>,
    pub border_width: i32,
    pub titlebar_height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IdentityLayout<K> {
    pub group: Rectangle<i32, K>,
    pub app_icon: Option<Rectangle<i32, K>>,
    pub title: Option<Rectangle<i32, K>>,
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
        let identity_x = titlebar.loc.x + left_controls_width + TITLE_HORIZONTAL_PADDING;
        let identity_right =
            titlebar.loc.x + titlebar.size.w - right_controls_width - TITLE_HORIZONTAL_PADDING;
        let identity_area = Rectangle::new(
            (identity_x, titlebar.loc.y).into(),
            ((identity_right - identity_x).max(0), titlebar_height).into(),
        );

        Self {
            content,
            titlebar,
            body_outer,
            outer,
            controls,
            identity_area,
            border_width,
            titlebar_height,
        }
    }

    pub fn max_title_width_scaled(&self, has_icon: bool, scale: f32) -> i32 {
        self.max_title_width_with_metrics(
            has_icon,
            scaled_identity_metric(APP_ICON_SIZE, scale),
            scaled_identity_metric(APP_ICON_GAP, scale),
        )
    }

    pub fn identity_layout_scaled(
        &self,
        position: TitlebarContentPosition,
        title_size: Option<(i32, i32)>,
        has_icon: bool,
        scale: f32,
    ) -> IdentityLayout<K> {
        self.identity_layout_with_metrics(
            position,
            title_size,
            has_icon,
            scaled_identity_metric(APP_ICON_SIZE, scale),
            scaled_identity_metric(APP_ICON_GAP, scale),
        )
    }

    fn max_title_width_with_metrics(&self, has_icon: bool, icon_size: i32, gap: i32) -> i32 {
        let icon_width = if has_icon { icon_size + gap } else { 0 };
        self.identity_area.size.w.saturating_sub(icon_width).max(0)
    }

    fn identity_layout_with_metrics(
        &self,
        position: TitlebarContentPosition,
        title_size: Option<(i32, i32)>,
        has_icon: bool,
        icon_size: i32,
        icon_gap: i32,
    ) -> IdentityLayout<K> {
        let has_icon = has_icon && self.identity_area.size.w >= icon_size;
        let title_size = title_size
            .filter(|(width, height)| *width > 0 && *height > 0)
            .and_then(|(width, height)| {
                let width =
                    width.min(self.max_title_width_with_metrics(has_icon, icon_size, icon_gap));
                (width > 0).then_some((width, height))
            });
        let gap = if has_icon && title_size.is_some() {
            icon_gap
        } else {
            0
        };
        let title_width = title_size.map_or(0, |size| size.0);
        let group_width = (if has_icon { icon_size } else { 0 }) + gap + title_width;
        let group_x = match position {
            TitlebarContentPosition::Left => self.identity_area.loc.x,
            TitlebarContentPosition::Center => {
                self.identity_area.loc.x + (self.identity_area.size.w - group_width) / 2
            }
            TitlebarContentPosition::Right => {
                self.identity_area.loc.x + self.identity_area.size.w - group_width
            }
        };
        let group = Rectangle::new(
            (group_x, self.titlebar.loc.y).into(),
            (group_width.max(0), self.titlebar.size.h).into(),
        );
        let app_icon = has_icon.then(|| {
            Rectangle::new(
                (
                    group_x,
                    self.titlebar.loc.y + (self.titlebar.size.h - icon_size) / 2,
                )
                    .into(),
                (icon_size, icon_size).into(),
            )
        });
        let title_x = group_x + if has_icon { icon_size + gap } else { 0 };
        let title = title_size.map(|(width, height)| {
            Rectangle::new(
                (
                    title_x,
                    self.titlebar.loc.y + (self.titlebar.size.h - height) / 2,
                )
                    .into(),
                (width, height).into(),
            )
        });
        IdentityLayout {
            group,
            app_icon,
            title,
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

fn scaled_identity_metric(base: i32, scale: f32) -> i32 {
    crate::render::window_decoration::scaled_metric(base, scale)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderedMetrics {
    pub height: i32,
    pub glyph_size: i32,
    pub radius: i32,
}

/// Scale titlebar metrics from their native values as a unit. In particular,
/// glyph padding belongs to the native titlebar: subtracting it after the
/// titlebar has already been scaled makes controls collapse much faster than
/// their window while zooming out.
pub fn rendered_metrics(config: &Titlebars, font_size_px: u16, scale: f32) -> RenderedMetrics {
    let native_height = effective_height(config, font_size_px);
    RenderedMetrics {
        height: crate::render::window_decoration::scaled_metric(native_height, scale),
        glyph_size: crate::render::window_decoration::scaled_metric(
            glyph_size(native_height),
            scale,
        ),
        radius: crate::render::window_decoration::scaled_metric(config.radius_px, scale),
    }
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

pub fn outer_rect_for_client(
    window: &Window,
    client: Rectangle<i32, smithay::utils::Logical>,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
) -> Rectangle<i32, smithay::utils::Logical> {
    let border = decorations.border_width_px.max(0);
    if uses_server_titlebar(window, &decorations.titlebars) {
        let height = effective_height(&decorations.titlebars, font.size);
        Rectangle::new(
            (client.loc.x - border, client.loc.y - height).into(),
            (
                client
                    .size
                    .w
                    .saturating_add(border.saturating_mul(2))
                    .max(1),
                client
                    .size
                    .h
                    .saturating_add(height.saturating_add(border))
                    .max(1),
            )
                .into(),
        )
    } else {
        Rectangle::new(
            (client.loc.x - border, client.loc.y - border).into(),
            (
                client
                    .size
                    .w
                    .saturating_add(border.saturating_mul(2))
                    .max(1),
                client
                    .size
                    .h
                    .saturating_add(border.saturating_mul(2))
                    .max(1),
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
    fn button_glyph_scales_one_for_one_with_the_titlebar() {
        let config = Titlebars::default();
        let native = rendered_metrics(&config, 15, 1.0);
        assert_eq!(native.height, 32);
        assert_eq!(native.glyph_size, 16);

        let half = rendered_metrics(&config, 15, 0.5);
        assert_eq!(half.height, 16);
        assert_eq!(half.glyph_size, 8);

        let intermediate = rendered_metrics(&config, 15, 0.8);
        assert_eq!(intermediate.height, 26);
        assert_eq!(intermediate.glyph_size, 13);
    }

    #[test]
    fn titlebar_identity_metrics_shrink_with_zoom() {
        let config = Titlebars {
            button_position: TitlebarButtonPosition::Right,
            ..Titlebars::default()
        };
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((0, 16).into(), (200, 100).into()),
            0,
            16,
            &config,
        );

        let identity =
            layout.identity_layout_scaled(TitlebarContentPosition::Left, Some((60, 9)), true, 0.5);
        let icon = identity.app_icon.expect("scaled icon fits");
        let title = identity.title.expect("scaled title fits");

        assert_eq!(icon.size, (8, 8).into());
        assert_eq!(title.loc.x - (icon.loc.x + icon.size.w), 4);
        assert_eq!(identity.group.size.w, 8 + 4 + 60);
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

    #[test]
    fn title_and_icon_move_as_one_group() {
        let config = Titlebars {
            button_position: TitlebarButtonPosition::Right,
            ..Titlebars::default()
        };
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((0, 32).into(), (400, 200).into()),
            0,
            32,
            &config,
        );

        for position in [
            TitlebarContentPosition::Left,
            TitlebarContentPosition::Center,
            TitlebarContentPosition::Right,
        ] {
            let identity = layout.identity_layout_scaled(position, Some((120, 18)), true, 1.0);
            let icon = identity.app_icon.expect("icon fits");
            let title = identity.title.expect("title fits");
            assert_eq!(title.loc.x - (icon.loc.x + icon.size.w), APP_ICON_GAP);
            assert!(identity.group.loc.x >= layout.identity_area.loc.x);
            assert!(
                identity.group.loc.x + identity.group.size.w
                    <= layout.identity_area.loc.x + layout.identity_area.size.w
            );
        }
    }

    #[test]
    fn title_position_aligns_group_with_the_control_free_area() {
        let config = Titlebars::default();
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((0, 32).into(), (400, 200).into()),
            0,
            32,
            &config,
        );
        let left = layout.identity_layout_scaled(
            TitlebarContentPosition::Left,
            Some((100, 18)),
            false,
            1.0,
        );
        let center = layout.identity_layout_scaled(
            TitlebarContentPosition::Center,
            Some((100, 18)),
            false,
            1.0,
        );
        let right = layout.identity_layout_scaled(
            TitlebarContentPosition::Right,
            Some((100, 18)),
            false,
            1.0,
        );

        assert_eq!(left.group.loc.x, layout.identity_area.loc.x);
        assert_eq!(
            center.group.loc.x,
            layout.identity_area.loc.x + (layout.identity_area.size.w - 100) / 2
        );
        assert_eq!(
            right.group.loc.x + right.group.size.w,
            layout.identity_area.loc.x + layout.identity_area.size.w
        );
    }

    #[test]
    fn narrow_titlebar_never_places_identity_over_controls() {
        let config = Titlebars::default();
        let layout = DecorationLayout::<Logical>::new(
            Rectangle::new((0, 32).into(), (80, 200).into()),
            0,
            32,
            &config,
        );
        let identity = layout.identity_layout_scaled(
            TitlebarContentPosition::Center,
            Some((200, 18)),
            true,
            1.0,
        );

        assert_eq!(layout.identity_area.size.w, 0);
        assert_eq!(identity.group.size.w, 0);
        assert!(identity.app_icon.is_none());
        assert!(identity.title.is_none());
    }
}
