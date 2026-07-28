use std::collections::HashMap;

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WindowId(u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MappingState {
    map_requested: bool,
    surface_ready: bool,
    admitted: bool,
    ever_admitted: bool,
    generation: u64,
}

#[derive(Clone)]
pub(crate) struct Admission {
    pub(crate) id: WindowId,
    pub(crate) window: Window,
    pub(crate) generation: u64,
    pub(crate) first: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MappingAdmission {
    generation: u64,
    first: bool,
}

impl MappingState {
    pub(crate) fn request_map(&mut self) {
        if self.map_requested {
            return;
        }
        self.map_requested = true;
        self.generation = self.generation.saturating_add(1);
    }

    pub(crate) fn set_surface_ready(&mut self, ready: bool) {
        self.surface_ready = ready;
    }

    fn admit(&mut self) -> Option<MappingAdmission> {
        if !self.map_requested || !self.surface_ready || self.admitted {
            return None;
        }
        self.admitted = true;
        let admission = MappingAdmission {
            generation: self.generation,
            first: !self.ever_admitted,
        };
        self.ever_admitted = true;
        Some(admission)
    }

    pub(crate) fn withdraw(&mut self) -> bool {
        let was_admitted = self.admitted;
        self.map_requested = false;
        self.surface_ready = false;
        self.admitted = false;
        was_admitted
    }
}

struct WindowRecord {
    window: Window,
    mapping: MappingState,
}

#[derive(Default)]
pub(crate) struct WindowRegistry {
    next_id: u64,
    records: HashMap<WindowId, WindowRecord>,
    xdg: HashMap<WlSurface, WindowId>,
}

impl WindowRegistry {
    pub(crate) fn register_xdg(&mut self, surface: WlSurface, window: Window) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.records.insert(
            id,
            WindowRecord {
                window,
                mapping: MappingState::default(),
            },
        );
        self.xdg.insert(surface, id);
        id
    }

    pub(crate) fn id_for_xdg(&self, surface: &WlSurface) -> Option<WindowId> {
        self.xdg.get(surface).copied()
    }

    pub(crate) fn window(&self, id: WindowId) -> Option<&Window> {
        self.records.get(&id).map(|record| &record.window)
    }

    pub(crate) fn window_for_surface(&self, surface: &WlSurface) -> Option<&Window> {
        self.id_for_xdg(surface).and_then(|id| self.window(id))
    }

    pub(crate) fn request_map(&mut self, id: WindowId) {
        if let Some(record) = self.records.get_mut(&id) {
            record.mapping.request_map();
        }
    }

    pub(crate) fn set_surface_ready(&mut self, id: WindowId, ready: bool) {
        if let Some(record) = self.records.get_mut(&id) {
            record.mapping.set_surface_ready(ready);
        }
    }

    pub(crate) fn admit(&mut self, id: WindowId) -> Option<Admission> {
        let record = self.records.get_mut(&id)?;
        let admission = record.mapping.admit()?;
        Some(Admission {
            id,
            window: record.window.clone(),
            generation: admission.generation,
            first: admission.first,
        })
    }

    pub(crate) fn withdraw(&mut self, id: WindowId) -> Option<Window> {
        let record = self.records.get_mut(&id)?;
        record.mapping.withdraw().then(|| record.window.clone())
    }

    pub(crate) fn destroy_xdg(&mut self, surface: &WlSurface) -> Option<Window> {
        let id = self.xdg.remove(surface)?;
        self.records.remove(&id).map(|record| record.window)
    }
}

#[cfg(test)]
mod tests {
    use super::MappingState;

    #[test]
    fn admission_requires_both_map_intent_and_a_surface() {
        let mut state = MappingState::default();
        state.request_map();
        assert_eq!(state.admit(), None);

        state.set_surface_ready(true);
        let admission = state.admit().expect("ready mapped window is admitted");
        assert_eq!(admission.generation, 1);
        assert!(admission.first);
    }

    #[test]
    fn repeated_events_do_not_repeat_scene_admission() {
        let mut state = MappingState::default();
        state.request_map();
        state.request_map();
        state.set_surface_ready(true);
        state.set_surface_ready(true);

        assert!(state.admit().is_some());
        assert_eq!(state.admit(), None);
    }

    #[test]
    fn remap_is_a_new_generation_without_becoming_a_new_window() {
        let mut state = MappingState::default();
        state.request_map();
        state.set_surface_ready(true);
        assert!(state.admit().expect("first admission").first);
        assert!(state.withdraw());
        assert!(!state.withdraw());

        state.request_map();
        state.set_surface_ready(true);
        let admission = state.admit().expect("second admission");
        assert_eq!(admission.generation, 2);
        assert!(!admission.first);
    }

    #[test]
    fn surface_association_can_arrive_before_map_request() {
        let mut state = MappingState::default();
        state.set_surface_ready(true);
        assert_eq!(state.admit(), None);

        state.request_map();
        assert!(state.admit().is_some());
    }
}
