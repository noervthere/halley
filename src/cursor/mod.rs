mod theme;
mod visibility;

pub(crate) mod render;
pub(crate) mod snapshot;
pub(crate) mod surface;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Transform};
use xcursor::CursorTheme;

pub(crate) use theme::CursorFrame;
use theme::{PreparedCursor, load_prepared_cursor};
pub(crate) use visibility::{TimerDirective, Visibility};

pub(crate) enum RenderCursor {
    Hidden,
    Named(Rc<CursorFrame>),
    Surface {
        surface: WlSurface,
        snapshot: Option<Rc<CursorSurfaceSnapshot>>,
    },
}

pub(crate) struct CursorSurfaceSnapshot {
    pub surface: WlSurface,
    pub buffer: MemoryRenderBuffer,
    pub metadata_bgra: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub scale: i32,
    pub(crate) format: Fourcc,
    pub(crate) transform: Transform,
}

/// Resolves and caches cursor icons independently of the presentation
/// backend. The scene receives an already-selected frame and never needs to
/// know how themes, aliases, animation, or fallback work.
pub struct CursorManager {
    theme_name: String,
    theme: CursorTheme,
    default_theme: CursorTheme,
    size: u8,
    image: CursorImageStatus,
    override_icon: Option<CursorIcon>,
    cache: RefCell<HashMap<(CursorIcon, i32), Rc<PreparedCursor>>>,
    surface_snapshot: RefCell<Option<Rc<CursorSurfaceSnapshot>>>,
}

impl CursorManager {
    pub fn new(config: &halley_config::Cursor) -> Self {
        Self {
            theme_name: config.theme.clone(),
            theme: CursorTheme::load(&config.theme),
            default_theme: CursorTheme::load("default"),
            size: config.size,
            image: CursorImageStatus::default_named(),
            override_icon: None,
            cache: RefCell::new(HashMap::new()),
            surface_snapshot: RefCell::new(None),
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

    pub fn set_image(&mut self, image: CursorImageStatus) -> Option<WlSurface> {
        if self.image == image {
            return None;
        }
        let previous = match &self.image {
            CursorImageStatus::Surface(surface) => Some(surface.clone()),
            _ => None,
        };
        self.image = image;
        self.surface_snapshot.get_mut().take();
        previous
    }

    pub fn reset_image(&mut self) -> Option<WlSurface> {
        self.set_image(CursorImageStatus::default_named())
    }

    pub fn current_surface(&self) -> Option<&WlSurface> {
        if self.override_icon.is_some() {
            return None;
        }
        match &self.image {
            CursorImageStatus::Surface(surface) if surface.alive() => Some(surface),
            _ => None,
        }
    }

    pub fn surface_destroyed(&mut self, surface: &WlSurface) -> bool {
        if !matches!(&self.image, CursorImageStatus::Surface(current) if current == surface) {
            return false;
        }
        self.image = CursorImageStatus::default_named();
        self.surface_snapshot.get_mut().take();
        true
    }

    pub(crate) fn render_cursor(&self, output_scale: i32, time: Duration) -> RenderCursor {
        self.render_cursor_with_override(output_scale, time, None)
    }

    pub(crate) fn render_cursor_with_override(
        &self,
        output_scale: i32,
        time: Duration,
        presentation_override: Option<CursorIcon>,
    ) -> RenderCursor {
        if let Some(icon) = presentation_override.or(self.override_icon) {
            return RenderCursor::Named(self.frame(icon, output_scale, time));
        }
        match &self.image {
            CursorImageStatus::Hidden => RenderCursor::Hidden,
            CursorImageStatus::Named(icon) => {
                RenderCursor::Named(self.frame(*icon, output_scale, time))
            }
            CursorImageStatus::Surface(surface) if surface.alive() => {
                let snapshot = self
                    .surface_snapshot
                    .borrow()
                    .as_ref()
                    .filter(|snapshot| snapshot.surface == *surface)
                    .cloned();
                RenderCursor::Surface {
                    surface: surface.clone(),
                    snapshot,
                }
            }
            CursorImageStatus::Surface(_) => {
                RenderCursor::Named(self.default_frame(output_scale, time))
            }
        }
    }

    pub fn current_is_animated(&self, output_scale: i32) -> bool {
        self.current_is_animated_with_override(output_scale, None)
    }

    pub fn current_is_animated_with_override(
        &self,
        output_scale: i32,
        presentation_override: Option<CursorIcon>,
    ) -> bool {
        if let Some(icon) = presentation_override.or(self.override_icon) {
            return self.is_animated(icon, output_scale);
        }
        match &self.image {
            CursorImageStatus::Named(icon) => self.is_animated(*icon, output_scale),
            CursorImageStatus::Surface(_) | CursorImageStatus::Hidden => false,
        }
    }

    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    pub fn size(&self) -> u8 {
        self.size
    }

    pub fn set_override(&mut self, icon: Option<CursorIcon>) -> bool {
        if self.override_icon == icon {
            return false;
        }
        self.override_icon = icon;
        true
    }

    pub(crate) fn surface_snapshot(&self) -> &RefCell<Option<Rc<CursorSurfaceSnapshot>>> {
        &self.surface_snapshot
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

    #[test]
    fn client_cursor_status_is_retained_independently_of_theme_reload() {
        let config = halley_config::Cursor::default();
        let mut manager = CursorManager::new(&config);
        manager.set_image(CursorImageStatus::Named(CursorIcon::Text));

        let changed = halley_config::Cursor { size: 32, ..config };
        assert!(manager.reload(&changed));
        assert!(matches!(
            manager.image,
            CursorImageStatus::Named(CursorIcon::Text)
        ));
    }

    #[test]
    fn presentation_override_replaces_hidden_client_cursor_without_mutating_it() {
        let config = halley_config::Cursor::default();
        let mut manager = CursorManager::new(&config);
        manager.set_image(CursorImageStatus::Hidden);

        assert!(matches!(
            manager.render_cursor(1, Duration::ZERO),
            RenderCursor::Hidden
        ));
        assert!(matches!(
            manager.render_cursor_with_override(1, Duration::ZERO, Some(CursorIcon::Default)),
            RenderCursor::Named(_)
        ));
        assert!(matches!(
            manager.render_cursor(1, Duration::ZERO),
            RenderCursor::Hidden
        ));
    }

    #[test]
    fn reset_image_recovers_from_a_hidden_client_cursor() {
        let config = halley_config::Cursor::default();
        let mut manager = CursorManager::new(&config);
        manager.set_image(CursorImageStatus::Hidden);

        manager.reset_image();

        assert!(matches!(
            manager.image,
            CursorImageStatus::Named(CursorIcon::Default)
        ));
    }
}
