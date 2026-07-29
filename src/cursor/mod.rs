mod theme;

pub(crate) mod render;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use smithay::input::pointer::CursorIcon;
use xcursor::CursorTheme;

pub(crate) use theme::CursorFrame;
use theme::{PreparedCursor, load_prepared_cursor};

/// Resolves and caches cursor icons independently of the presentation
/// backend. The scene receives an already-selected frame and never needs to
/// know how themes, aliases, animation, or fallback work.
pub struct CursorManager {
    theme_name: String,
    theme: CursorTheme,
    default_theme: CursorTheme,
    size: u8,
    cache: RefCell<HashMap<(CursorIcon, i32), Rc<PreparedCursor>>>,
}

impl CursorManager {
    pub fn new(config: &halley_config::Cursor) -> Self {
        Self {
            theme_name: config.theme.clone(),
            theme: CursorTheme::load(&config.theme),
            default_theme: CursorTheme::load("default"),
            size: config.size,
            cache: RefCell::new(HashMap::new()),
        }
    }

    pub fn reload(&mut self, config: &halley_config::Cursor) -> bool {
        if self.theme_name == config.theme && self.size == config.size {
            return false;
        }
        self.theme_name.clone_from(&config.theme);
        self.theme = CursorTheme::load(&config.theme);
        self.default_theme = CursorTheme::load("default");
        self.size = config.size;
        self.cache.get_mut().clear();
        true
    }

    pub fn frame(&self, icon: CursorIcon, output_scale: i32, time: Duration) -> Rc<CursorFrame> {
        self.prepared(icon, output_scale).frame(time)
    }

    pub fn default_frame(&self, output_scale: i32, time: Duration) -> Rc<CursorFrame> {
        self.frame(CursorIcon::Default, output_scale, time)
    }

    pub fn is_animated(&self, icon: CursorIcon, output_scale: i32) -> bool {
        self.prepared(icon, output_scale).is_animated()
    }

    pub fn default_is_animated(&self, output_scale: i32) -> bool {
        self.is_animated(CursorIcon::Default, output_scale)
    }

    fn prepared(&self, icon: CursorIcon, output_scale: i32) -> Rc<PreparedCursor> {
        let output_scale = output_scale.max(1);
        self.cache
            .borrow_mut()
            .entry((icon, output_scale))
            .or_insert_with(|| {
                Rc::new(load_prepared_cursor(
                    &self.theme_name,
                    &self.theme,
                    &self.default_theme,
                    icon,
                    self.size,
                    output_scale,
                ))
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_only_invalidates_for_theme_or_size_changes() {
        let config = halley_config::Cursor::default();
        let mut manager = CursorManager::new(&config);

        assert!(!manager.reload(&config));

        let changed = halley_config::Cursor { size: 32, ..config };
        assert!(manager.reload(&changed));
        assert!(!manager.reload(&changed));
    }
}
