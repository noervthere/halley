use std::collections::HashMap;

use crate::field::{Field, NodeId, NodeState};

/// Current interaction target (separate from history).
/// Focus only applies to nodes that are present and experience-visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    focused: Option<NodeId>,
}

impl Focus {
    pub fn new() -> Self {
        Self { focused: None }
    }

    pub fn current(&self) -> Option<NodeId> {
        self.focused
    }

    pub fn is_focused(&self, id: NodeId) -> bool {
        self.focused == Some(id)
    }

    /// Set focus to `id` if it exists and is experience-visible.
    pub fn set(&mut self, field: &Field, id: NodeId) -> bool {
        if !field.is_visible(id) {
            return false;
        }
        self.focused = Some(id);
        true
    }

    pub fn clear(&mut self) {
        self.focused = None;
    }

    /// If the focused node is removed, clear focus.
    pub fn on_removed(&mut self, removed: NodeId) {
        if self.focused == Some(removed) {
            self.focused = None;
        }
    }

    /// If the focused node becomes hidden (collapse/hide/detach), clear focus.
    pub fn on_hidden(&mut self, field: &Field, id: NodeId) {
        if self.focused == Some(id) && !field.is_visible(id) {
            self.focused = None;
        }
    }
}

impl Default for Focus {
    fn default() -> Self {
        Self::new()
    }
}

/// True if `id` is eligible to receive focus via cycling (alt-tab-style):
/// visible and in a representation state that's actually a real window
/// (`Active` or the decayed `Node` dot) rather than a collapsed-cluster
/// core handle. Ported from `cycle::is_focus_cycle_candidate` - the old
/// version also checked `kind == NodeKind::Surface`, which is now
/// redundant: with `NodeKind` gone, `matches!(state, Active | Node)`
/// already excludes `Core` on its own.
pub fn is_focus_cycle_candidate(field: &Field, id: NodeId) -> bool {
    field.node(id).is_some_and(|node| {
        field.is_visible(id) && matches!(node.state, NodeState::Active | NodeState::Node)
    })
}

/// Build an ordered list of focus-cycle candidates: most-recently-focused
/// first (ties broken by id, descending, for determinism), with `origin`
/// (if given and still a candidate) moved to the front so cycling always
/// starts from where the user currently is.
///
/// Ported from `cycle::build_candidates`. `last_focus_ms` is passed in
/// explicitly rather than read off `Field` because it isn't a `Field`/
/// `Node` concept - it's `wl`'s own `FocusState::last_surface_focus_ms`
/// timestamp map (distinct from `Node::last_touch_ms`, which tracks decay
/// aging, not focus history - don't conflate the two).
pub fn focus_cycle_candidates(
    field: &Field,
    last_focus_ms: &HashMap<NodeId, u64>,
    origin: Option<NodeId>,
) -> Vec<NodeId> {
    let mut candidates: Vec<NodeId> = field
        .node_ids_all()
        .into_iter()
        .filter(|&id| is_focus_cycle_candidate(field, id))
        .collect();

    candidates.sort_by(|a, b| {
        let a_at = last_focus_ms.get(a).copied().unwrap_or(0);
        let b_at = last_focus_ms.get(b).copied().unwrap_or(0);
        b_at.cmp(&a_at).then_with(|| b.as_u64().cmp(&a.as_u64()))
    });

    if let Some(origin) = origin
        && let Some(index) = candidates.iter().position(|&id| id == origin)
    {
        let o = candidates.remove(index);
        candidates.insert(0, o);
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Vec2;

    #[test]
    fn starts_empty() {
        let f = Focus::new();
        assert_eq!(f.current(), None);
    }

    #[test]
    fn can_focus_existing_node() {
        let mut field = Field::new();
        let id = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        let mut focus = Focus::new();
        assert!(focus.set(&field, id));
        assert_eq!(focus.current(), Some(id));
        assert!(focus.is_focused(id));
    }

    #[test]
    fn cannot_focus_missing_node() {
        let field = Field::new();
        let mut focus = Focus::new();
        assert!(!focus.set(&field, NodeId::new(999)));
        assert_eq!(focus.current(), None);
    }

    #[test]
    fn cannot_focus_hidden_node() {
        let mut field = Field::new();
        let id = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        assert!(field.set_hidden(id, true));

        let mut focus = Focus::new();
        assert!(!focus.set(&field, id));
        assert_eq!(focus.current(), None);
    }

    #[test]
    fn clears_when_focused_node_removed() {
        let mut field = Field::new();
        let id = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        let mut focus = Focus::new();
        assert!(focus.set(&field, id));

        field.remove(id);
        focus.on_removed(id);

        assert_eq!(focus.current(), None);
    }

    #[test]
    fn clears_when_focused_node_becomes_hidden() {
        let mut field = Field::new();
        let id = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        let mut focus = Focus::new();
        assert!(focus.set(&field, id));

        assert!(field.set_hidden(id, true));
        focus.on_hidden(&field, id);

        assert_eq!(focus.current(), None);
    }

    #[test]
    fn cycle_candidate_excludes_hidden_and_core_state() {
        let mut field = Field::new();
        let active = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });
        let hidden = field.spawn_surface("B", Vec2 { x: 10.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });
        let core = field.spawn_surface("C", Vec2 { x: 20.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        field.set_hidden(hidden, true);
        field.set_state(core, NodeState::Core);

        assert!(is_focus_cycle_candidate(&field, active));
        assert!(!is_focus_cycle_candidate(&field, hidden));
        assert!(!is_focus_cycle_candidate(&field, core));
    }

    #[test]
    fn cycle_candidates_order_by_recency_then_move_origin_to_front() {
        let mut field = Field::new();
        let a = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });
        let b = field.spawn_surface("B", Vec2 { x: 10.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });
        let c = field.spawn_surface("C", Vec2 { x: 20.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        let mut last_focus_ms = HashMap::new();
        last_focus_ms.insert(a, 100);
        last_focus_ms.insert(b, 300);
        last_focus_ms.insert(c, 200);

        // No origin: strictly most-recent-first.
        let candidates = focus_cycle_candidates(&field, &last_focus_ms, None);
        assert_eq!(candidates, vec![b, c, a]);

        // With an origin, it moves to the front even though it isn't
        // the most recent.
        let candidates = focus_cycle_candidates(&field, &last_focus_ms, Some(a));
        assert_eq!(candidates, vec![a, b, c]);
    }

    #[test]
    fn cycle_candidates_defaults_missing_timestamps_to_zero() {
        let mut field = Field::new();
        let known = field.spawn_surface("A", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });
        let unknown = field.spawn_surface("B", Vec2 { x: 10.0, y: 0.0 }, Vec2 { x: 10.0, y: 10.0 });

        let mut last_focus_ms = HashMap::new();
        last_focus_ms.insert(known, 50);

        let candidates = focus_cycle_candidates(&field, &last_focus_ms, None);
        // known has a real timestamp and sorts before unknown (defaults to 0).
        assert_eq!(candidates, vec![known, unknown]);
    }
}
