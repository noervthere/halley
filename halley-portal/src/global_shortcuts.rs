use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use zbus::blocking::{Connection, Proxy};
use zbus::fdo;
use zbus::interface;
use zbus::message::Header;
use zbus::names::{BusName, OwnedUniqueName, WellKnownName};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const VERSION: u32 = 2;
const FRONTEND_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const COMPOSITOR_BUS_NAME: &str = "org.halley.Compositor";
const COMPOSITOR_OBJECT_PATH: &str = "/org/halley/GlobalShortcuts";
const COMPOSITOR_INTERFACE: &str = "org.halley.GlobalShortcuts1";
const PORTAL_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_INTERFACE: &str = "org.freedesktop.impl.portal.GlobalShortcuts";

type Vardict = HashMap<String, OwnedValue>;
type Shortcut = (String, Vardict);

#[derive(Clone, Debug)]
struct BoundShortcut {
    id: String,
    description: String,
    trigger: String,
}

impl BoundShortcut {
    fn portal_value(&self) -> fdo::Result<Shortcut> {
        let mut properties = Vardict::new();
        properties.insert(
            "description".to_owned(),
            owned(Value::from(self.description.clone()))?,
        );
        properties.insert(
            "trigger_description".to_owned(),
            owned(Value::from(self.trigger.clone()))?,
        );
        Ok((self.id.clone(), properties))
    }
}

#[derive(Debug)]
struct GlobalShortcutSession {
    app_id: String,
    bound: bool,
    shortcuts: Vec<BoundShortcut>,
}

#[derive(Default)]
struct GlobalShortcutSessions {
    sessions: HashMap<String, GlobalShortcutSession>,
}

pub struct GlobalShortcutsInterface {
    connection: Connection,
    sessions: Arc<Mutex<GlobalShortcutSessions>>,
}

impl GlobalShortcutsInterface {
    pub fn new(connection: Connection) -> Self {
        start_event_forwarding(&connection);
        Self {
            connection,
            sessions: Arc::new(Mutex::new(GlobalShortcutSessions::default())),
        }
    }

    async fn authorized_frontend(&self, header: Header<'_>) -> fdo::Result<OwnedUniqueName> {
        authorized_frontend(&self.connection, header).await
    }
}

#[interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
impl GlobalShortcutsInterface {
    async fn create_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Vardict,
    ) -> fdo::Result<(u32, Vardict)> {
        self.authorized_frontend(header).await?;
        export_request(&self.connection, &handle)?;
        let session_path = session_handle.to_string();
        {
            let mut sessions = self.sessions.lock().map_err(|_| {
                fdo::Error::Failed("global shortcut session lock poisoned".to_owned())
            })?;
            if sessions.sessions.contains_key(&session_path) {
                return Ok((2, Vardict::new()));
            }
            sessions.sessions.insert(
                session_path.clone(),
                GlobalShortcutSession {
                    app_id,
                    bound: false,
                    shortcuts: Vec::new(),
                },
            );
        }
        self.connection.object_server().at(
            session_handle,
            GlobalShortcutSessionInterface {
                connection: self.connection.clone(),
                handle: session_path,
                sessions: self.sessions.clone(),
            },
        )?;
        Ok((0, Vardict::new()))
    }

    async fn bind_shortcuts(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        shortcuts: Vec<Shortcut>,
        _parent_window: String,
        _options: Vardict,
    ) -> fdo::Result<(u32, Vardict)> {
        self.authorized_frontend(header).await?;
        export_request(&self.connection, &handle)?;
        let session_path = session_handle.to_string();
        {
            let sessions = self.sessions.lock().map_err(|_| {
                fdo::Error::Failed("global shortcut session lock poisoned".to_owned())
            })?;
            let Some(session) = sessions.sessions.get(&session_path) else {
                return Ok((2, Vardict::new()));
            };
            if session.bound {
                return Ok((2, Vardict::new()));
            }
        }

        let requested = shortcuts
            .iter()
            .filter_map(|(id, properties)| {
                extract_string(properties, "preferred_trigger").map(|trigger| (id.clone(), trigger))
            })
            .collect::<Vec<_>>();
        let accepted = register_with_compositor(&self.connection, &session_path, &requested)
            .map_err(|err| {
                eventline::warn!("global shortcuts: compositor registration failed: {err}");
                fdo::Error::Failed(err.to_string())
            })?;
        let accepted_by_id = accepted.into_iter().collect::<HashMap<_, _>>();
        let bound = shortcuts
            .into_iter()
            .filter_map(|(id, properties)| {
                let trigger = accepted_by_id.get(&id)?.clone();
                Some(BoundShortcut {
                    id,
                    description: extract_string(&properties, "description").unwrap_or_default(),
                    trigger,
                })
            })
            .collect::<Vec<_>>();

        {
            let mut sessions = self.sessions.lock().map_err(|_| {
                fdo::Error::Failed("global shortcut session lock poisoned".to_owned())
            })?;
            let Some(session) = sessions.sessions.get_mut(&session_path) else {
                unregister_with_compositor(&self.connection, &session_path);
                return Ok((2, Vardict::new()));
            };
            session.bound = true;
            session.shortcuts = bound.clone();
            eventline::info!(
                "global shortcuts: bound {} shortcut(s) for {}",
                session.shortcuts.len(),
                session.app_id
            );
        }
        shortcuts_result(&bound).map(|results| (0, results))
    }

    async fn list_shortcuts(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
    ) -> fdo::Result<(u32, Vardict)> {
        self.authorized_frontend(header).await?;
        export_request(&self.connection, &handle)?;
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("global shortcut session lock poisoned".to_owned()))?;
        let Some(session) = sessions.sessions.get(session_handle.as_str()) else {
            return Ok((2, Vardict::new()));
        };
        shortcuts_result(&session.shortcuts).map(|results| (0, results))
    }

    async fn configure_shortcuts(
        &self,
        #[zbus(header)] header: Header<'_>,
        session_handle: OwnedObjectPath,
        _parent_window: String,
        _options: Vardict,
    ) -> fdo::Result<()> {
        self.authorized_frontend(header).await?;
        if !self
            .sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("global shortcut session lock poisoned".to_owned()))?
            .sessions
            .contains_key(session_handle.as_str())
        {
            return Err(fdo::Error::InvalidArgs("unknown session".to_owned()));
        }
        Ok(())
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        VERSION
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        shortcut_id: String,
        timestamp: u64,
        options: Vardict,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        shortcut_id: String,
        timestamp: u64,
        options: Vardict,
    ) -> zbus::Result<()>;
}

struct GlobalShortcutSessionInterface {
    connection: Connection,
    handle: String,
    sessions: Arc<Mutex<GlobalShortcutSessions>>,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl GlobalShortcutSessionInterface {
    async fn close(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        authorized_frontend(&self.connection, header).await?;
        self.sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("global shortcut session lock poisoned".to_owned()))?
            .sessions
            .remove(&self.handle);
        unregister_with_compositor(&self.connection, &self.handle);
        Ok(())
    }
}

struct RequestInterface;

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestInterface {
    fn close(&self) -> fdo::Result<()> {
        Ok(())
    }
}

async fn authorized_frontend(
    connection: &Connection,
    header: Header<'_>,
) -> fdo::Result<OwnedUniqueName> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus sender".to_owned()))?;
    let proxy = fdo::DBusProxy::new(connection.inner())
        .await
        .map_err(|err| fdo::Error::Failed(err.to_string()))?;
    let frontend_name = WellKnownName::try_from(FRONTEND_BUS_NAME)
        .map_err(|err| fdo::Error::Failed(err.to_string()))?;
    let owner = proxy
        .get_name_owner(BusName::WellKnown(frontend_name))
        .await
        .map_err(|_| {
            fdo::Error::AccessDenied(
                "only the active desktop portal frontend may use this interface".to_owned(),
            )
        })?;
    if sender != &owner.as_ref() {
        return Err(fdo::Error::AccessDenied(
            "only the active desktop portal frontend may use this interface".to_owned(),
        ));
    }
    Ok(owner)
}

fn register_with_compositor(
    connection: &Connection,
    session: &str,
    shortcuts: &[(String, String)],
) -> zbus::Result<Vec<(String, String)>> {
    let proxy = Proxy::new(
        connection,
        COMPOSITOR_BUS_NAME,
        COMPOSITOR_OBJECT_PATH,
        COMPOSITOR_INTERFACE,
    )?;
    proxy.call("RegisterShortcuts", &(session, shortcuts))
}

fn unregister_with_compositor(connection: &Connection, session: &str) {
    let result = Proxy::new(
        connection,
        COMPOSITOR_BUS_NAME,
        COMPOSITOR_OBJECT_PATH,
        COMPOSITOR_INTERFACE,
    )
    .and_then(|proxy| proxy.call::<_, _, ()>("UnregisterSession", &(session)));
    if let Err(err) = result {
        eventline::warn!("global shortcuts: could not unregister {session}: {err}");
    }
}

fn start_event_forwarding(connection: &Connection) {
    let connection = connection.inner().clone();
    let task = connection.clone().executor().spawn(
        async move {
            let proxy = match zbus::Proxy::new(
                &connection,
                COMPOSITOR_BUS_NAME,
                COMPOSITOR_OBJECT_PATH,
                COMPOSITOR_INTERFACE,
            )
            .await
            {
                Ok(proxy) => proxy,
                Err(err) => {
                    eventline::warn!("global shortcuts: cannot connect event forwarding: {err}");
                    return;
                }
            };
            let mut activated = match proxy.receive_signal("Activated").await {
                Ok(stream) => stream,
                Err(err) => {
                    eventline::warn!("global shortcuts: cannot receive activations: {err}");
                    return;
                }
            };
            let mut deactivated = match proxy.receive_signal("Deactivated").await {
                Ok(stream) => stream,
                Err(err) => {
                    eventline::warn!("global shortcuts: cannot receive deactivations: {err}");
                    return;
                }
            };
            loop {
                let (activated_event, message) = futures_util::select! {
                    message = activated.next() => (true, message),
                    message = deactivated.next() => (false, message),
                };
                let Some(message) = message else {
                    eventline::warn!("global shortcuts: compositor event stream ended");
                    return;
                };
                let Ok((session, shortcut_id, timestamp)) =
                    message.body().deserialize::<(String, String, u64)>()
                else {
                    eventline::warn!("global shortcuts: malformed compositor event");
                    continue;
                };
                let Ok(session) = OwnedObjectPath::try_from(session) else {
                    eventline::warn!("global shortcuts: invalid session in compositor event");
                    continue;
                };
                let member = if activated_event {
                    "Activated"
                } else {
                    "Deactivated"
                };
                if let Err(err) = connection
                    .emit_signal(
                        None::<&str>,
                        PORTAL_OBJECT_PATH,
                        PORTAL_INTERFACE,
                        member,
                        &(session, shortcut_id, timestamp, Vardict::new()),
                    )
                    .await
                {
                    eventline::warn!("global shortcuts: failed to forward {member}: {err}");
                }
            }
        },
        "halley global shortcut portal forwarding",
    );
    task.detach();
}

fn export_request(connection: &Connection, handle: &OwnedObjectPath) -> fdo::Result<()> {
    match connection
        .object_server()
        .at(handle.clone(), RequestInterface)
    {
        Ok(_) => Ok(()),
        Err(err) => {
            eventline::warn!("could not export portal request {handle}: {err}");
            Ok(())
        }
    }
}

fn shortcuts_result(shortcuts: &[BoundShortcut]) -> fdo::Result<Vardict> {
    let shortcuts = shortcuts
        .iter()
        .map(BoundShortcut::portal_value)
        .collect::<fdo::Result<Vec<_>>>()?;
    let mut results = Vardict::new();
    results.insert("shortcuts".to_owned(), owned(Value::from(shortcuts))?);
    Ok(results)
}

fn extract_string(dict: &Vardict, key: &str) -> Option<String> {
    match &**dict.get(key)? {
        Value::Str(value) => Some(value.as_str().to_owned()),
        _ => None,
    }
}

fn owned(value: Value<'_>) -> fdo::Result<OwnedValue> {
    OwnedValue::try_from(value).map_err(|err| fdo::Error::Failed(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested(description: &str, trigger: &str) -> Vardict {
        let mut properties = Vardict::new();
        properties.insert(
            "description".to_owned(),
            owned(Value::from(description.to_owned())).unwrap(),
        );
        properties.insert(
            "preferred_trigger".to_owned(),
            owned(Value::from(trigger.to_owned())).unwrap(),
        );
        properties
    }

    #[test]
    fn preferred_trigger_and_description_are_kept_separate() {
        let properties = requested("Push to talk", "CTRL+space");
        assert_eq!(
            extract_string(&properties, "preferred_trigger").as_deref(),
            Some("CTRL+space")
        );
        assert_eq!(
            extract_string(&properties, "description").as_deref(),
            Some("Push to talk")
        );
    }

    #[test]
    fn bound_result_uses_trigger_description() {
        let result = shortcuts_result(&[BoundShortcut {
            id: "push-to-talk".to_owned(),
            description: "Push to talk".to_owned(),
            trigger: "CTRL+space".to_owned(),
        }])
        .unwrap();
        assert!(result.contains_key("shortcuts"));
    }
}
