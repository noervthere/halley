use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::blocking::Connection;
use zbus::fdo;
use zbus::interface;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const VERSION: u32 = 6;
const AVAILABLE_SOURCE_TYPES: u32 = halley_ipc::SOURCE_MONITOR | halley_ipc::SOURCE_WINDOW;
const CURSOR_HIDDEN: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;
const CURSOR_METADATA: u32 = 4;
const AVAILABLE_CURSOR_MODES: u32 = CURSOR_HIDDEN | CURSOR_EMBEDDED | CURSOR_METADATA;

type Vardict = HashMap<String, OwnedValue>;

pub struct ScreenCastInterface {
    connection: Connection,
    sessions: Arc<Mutex<crate::session::SessionStore>>,
    producer: Arc<crate::pipewire::Producer>,
}

impl ScreenCastInterface {
    pub fn new(connection: Connection, producer: Arc<crate::pipewire::Producer>) -> Self {
        Self {
            connection,
            sessions: Arc::new(Mutex::new(crate::session::SessionStore::default())),
            producer,
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastInterface {
    fn create_session(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: &str,
        _options: Vardict,
    ) -> fdo::Result<(u32, Vardict)> {
        export_request(&self.connection, &handle)?;
        let session_path = session_handle.to_string();
        let session = self
            .sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("session store lock poisoned".to_string()))?
            .create(session_path.clone());
        self.connection.object_server().at(
            session_handle,
            SessionInterface {
                handle: session_path,
                sessions: self.sessions.clone(),
                producer: self.producer.clone(),
            },
        )?;
        let mut results = Vardict::new();
        results.insert("session_id".to_string(), owned(Value::from(session.id))?);
        Ok((0, results))
    }

    fn select_sources(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: &str,
        options: Vardict,
    ) -> fdo::Result<(u32, Vardict)> {
        export_request(&self.connection, &handle)?;
        let source_types = extract_u32(&options, "types").unwrap_or(halley_ipc::SOURCE_MONITOR);
        let supported = source_types & AVAILABLE_SOURCE_TYPES;
        if supported == 0 || source_types & !AVAILABLE_SOURCE_TYPES != 0 {
            return Ok((2, Vardict::new()));
        }
        let cursor_mode = match extract_u32(&options, "cursor_mode").unwrap_or(CURSOR_HIDDEN) {
            CURSOR_HIDDEN => halley_ipc::CursorMode::Hidden,
            CURSOR_EMBEDDED => halley_ipc::CursorMode::Embedded,
            CURSOR_METADATA => halley_ipc::CursorMode::Metadata,
            _ => return Ok((2, Vardict::new())),
        };
        let session_path = session_handle.to_string();
        if self
            .sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("session store lock poisoned".to_string()))?
            .get(&session_path)
            .is_none()
        {
            return Ok((2, Vardict::new()));
        }
        match crate::compositor::choose_source(handle.to_string(), supported) {
            Ok(halley_ipc::SourceChooserResponse::Selected(source)) => {
                let mut sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| fdo::Error::Failed("session store lock poisoned".to_string()))?;
                let Some(session) = sessions.get_mut(&session_path) else {
                    return Ok((2, Vardict::new()));
                };
                session.selected = Some(source);
                session.cursor_mode = cursor_mode;
                Ok((0, Vardict::new()))
            }
            Ok(halley_ipc::SourceChooserResponse::Cancelled) => Ok((1, Vardict::new())),
            Ok(halley_ipc::SourceChooserResponse::Failed { message }) | Err(message) => {
                eventline::warn!("source chooser failed: {message}");
                Ok((2, Vardict::new()))
            }
        }
    }

    fn start(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: &str,
        _parent_window: &str,
        _options: Vardict,
    ) -> fdo::Result<(u32, Vardict)> {
        export_request(&self.connection, &handle)?;
        let session_path = session_handle.to_string();
        let (source, cursor_mode) = {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| fdo::Error::Failed("session store lock poisoned".to_string()))?;
            let Some(session) = sessions.get(&session_path) else {
                return Ok((2, Vardict::new()));
            };
            let Some(source) = session.selected.clone() else {
                return Ok((2, Vardict::new()));
            };
            (source, session.cursor_mode)
        };
        let (node, serial) =
            match self
                .producer
                .create_stream(&session_path, source.clone(), cursor_mode)
            {
                Ok(stream) => stream,
                Err(err) => {
                    eventline::warn!("could not start PipeWire stream: {err}");
                    return Ok((2, Vardict::new()));
                }
            };
        let mut properties = Vardict::new();
        properties.insert(
            "size".to_string(),
            owned(Value::from((source.width(), source.height())))?,
        );
        match &source {
            halley_ipc::CaptureSource::Monitor { name, x, y, .. } => {
                properties.insert("position".to_string(), owned(Value::from((*x, *y)))?);
                properties.insert(
                    "source_type".to_string(),
                    OwnedValue::from(halley_ipc::SOURCE_MONITOR),
                );
                properties.insert("mapping_id".to_string(), owned(Value::from(name.clone()))?);
            }
            halley_ipc::CaptureSource::Window { surface_id, .. } => {
                properties.insert(
                    "source_type".to_string(),
                    OwnedValue::from(halley_ipc::SOURCE_WINDOW),
                );
                properties.insert(
                    "mapping_id".to_string(),
                    owned(Value::from(format!("halley-window-{surface_id}")))?,
                );
            }
        }
        if let Some(serial) = serial {
            properties.insert("pipewire-serial".to_string(), OwnedValue::from(serial));
        }
        let streams = vec![(node, properties)];
        let mut results = Vardict::new();
        results.insert("streams".to_string(), owned(Value::from(streams))?);
        Ok((0, results))
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        AVAILABLE_SOURCE_TYPES
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        AVAILABLE_CURSOR_MODES
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        VERSION
    }
}

struct SessionInterface {
    handle: String,
    sessions: Arc<Mutex<crate::session::SessionStore>>,
    producer: Arc<crate::pipewire::Producer>,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionInterface {
    fn close(&self) -> fdo::Result<()> {
        self.producer.destroy_stream(&self.handle);
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&self.handle);
        }
        Ok(())
    }
}

struct RequestInterface {
    handle: String,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestInterface {
    fn close(&self) -> fdo::Result<()> {
        let _ = crate::compositor::cancel_source(self.handle.clone());
        Ok(())
    }
}

fn export_request(connection: &Connection, handle: &OwnedObjectPath) -> fdo::Result<()> {
    match connection.object_server().at(
        handle.clone(),
        RequestInterface {
            handle: handle.to_string(),
        },
    ) {
        Ok(_) => Ok(()),
        Err(err) => {
            eventline::warn!("could not export portal request {handle}: {err}");
            Ok(())
        }
    }
}

fn extract_u32(dict: &Vardict, key: &str) -> Option<u32> {
    match &**dict.get(key)? {
        Value::U32(value) => Some(*value),
        _ => None,
    }
}

fn owned(value: Value<'_>) -> fdo::Result<OwnedValue> {
    OwnedValue::try_from(value).map_err(|err| fdo::Error::Failed(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_capabilities_are_exact() {
        assert_eq!(
            AVAILABLE_SOURCE_TYPES,
            halley_ipc::SOURCE_MONITOR | halley_ipc::SOURCE_WINDOW
        );
        assert_eq!(
            AVAILABLE_CURSOR_MODES,
            CURSOR_HIDDEN | CURSOR_EMBEDDED | CURSOR_METADATA
        );
    }
}
