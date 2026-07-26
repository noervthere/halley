use std::borrow::Cow;

use smithay::backend::input::KeyState;
use smithay::desktop::{PopupKind, Window};
use smithay::input::Seat;
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Serial};
use smithay::wayland::seat::WaylandFocus;
use smithay::xwayland::X11Surface;

use crate::session::{Session, SessionDriver};

#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardFocusTarget {
    Wayland(WlSurface),
    X11(Box<X11Surface>),
}

impl KeyboardFocusTarget {
    pub fn for_window(window: &Window) -> Option<Self> {
        if let Some(surface) = window.x11_surface() {
            Some(Self::X11(Box::new(surface.clone())))
        } else {
            window
                .wl_surface()
                .map(|surface| Self::Wayland(surface.into_owned()))
        }
    }

    fn target<D: SessionDriver>(&self) -> &dyn KeyboardTarget<Session<D>> {
        match self {
            Self::Wayland(surface) => surface,
            Self::X11(surface) => surface.as_ref(),
        }
    }
}

impl From<WlSurface> for KeyboardFocusTarget {
    fn from(surface: WlSurface) -> Self {
        Self::Wayland(surface)
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(popup: PopupKind) -> Self {
        Self::Wayland(popup.wl_surface().clone())
    }
}

impl From<KeyboardFocusTarget> for WlSurface {
    fn from(target: KeyboardFocusTarget) -> Self {
        target
            .wl_surface()
            .expect("keyboard focus target has no Wayland surface")
            .into_owned()
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Wayland(surface) => surface.alive(),
            Self::X11(surface) => surface.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Wayland(surface) => Some(Cow::Borrowed(surface)),
            Self::X11(surface) => surface.wl_surface().map(Cow::Owned),
        }
    }
}

impl<D: SessionDriver> KeyboardTarget<Session<D>> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<Session<D>>,
        session: &mut Session<D>,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        self.target().enter(seat, session, keys, serial);
    }

    fn leave(&self, seat: &Seat<Session<D>>, session: &mut Session<D>, serial: Serial) {
        self.target().leave(seat, session, serial);
    }

    fn key(
        &self,
        seat: &Seat<Session<D>>,
        session: &mut Session<D>,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        self.target().key(seat, session, key, state, serial, time);
    }

    fn modifiers(
        &self,
        seat: &Seat<Session<D>>,
        session: &mut Session<D>,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        self.target().modifiers(seat, session, modifiers, serial);
    }
}
