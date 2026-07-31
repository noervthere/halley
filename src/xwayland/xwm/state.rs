use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::xwayland) enum WindowState {
    #[default]
    Withdrawn,
    Normal,
    Iconic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagedState {
    generation: u64,
    state: WindowState,
}

#[derive(Default)]
pub(in crate::xwayland) struct ManagedStateRegistry {
    windows: HashMap<u32, ManagedState>,
    next_generation: u64,
    last_activation_time: Option<u32>,
}

impl ManagedStateRegistry {
    pub fn register(&mut self, xid: u32) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        self.windows.insert(
            xid,
            ManagedState {
                generation,
                state: WindowState::Withdrawn,
            },
        );
        generation
    }

    pub fn transition(&mut self, xid: u32, state: WindowState) -> u64 {
        let generation = self
            .windows
            .get(&xid)
            .map(|managed| managed.generation)
            .unwrap_or_else(|| self.register(xid));
        if let Some(managed) = self.windows.get_mut(&xid) {
            managed.state = state;
        }
        generation
    }

    pub fn state(&self, xid: u32) -> WindowState {
        self.windows
            .get(&xid)
            .map(|managed| managed.state)
            .unwrap_or(WindowState::Withdrawn)
    }

    pub fn destroy(&mut self, xid: u32) {
        self.windows.remove(&xid);
    }

    pub fn clear(&mut self) {
        self.windows.clear();
        self.last_activation_time = None;
    }

    /// X11 timestamps wrap at 32 bits. A signed wrapping difference is the
    /// conventional ordering as long as compared events are less than 2^31 ms
    /// apart.
    pub fn accept_activation(&mut self, timestamp: u32, same_client_handoff: bool) -> bool {
        if timestamp == 0 {
            return same_client_handoff || self.last_activation_time.is_none();
        }
        let accepted = self
            .last_activation_time
            .is_none_or(|previous| timestamp.wrapping_sub(previous) as i32 >= 0);
        if accepted {
            self.last_activation_time = Some(timestamp);
        }
        accepted
    }
}

#[cfg(test)]
mod tests {
    use super::{ManagedStateRegistry, WindowState};

    #[test]
    fn xid_reuse_gets_a_new_generation() {
        let mut states = ManagedStateRegistry::default();
        let first = states.register(42);
        states.destroy(42);
        let second = states.register(42);

        assert_ne!(first, second);
        assert_eq!(states.state(42), WindowState::Withdrawn);
    }

    #[test]
    fn lifecycle_transitions_are_explicit() {
        let mut states = ManagedStateRegistry::default();
        states.register(7);
        states.transition(7, WindowState::Normal);
        assert_eq!(states.state(7), WindowState::Normal);
        states.transition(7, WindowState::Iconic);
        assert_eq!(states.state(7), WindowState::Iconic);
        states.transition(7, WindowState::Withdrawn);
        assert_eq!(states.state(7), WindowState::Withdrawn);
    }

    #[test]
    fn activation_order_handles_timestamp_wrap() {
        let mut states = ManagedStateRegistry::default();
        assert!(states.accept_activation(u32::MAX - 4, false));
        assert!(states.accept_activation(3, false));
        assert!(!states.accept_activation(u32::MAX - 20, false));
    }

    #[test]
    fn zero_timestamp_requires_a_handoff_after_the_first_request() {
        let mut states = ManagedStateRegistry::default();
        assert!(states.accept_activation(0, false));
        assert!(states.accept_activation(10, false));
        assert!(!states.accept_activation(0, false));
        assert!(states.accept_activation(0, true));
    }
}
