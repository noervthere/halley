use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt;
use smithay::backend::input::{KeyState, Keycode};
use smithay::input::keyboard::{Keysym, ModifiersState, xkb};
use zbus::blocking::Connection;
use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::message::Header;
use zbus::names::{BusName, OwnedUniqueName, UniqueName};

const BUS_NAME: &str = "org.halley.Compositor";
const OBJECT_PATH: &str = "/org/halley/GlobalShortcuts";
const INTERFACE: &str = "org.halley.GlobalShortcuts1";
const PORTAL_BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.halley";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TriggerModifiers {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
    num: bool,
}

impl TriggerModifiers {
    fn matches(self, state: &ModifiersState) -> bool {
        self.ctrl == state.ctrl
            && self.alt == state.alt
            && self.shift == state.shift
            && self.logo == state.logo
            && (!self.num || state.num_lock)
            && !state.iso_level3_shift
            && !state.iso_level5_shift
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Trigger {
    keysym: Keysym,
    modifiers: TriggerModifiers,
}

impl Trigger {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('+').peekable();
        let mut modifiers = TriggerModifiers::default();
        while parts
            .peek()
            .is_some_and(|part| matches!(*part, "CTRL" | "ALT" | "SHIFT" | "LOGO" | "NUM"))
        {
            match parts.next()? {
                "CTRL" if !modifiers.ctrl => modifiers.ctrl = true,
                "ALT" if !modifiers.alt => modifiers.alt = true,
                "SHIFT" if !modifiers.shift => modifiers.shift = true,
                "LOGO" if !modifiers.logo => modifiers.logo = true,
                "NUM" if !modifiers.num => modifiers.num = true,
                _ => return None,
            }
        }
        let name = parts.next()?;
        if parts.next().is_some()
            || name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return None;
        }
        let keysym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
        (keysym.raw() != 0).then_some(Self { keysym, modifiers })
    }
}

#[derive(Clone, Debug)]
struct Registration {
    session: String,
    id: String,
    trigger: Trigger,
}

#[derive(Clone, Debug)]
struct ActiveShortcut {
    session: String,
    id: String,
}

#[derive(Debug, Default)]
struct ShortcutData {
    portal_owner: Option<OwnedUniqueName>,
    registrations: Vec<Registration>,
    active: HashMap<Keycode, ActiveShortcut>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShortcutEvent {
    session: String,
    id: String,
    activated: bool,
}

impl ShortcutData {
    fn register(
        &mut self,
        owner: OwnedUniqueName,
        session: String,
        requested: Vec<(String, String)>,
    ) -> Vec<(String, String)> {
        self.remove_session(&session);
        self.portal_owner = Some(owner);

        let mut accepted = Vec::new();
        for (id, trigger_text) in requested {
            let Some(trigger) = Trigger::parse(&trigger_text) else {
                continue;
            };
            if self
                .registrations
                .iter()
                .any(|existing| existing.trigger == trigger)
            {
                continue;
            }
            accepted.push((id.clone(), trigger_text.clone()));
            self.registrations.push(Registration {
                session: session.clone(),
                id,
                trigger,
            });
        }
        accepted
    }

    fn remove_session(&mut self, session: &str) {
        self.registrations
            .retain(|registration| registration.session != session);
        self.active.retain(|_, active| active.session != session);
    }

    fn portal_disconnected(&mut self, owner: &UniqueName<'_>) {
        if self.portal_owner.as_deref() == Some(owner) {
            self.portal_owner = None;
            self.registrations.clear();
            self.active.clear();
        }
    }

    fn route(
        &mut self,
        keycode: Keycode,
        state: KeyState,
        modifiers: &ModifiersState,
        keysym: Option<Keysym>,
    ) -> (bool, Option<ShortcutEvent>) {
        if state == KeyState::Released {
            let Some(active) = self.active.remove(&keycode) else {
                return (false, None);
            };
            return (
                true,
                Some(ShortcutEvent {
                    session: active.session,
                    id: active.id,
                    activated: false,
                }),
            );
        }

        if self.active.contains_key(&keycode) {
            return (true, None);
        }
        let Some(keysym) = keysym else {
            return (false, None);
        };
        let Some(registration) = self
            .registrations
            .iter()
            .find(|registration| {
                registration.trigger.keysym == keysym
                    && registration.trigger.modifiers.matches(modifiers)
            })
            .cloned()
        else {
            return (false, None);
        };
        let active = ActiveShortcut {
            session: registration.session.clone(),
            id: registration.id.clone(),
        };
        self.active.insert(keycode, active);
        (
            true,
            Some(ShortcutEvent {
                session: registration.session,
                id: registration.id,
                activated: true,
            }),
        )
    }
}

#[derive(Clone)]
struct ShortcutBroker {
    data: Arc<Mutex<ShortcutData>>,
    connection: Arc<OnceLock<Connection>>,
}

impl ShortcutBroker {
    fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(ShortcutData::default())),
            connection: Arc::new(OnceLock::new()),
        }
    }

    async fn authorized_sender(&self, header: Header<'_>) -> fdo::Result<OwnedUniqueName> {
        let connection = self.connection.get().ok_or_else(|| {
            fdo::Error::Failed("global shortcuts broker is not started".to_owned())
        })?;
        crate::dbus::require_name_owner(
            connection.inner(),
            header,
            PORTAL_BUS_NAME,
            "only the active Halley portal backend may register shortcuts",
        )
        .await
    }

    fn process_key(
        &self,
        time: Duration,
        keycode: Keycode,
        state: KeyState,
        modifiers: &ModifiersState,
        keysym: Option<Keysym>,
    ) -> bool {
        let (intercept, event, owner) = {
            let mut data = self
                .data
                .lock()
                .expect("global shortcuts state lock poisoned");
            let (intercept, event) = data.route(keycode, state, modifiers, keysym);
            (intercept, event, data.portal_owner.clone())
        };
        let (Some(event), Some(owner), Some(connection)) = (event, owner, self.connection.get())
        else {
            return intercept;
        };
        let member = if event.activated {
            "Activated"
        } else {
            "Deactivated"
        };
        if let Err(err) = connection.emit_signal(
            Some(BusName::Unique(owner.as_ref())),
            OBJECT_PATH,
            INTERFACE,
            member,
            &(event.session, event.id, time.as_millis() as u64),
        ) {
            eventline::warn!("global shortcuts: failed to emit event: {err}");
        }
        intercept
    }
}

#[interface(name = "org.halley.GlobalShortcuts1")]
impl ShortcutBroker {
    async fn register_shortcuts(
        &self,
        #[zbus(header)] header: Header<'_>,
        session: String,
        shortcuts: Vec<(String, String)>,
    ) -> fdo::Result<Vec<(String, String)>> {
        let owner = self.authorized_sender(header).await?;
        Ok(self
            .data
            .lock()
            .map_err(|_| fdo::Error::Failed("global shortcuts state lock poisoned".to_owned()))?
            .register(owner, session, shortcuts))
    }

    async fn unregister_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        session: &str,
    ) -> fdo::Result<()> {
        self.authorized_sender(header).await?;
        self.data
            .lock()
            .map_err(|_| fdo::Error::Failed("global shortcuts state lock poisoned".to_owned()))?
            .remove_session(session);
        Ok(())
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        session: &str,
        shortcut_id: &str,
        timestamp: u64,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        session: &str,
        shortcut_id: &str,
        timestamp: u64,
    ) -> zbus::Result<()>;
}

pub struct GlobalShortcutService {
    broker: ShortcutBroker,
    _connection: Connection,
}

impl GlobalShortcutService {
    pub fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let broker = ShortcutBroker::new();
        let connection = Connection::session()?;
        broker
            .connection
            .set(connection.clone())
            .map_err(|_| "global shortcuts connection was already initialized")?;
        connection.object_server().at(OBJECT_PATH, broker.clone())?;
        connection.request_name_with_flags(
            BUS_NAME,
            RequestNameFlags::AllowReplacement
                | RequestNameFlags::ReplaceExisting
                | RequestNameFlags::DoNotQueue,
        )?;
        start_disconnect_monitor(&connection, broker.data.clone());
        eventline::info!("global shortcuts: compositor broker ready");
        Ok(Self {
            broker,
            _connection: connection,
        })
    }

    pub fn process_key(
        &self,
        time: Duration,
        keycode: Keycode,
        state: KeyState,
        modifiers: &ModifiersState,
        keysym: Option<Keysym>,
    ) -> bool {
        self.broker
            .process_key(time, keycode, state, modifiers, keysym)
    }
}

fn start_disconnect_monitor(connection: &Connection, data: Arc<Mutex<ShortcutData>>) {
    let connection = connection.inner().clone();
    let task = connection.clone().executor().spawn(
        async move {
            let proxy = match fdo::DBusProxy::new(&connection).await {
                Ok(proxy) => proxy,
                Err(err) => {
                    eventline::warn!("global shortcuts: cannot monitor portal owner: {err}");
                    return;
                }
            };
            let mut stream = match proxy
                .receive_name_owner_changed_with_args(&[(0, PORTAL_BUS_NAME)])
                .await
            {
                Ok(stream) => stream,
                Err(err) => {
                    eventline::warn!("global shortcuts: cannot watch portal disconnects: {err}");
                    return;
                }
            };
            while let Some(signal) = stream.next().await {
                let Ok(args) = signal.args() else {
                    continue;
                };
                let Some(owner) = &**args.old_owner() else {
                    continue;
                };
                data.lock()
                    .expect("global shortcuts state lock poisoned")
                    .portal_disconnected(owner);
            }
        },
        "halley global shortcut portal cleanup",
    );
    task.detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_space_data() -> ShortcutData {
        let owner = OwnedUniqueName::try_from(":1.42").unwrap();
        let mut data = ShortcutData::default();
        assert_eq!(
            data.register(
                owner,
                "/org/freedesktop/portal/session/test".to_owned(),
                vec![("push-to-talk".to_owned(), "CTRL+space".to_owned())],
            ),
            [("push-to-talk".to_owned(), "CTRL+space".to_owned())]
        );
        data
    }

    fn modifiers(ctrl: bool) -> ModifiersState {
        ModifiersState {
            ctrl,
            ..ModifiersState::default()
        }
    }

    #[test]
    fn parser_uses_xdg_modifier_and_keysym_names() {
        let parsed = Trigger::parse("CTRL+ALT+space").unwrap();
        assert_eq!(parsed.keysym, Keysym::space);
        assert!(parsed.modifiers.ctrl);
        assert!(parsed.modifiers.alt);
        assert!(Trigger::parse("Ctrl+space").is_none());
        assert!(Trigger::parse("CTRL+not-a-keysym").is_none());
    }

    #[test]
    fn ctrl_space_normalizes_caps_and_num_lock_only() {
        let trigger = Trigger::parse("CTRL+space").unwrap();
        let mut state = modifiers(true);
        state.caps_lock = true;
        state.num_lock = true;
        assert!(trigger.modifiers.matches(&state));
        state.shift = true;
        assert!(!trigger.modifiers.matches(&state));
        state.shift = false;
        state.alt = true;
        assert!(!trigger.modifiers.matches(&state));
        state.alt = false;
        state.logo = true;
        assert!(!trigger.modifiers.matches(&state));
    }

    #[test]
    fn held_key_repeat_activates_once_and_remains_intercepted() {
        let mut data = ctrl_space_data();
        let keycode = Keycode::new(65);
        let first = data.route(
            keycode,
            KeyState::Pressed,
            &modifiers(true),
            Some(Keysym::space),
        );
        let repeat = data.route(
            keycode,
            KeyState::Pressed,
            &modifiers(true),
            Some(Keysym::space),
        );
        assert!(first.0);
        assert!(first.1.unwrap().activated);
        assert_eq!(repeat, (true, None));
    }

    #[test]
    fn release_is_paired_after_modifier_release() {
        let mut data = ctrl_space_data();
        let keycode = Keycode::new(65);
        data.route(
            keycode,
            KeyState::Pressed,
            &modifiers(true),
            Some(Keysym::space),
        );
        let release = data.route(
            keycode,
            KeyState::Released,
            &ModifiersState::default(),
            Some(Keysym::space),
        );
        assert!(release.0);
        assert!(!release.1.unwrap().activated);
    }

    #[test]
    fn unmatched_or_extra_modifier_chords_pass_through() {
        let mut data = ctrl_space_data();
        let keycode = Keycode::new(65);
        let no_ctrl = data.route(
            keycode,
            KeyState::Pressed,
            &ModifiersState::default(),
            Some(Keysym::space),
        );
        let mut with_shift = modifiers(true);
        with_shift.shift = true;
        let shifted = data.route(keycode, KeyState::Pressed, &with_shift, Some(Keysym::space));
        assert_eq!(no_ctrl, (false, None));
        assert_eq!(shifted, (false, None));
    }

    #[test]
    fn registration_deduplicates_triggers_and_disconnect_cleans_up() {
        let owner = OwnedUniqueName::try_from(":1.42").unwrap();
        let mut data = ShortcutData::default();
        assert_eq!(
            data.register(
                owner.clone(),
                "one".to_owned(),
                vec![("first".to_owned(), "CTRL+space".to_owned())],
            )
            .len(),
            1
        );
        assert!(
            data.register(
                owner.clone(),
                "two".to_owned(),
                vec![("second".to_owned(), "CTRL+space".to_owned())],
            )
            .is_empty()
        );
        data.portal_disconnected(&owner.as_ref());
        assert!(data.registrations.is_empty());
        assert!(data.active.is_empty());
    }
}
