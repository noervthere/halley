use std::cell::Cell;
use std::error::Error;

use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, GetGeometryReply, PropMode, Window};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        UTF8_STRING,
        WM_S0,
        WM_STATE,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WM_NAME,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_GEOMETRY,
        _NET_DESKTOP_VIEWPORT,
        _NET_WORKAREA,
        _NET_WM_MOVERESIZE,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_DEMANDS_ATTENTION,
        _NET_WM_STATE_FOCUSED,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_ALLOWED_ACTIONS,
        _NET_WM_ACTION_MOVE,
        _NET_WM_ACTION_RESIZE,
        _NET_WM_ACTION_MINIMIZE,
        _NET_WM_ACTION_MAXIMIZE_HORZ,
        _NET_WM_ACTION_MAXIMIZE_VERT,
        _NET_WM_ACTION_FULLSCREEN,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcccmState {
    Normal = 1,
    Iconic = 3,
}

#[derive(Clone, Copy, Debug)]
pub struct AllowedActions {
    pub move_: bool,
    pub resize: bool,
    pub minimize: bool,
    pub maximize: bool,
    pub fullscreen: bool,
}

impl Default for AllowedActions {
    fn default() -> Self {
        Self {
            move_: true,
            resize: true,
            minimize: true,
            maximize: true,
            fullscreen: true,
        }
    }
}

/// A property-only connection to the X server managed by Smithay's XWM.
///
/// Smithay remains the sole owner of `WM_S0`, substructure redirection, and the
/// X11 event stream. This connection only fills protocol state that Smithay's
/// public API does not currently expose, keeping that compatibility policy in
/// Halley without modifying the pinned dependency.
pub struct X11Control {
    connection: RustConnection,
    atoms: Atoms,
    root: Window,
    active_window: Cell<Option<Option<Window>>>,
}

impl X11Control {
    pub fn connect(display_number: u32) -> Result<Self, Box<dyn Error>> {
        let display = format!(":{display_number}");
        let (connection, screen_number) = RustConnection::connect(Some(&display))?;
        let root = connection.setup().roots[screen_number].root;
        let atoms = Atoms::new(&connection)?.reply()?;

        let owner = connection.get_selection_owner(atoms.WM_S0)?.reply()?.owner;
        if owner == NONE {
            return Err("Smithay XWM does not own WM_S0".into());
        }
        let support_window = supporting_wm_window(&connection, root, &atoms)?;
        let root_geometry = connection.get_geometry(root)?.reply()?;
        publish_ewmh(&connection, root, support_window, root_geometry, &atoms)?;
        connection.flush()?;

        Ok(Self {
            connection,
            atoms,
            root,
            active_window: Cell::new(None),
        })
    }

    pub fn set_wm_state(&self, window: Window, state: IcccmState) -> Result<(), Box<dyn Error>> {
        self.connection
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms.WM_STATE,
                self.atoms.WM_STATE,
                &[state as u32, NONE],
            )?
            .check()?;
        Ok(())
    }

    pub fn withdraw(&self, window: Window) -> Result<(), Box<dyn Error>> {
        self.connection
            .delete_property(window, self.atoms.WM_STATE)?
            .check()?;
        Ok(())
    }

    pub fn set_active_window(&self, window: Option<Window>) -> Result<(), Box<dyn Error>> {
        if self.active_window.get() == Some(window) {
            return Ok(());
        }
        self.connection.change_property32(
            PropMode::REPLACE,
            self.root,
            self.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            &[window.unwrap_or(NONE)],
        )?;
        self.connection.flush()?;
        self.active_window.set(Some(window));
        Ok(())
    }

    pub fn set_allowed_actions(
        &self,
        window: Window,
        actions: AllowedActions,
    ) -> Result<(), Box<dyn Error>> {
        let mut atoms = Vec::with_capacity(8);
        if actions.move_ {
            atoms.push(self.atoms._NET_WM_ACTION_MOVE);
        }
        if actions.resize {
            atoms.push(self.atoms._NET_WM_ACTION_RESIZE);
        }
        if actions.minimize {
            atoms.push(self.atoms._NET_WM_ACTION_MINIMIZE);
        }
        if actions.maximize {
            atoms.extend([
                self.atoms._NET_WM_ACTION_MAXIMIZE_HORZ,
                self.atoms._NET_WM_ACTION_MAXIMIZE_VERT,
            ]);
        }
        if actions.fullscreen {
            atoms.push(self.atoms._NET_WM_ACTION_FULLSCREEN);
        }
        self.connection
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atoms._NET_WM_ALLOWED_ACTIONS,
                AtomEnum::ATOM,
                &atoms,
            )?
            .check()?;
        Ok(())
    }
}

fn supporting_wm_window(
    connection: &RustConnection,
    root: Window,
    atoms: &Atoms,
) -> Result<Window, Box<dyn Error>> {
    let reply = connection
        .get_property(
            false,
            root,
            atoms._NET_SUPPORTING_WM_CHECK,
            AtomEnum::WINDOW,
            0,
            1,
        )?
        .reply()?;
    reply
        .value32()
        .and_then(|mut values| values.next())
        .filter(|window| *window != NONE)
        .ok_or_else(|| "Smithay XWM did not publish _NET_SUPPORTING_WM_CHECK".into())
}

fn publish_ewmh(
    connection: &RustConnection,
    root: Window,
    support_window: Window,
    geometry: GetGeometryReply,
    atoms: &Atoms,
) -> Result<(), Box<dyn Error>> {
    connection.change_property8(
        PropMode::REPLACE,
        support_window,
        atoms._NET_WM_NAME,
        atoms.UTF8_STRING,
        b"Halley",
    )?;

    // Only advertise behavior Halley actually implements. In particular,
    // ABOVE/BELOW, SHADED, and STICKY stay absent until their compositor-side
    // policy exists rather than inheriting Smithay's broader default list.
    let supported = [
        atoms._NET_SUPPORTED,
        atoms._NET_SUPPORTING_WM_CHECK,
        atoms._NET_WM_NAME,
        atoms._NET_ACTIVE_WINDOW,
        atoms._NET_CLIENT_LIST,
        atoms._NET_CLIENT_LIST_STACKING,
        atoms._NET_NUMBER_OF_DESKTOPS,
        atoms._NET_CURRENT_DESKTOP,
        atoms._NET_DESKTOP_GEOMETRY,
        atoms._NET_DESKTOP_VIEWPORT,
        atoms._NET_WORKAREA,
        atoms._NET_WM_MOVERESIZE,
        atoms._NET_WM_STATE,
        atoms._NET_WM_STATE_MAXIMIZED_VERT,
        atoms._NET_WM_STATE_MAXIMIZED_HORZ,
        atoms._NET_WM_STATE_HIDDEN,
        atoms._NET_WM_STATE_FULLSCREEN,
        atoms._NET_WM_STATE_DEMANDS_ATTENTION,
        atoms._NET_WM_STATE_FOCUSED,
        atoms._NET_WM_STATE_SKIP_TASKBAR,
        atoms._NET_WM_STATE_SKIP_PAGER,
        atoms._NET_WM_ALLOWED_ACTIONS,
        atoms._NET_WM_ACTION_MOVE,
        atoms._NET_WM_ACTION_RESIZE,
        atoms._NET_WM_ACTION_MINIMIZE,
        atoms._NET_WM_ACTION_MAXIMIZE_HORZ,
        atoms._NET_WM_ACTION_MAXIMIZE_VERT,
        atoms._NET_WM_ACTION_FULLSCREEN,
    ];
    connection.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_SUPPORTED,
        AtomEnum::ATOM,
        &supported,
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_NUMBER_OF_DESKTOPS,
        AtomEnum::CARDINAL,
        &[1],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_CURRENT_DESKTOP,
        AtomEnum::CARDINAL,
        &[0],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_DESKTOP_GEOMETRY,
        AtomEnum::CARDINAL,
        &[u32::from(geometry.width), u32::from(geometry.height)],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_DESKTOP_VIEWPORT,
        AtomEnum::CARDINAL,
        &[0, 0],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_WORKAREA,
        AtomEnum::CARDINAL,
        &[0, 0, u32::from(geometry.width), u32::from(geometry.height)],
    )?;
    Ok(())
}
