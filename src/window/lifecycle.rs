use std::collections::HashMap;

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::xwayland::X11Surface;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WindowId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowKind {
    Xdg,
    X11,
    X11OverrideRedirect,
}

impl WindowKind {
    pub(crate) fn is_managed(self) -> bool {
        !matches!(self, Self::X11OverrideRedirect)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) location: Point<i32, Logical>,
    pub(crate) output: Option<String>,
}

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
    pub(crate) kind: WindowKind,
    pub(crate) placement: Option<Placement>,
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
    kind: WindowKind,
    mapping: MappingState,
    placement: Option<Placement>,
    presented: bool,
    input_ready: bool,
    geometry: GeometryState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GeometryState {
    target: Option<Rectangle<i32, Logical>>,
    pending: bool,
    focus_when_settled: bool,
}

impl GeometryState {
    fn begin(&mut self, target: Rectangle<i32, Logical>, focus_when_settled: bool) {
        self.target = Some(target);
        self.pending = true;
        self.focus_when_settled = focus_when_settled;
    }

    fn settle(&mut self, observed: Rectangle<i32, Logical>) -> Option<bool> {
        if !self.pending || self.target != Some(observed) {
            return None;
        }
        self.pending = false;
        Some(std::mem::take(&mut self.focus_when_settled))
    }
}

#[derive(Default)]
pub(crate) struct WindowRegistry {
    next_id: u64,
    records: HashMap<WindowId, WindowRecord>,
    xdg: HashMap<WlSurface, WindowId>,
    x11: HashMap<u32, WindowId>,
    associated: HashMap<WlSurface, WindowId>,
}

impl WindowRegistry {
    fn next_id(&mut self) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub(crate) fn register_xdg(&mut self, surface: WlSurface, window: Window) -> WindowId {
        let id = self.next_id();
        self.records.insert(
            id,
            WindowRecord {
                window,
                kind: WindowKind::Xdg,
                mapping: MappingState::default(),
                placement: None,
                presented: false,
                input_ready: false,
                geometry: GeometryState::default(),
            },
        );
        self.xdg.insert(surface, id);
        id
    }

    pub(crate) fn register_x11(&mut self, surface: X11Surface, kind: WindowKind) -> WindowId {
        let xid = surface.window_id();
        if let Some(id) = self.x11.get(&xid).copied() {
            return id;
        }
        let id = self.next_id();
        self.records.insert(
            id,
            WindowRecord {
                window: Window::new_x11_window(surface),
                kind,
                mapping: MappingState::default(),
                placement: None,
                presented: false,
                input_ready: false,
                geometry: GeometryState::default(),
            },
        );
        self.x11.insert(xid, id);
        id
    }

    pub(crate) fn id_for_x11(&self, surface: &X11Surface) -> Option<WindowId> {
        self.x11.get(&surface.window_id()).copied()
    }

    pub(crate) fn associate(&mut self, id: WindowId, surface: WlSurface) {
        self.associated.retain(|_, candidate| candidate != &id);
        self.associated.insert(surface, id);
    }

    pub(crate) fn id_for_xdg(&self, surface: &WlSurface) -> Option<WindowId> {
        self.xdg.get(surface).copied()
    }

    pub(crate) fn window(&self, id: WindowId) -> Option<&Window> {
        self.records.get(&id).map(|record| &record.window)
    }

    pub(crate) fn window_for_surface(&self, surface: &WlSurface) -> Option<&Window> {
        self.id_for_surface(surface).and_then(|id| self.window(id))
    }

    pub(crate) fn id_for_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.xdg
            .get(surface)
            .or_else(|| self.associated.get(surface))
            .copied()
    }

    pub(crate) fn placement(&self, id: WindowId) -> Option<&Placement> {
        self.records
            .get(&id)
            .and_then(|record| record.placement.as_ref())
    }

    pub(crate) fn set_placement(&mut self, id: WindowId, placement: Placement) {
        if let Some(record) = self.records.get_mut(&id) {
            record.placement = Some(placement);
        }
    }

    pub(crate) fn set_input_ready(&mut self, id: WindowId, ready: bool) {
        if let Some(record) = self.records.get_mut(&id) {
            record.input_ready = ready;
        }
    }

    pub(crate) fn present(&mut self, id: WindowId) -> bool {
        let Some(record) = self.records.get_mut(&id) else {
            return false;
        };
        if record.presented {
            return false;
        }
        record.presented = true;
        true
    }

    pub(crate) fn is_presented(&self, id: WindowId) -> bool {
        self.records.get(&id).is_some_and(|record| record.presented)
    }

    pub(crate) fn input_ready(&self, id: WindowId) -> bool {
        self.records
            .get(&id)
            .is_some_and(|record| record.input_ready)
    }

    pub(crate) fn input_ready_for_surface(&self, surface: &WlSurface) -> Option<bool> {
        self.id_for_surface(surface).map(|id| self.input_ready(id))
    }

    pub(crate) fn begin_geometry_settlement(
        &mut self,
        id: WindowId,
        target: Rectangle<i32, Logical>,
        gate_input: bool,
    ) {
        let Some(record) = self.records.get_mut(&id) else {
            return;
        };
        record.geometry.begin(target, gate_input);
        if gate_input {
            record.input_ready = false;
        }
    }

    pub(crate) fn geometry_target(&self, id: WindowId) -> Option<Rectangle<i32, Logical>> {
        self.records.get(&id)?.geometry.target
    }

    pub(crate) fn settle_geometry(
        &mut self,
        id: WindowId,
        observed: Rectangle<i32, Logical>,
    ) -> Option<bool> {
        let record = self.records.get_mut(&id)?;
        let focus = record.geometry.settle(observed)?;
        record.input_ready = true;
        Some(focus)
    }

    pub(crate) fn clear_geometry_target(&mut self, id: WindowId) {
        if let Some(record) = self.records.get_mut(&id) {
            record.geometry = GeometryState::default();
            record.input_ready = record.mapping.admitted;
        }
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

    pub(crate) fn is_admitted(&self, id: WindowId) -> bool {
        self.records
            .get(&id)
            .is_some_and(|record| record.mapping.admitted)
    }

    pub(crate) fn admit(&mut self, id: WindowId) -> Option<Admission> {
        let record = self.records.get_mut(&id)?;
        let admission = record.mapping.admit()?;
        Some(Admission {
            id,
            window: record.window.clone(),
            kind: record.kind,
            placement: record.placement.clone(),
            generation: admission.generation,
            first: admission.first,
        })
    }

    pub(crate) fn withdraw(&mut self, id: WindowId) -> Option<Window> {
        let record = self.records.get_mut(&id)?;
        if !record.mapping.withdraw() {
            return None;
        }
        record.input_ready = false;
        record.presented = false;
        record.geometry = GeometryState::default();
        Some(record.window.clone())
    }

    pub(crate) fn destroy_xdg(&mut self, surface: &WlSurface) -> Option<Window> {
        let id = self.xdg.remove(surface)?;
        self.associated.retain(|_, candidate| candidate != &id);
        self.records.remove(&id).map(|record| record.window)
    }

    pub(crate) fn destroy_x11(&mut self, surface: &X11Surface) -> Option<Window> {
        let id = self.x11.remove(&surface.window_id())?;
        self.associated.retain(|_, candidate| candidate != &id);
        self.records.remove(&id).map(|record| record.window)
    }

    pub(crate) fn clear_x11(&mut self) -> Vec<Window> {
        let ids: Vec<_> = self.x11.drain().map(|(_, id)| id).collect();
        self.associated
            .retain(|_, candidate| !ids.contains(candidate));
        ids.into_iter()
            .filter_map(|id| self.records.remove(&id).map(|record| record.window))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use smithay::utils::Rectangle;

    use super::{GeometryState, MappingState};

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

    #[test]
    fn geometry_settlement_ignores_intermediate_client_sizes() {
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let mut state = GeometryState::default();
        state.begin(target, true);

        assert_eq!(
            state.settle(Rectangle::new((0, 0).into(), (640, 480).into())),
            None
        );
        assert_eq!(state.settle(target), Some(true));
        assert_eq!(state.settle(target), None);
    }

    #[test]
    fn settled_window_geometry_does_not_request_a_second_focus() {
        let target = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let mut state = GeometryState::default();
        state.begin(target, false);

        assert_eq!(state.settle(target), Some(false));
    }
}
