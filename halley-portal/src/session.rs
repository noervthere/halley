use std::collections::HashMap;

#[derive(Clone)]
pub struct PortalSession {
    pub id: String,
    pub selected: Option<halley_ipc::CaptureSource>,
    pub cursor_mode: halley_ipc::CursorMode,
}

#[derive(Default)]
pub struct SessionStore {
    sessions: HashMap<String, PortalSession>,
    next_id: u64,
}

impl SessionStore {
    pub fn create(&mut self, handle: String) -> PortalSession {
        self.next_id = self.next_id.wrapping_add(1);
        let session = PortalSession {
            id: format!("halley{}", self.next_id),
            selected: None,
            cursor_mode: halley_ipc::CursorMode::Hidden,
        };
        self.sessions.insert(handle, session.clone());
        session
    }

    pub fn get(&self, handle: &str) -> Option<&PortalSession> {
        self.sessions.get(handle)
    }

    pub fn get_mut(&mut self, handle: &str) -> Option<&mut PortalSession> {
        self.sessions.get_mut(handle)
    }

    pub fn remove(&mut self, handle: &str) {
        self.sessions.remove(handle);
    }
}
