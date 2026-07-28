use std::collections::HashMap;

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::seat::WaylandFocus;
use smithay::xwayland::X11Surface;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WindowKey {
    Xdg(WlSurface),
    X11(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Xdg,
    X11,
    X11OverrideRedirect,
}

impl WindowKind {
    pub fn is_managed(self) -> bool {
        !matches!(self, Self::X11OverrideRedirect)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub location: Point<i32, Logical>,
    pub output: Option<String>,
}

#[derive(Clone)]
pub struct MapTransition {
    pub key: WindowKey,
    pub window: Window,
    pub kind: WindowKind,
    pub generation: u64,
    pub first_map: bool,
    pub placement: Option<Placement>,
}

#[derive(Clone)]
pub struct UnmapTransition {
    pub key: WindowKey,
    pub window: Window,
    pub kind: WindowKind,
    pub surface: Option<WlSurface>,
}

struct WindowRecord {
    window: Window,
    kind: WindowKind,
    surface: Option<WlSurface>,
    mapping: MappingState,
    placement: Option<Placement>,
}

#[derive(Default)]
struct MappingState {
    mapped: bool,
    ever_mapped: bool,
    generation: u64,
    finalized_generation: Option<u64>,
    configured_generation: Option<u64>,
}

impl MappingState {
    fn begin(&mut self) -> Option<(u64, bool)> {
        if self.mapped {
            return None;
        }
        self.mapped = true;
        self.generation = self.generation.saturating_add(1);
        self.finalized_generation = None;
        Some((self.generation, !self.ever_mapped))
    }

    fn finalize(&mut self) -> Option<(u64, bool)> {
        if !self.mapped || self.finalized_generation == Some(self.generation) {
            return None;
        }
        self.finalized_generation = Some(self.generation);
        let first_map = !self.ever_mapped;
        self.ever_mapped = true;
        Some((self.generation, first_map))
    }

    fn unmap(&mut self) -> bool {
        if !self.mapped {
            return false;
        }
        self.mapped = false;
        self.finalized_generation = None;
        true
    }

    fn needs_configure(&self) -> bool {
        !self.mapped && self.configured_generation != Some(self.generation.saturating_add(1))
    }

    fn mark_configured(&mut self) {
        if !self.mapped {
            self.configured_generation = Some(self.generation.saturating_add(1));
        }
    }
}

#[derive(Default)]
pub struct WindowLifecycle {
    records: HashMap<WindowKey, WindowRecord>,
}

impl WindowLifecycle {
    pub fn register_xdg(&mut self, surface: WlSurface, window: Window) {
        self.records.insert(
            WindowKey::Xdg(surface.clone()),
            WindowRecord {
                window,
                kind: WindowKind::Xdg,
                surface: Some(surface),
                mapping: MappingState::default(),
                placement: None,
            },
        );
    }

    pub fn register_x11(&mut self, surface: X11Surface, kind: WindowKind) {
        let key = WindowKey::X11(surface.window_id());
        self.records.entry(key).or_insert_with(|| WindowRecord {
            window: Window::new_x11_window(surface),
            kind,
            surface: None,
            mapping: MappingState::default(),
            placement: None,
        });
    }

    pub fn ensure_x11(&mut self, surface: &X11Surface, kind: WindowKind) -> WindowKey {
        let key = WindowKey::X11(surface.window_id());
        self.records
            .entry(key.clone())
            .or_insert_with(|| WindowRecord {
                window: Window::new_x11_window(surface.clone()),
                kind,
                surface: None,
                mapping: MappingState::default(),
                placement: None,
            });
        key
    }

    pub fn associate_x11(
        &mut self,
        surface: &X11Surface,
        wl_surface: WlSurface,
    ) -> Option<WlSurface> {
        let record = self.records.get_mut(&Self::x11_key(surface))?;
        record.surface.replace(wl_surface)
    }

    pub fn xdg_key(surface: &WlSurface) -> WindowKey {
        WindowKey::Xdg(surface.clone())
    }

    pub fn x11_key(surface: &X11Surface) -> WindowKey {
        WindowKey::X11(surface.window_id())
    }

    pub fn window(&self, key: &WindowKey) -> Option<&Window> {
        self.records.get(key).map(|record| &record.window)
    }

    pub fn window_for_wl_surface(&self, surface: &WlSurface) -> Option<&Window> {
        self.records.values().find_map(|record| {
            let matches = record.surface.as_ref() == Some(surface)
                || record
                    .window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == surface);
            matches.then_some(&record.window)
        })
    }

    pub fn key_for_wl_surface(&self, surface: &WlSurface) -> Option<WindowKey> {
        self.records.iter().find_map(|(key, record)| {
            let matches = record.surface.as_ref() == Some(surface)
                || record
                    .window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == surface);
            matches.then(|| key.clone())
        })
    }

    pub fn is_mapped(&self, key: &WindowKey) -> bool {
        self.records
            .get(key)
            .is_some_and(|record| record.mapping.mapped)
    }

    pub fn needs_configure(&self, key: &WindowKey) -> bool {
        self.records
            .get(key)
            .is_some_and(|record| record.mapping.needs_configure())
    }

    pub fn mark_configured(&mut self, key: &WindowKey) {
        if let Some(record) = self.records.get_mut(key) {
            record.mapping.mark_configured();
        }
    }

    pub fn begin_map(&mut self, key: &WindowKey) -> Option<MapTransition> {
        let record = self.records.get_mut(key)?;
        let (generation, first_map) = record.mapping.begin()?;
        Some(MapTransition {
            key: key.clone(),
            window: record.window.clone(),
            kind: record.kind,
            generation,
            first_map,
            placement: record.placement.clone(),
        })
    }

    pub fn finalize_map(&mut self, key: &WindowKey) -> Option<MapTransition> {
        let record = self.records.get_mut(key)?;
        let (generation, first_map) = record.mapping.finalize()?;
        Some(MapTransition {
            key: key.clone(),
            window: record.window.clone(),
            kind: record.kind,
            generation,
            first_map,
            placement: record.placement.clone(),
        })
    }

    pub fn unmap(
        &mut self,
        key: &WindowKey,
        placement: Option<Placement>,
    ) -> Option<UnmapTransition> {
        let record = self.records.get_mut(key)?;
        if !record.mapping.unmap() {
            return None;
        }
        if placement.is_some() {
            record.placement = placement;
        }
        Some(UnmapTransition {
            key: key.clone(),
            window: record.window.clone(),
            kind: record.kind,
            surface: record.surface.clone(),
        })
    }

    pub fn update_placement(&mut self, key: &WindowKey, placement: Placement) {
        if let Some(record) = self.records.get_mut(key) {
            record.placement = Some(placement);
        }
    }

    pub fn destroy(&mut self, key: &WindowKey) -> Option<UnmapTransition> {
        self.records.remove(key).map(|record| UnmapTransition {
            key: key.clone(),
            window: record.window,
            kind: record.kind,
            surface: record.surface,
        })
    }

    pub fn clear_x11(&mut self) -> Vec<UnmapTransition> {
        let keys: Vec<_> = self
            .records
            .keys()
            .filter(|key| matches!(key, WindowKey::X11(_)))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| self.destroy(&key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_map_and_finalize_are_idempotent() {
        let mut state = MappingState::default();
        assert_eq!(state.begin(), Some((1, true)));
        assert_eq!(state.begin(), None);
        assert_eq!(state.finalize(), Some((1, true)));
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn remap_gets_a_new_generation_but_is_not_a_first_map() {
        let mut state = MappingState::default();
        state.begin();
        state.finalize();
        assert!(state.unmap());
        assert!(!state.unmap());
        assert_eq!(state.begin(), Some((2, false)));
        assert_eq!(state.finalize(), Some((2, false)));
    }

    #[test]
    fn every_unmapped_generation_requires_one_configure() {
        let mut state = MappingState::default();
        assert!(state.needs_configure());
        state.mark_configured();
        assert!(!state.needs_configure());
        state.begin();
        state.finalize();
        assert!(!state.needs_configure());
        state.unmap();
        assert!(state.needs_configure());
        state.mark_configured();
        assert!(!state.needs_configure());
    }
}
