mod dbus;
mod keyboard_monitor;

use std::time::Duration;

use smithay::backend::input::{KeyState, Keycode};
use smithay::input::keyboard::xkb;

pub use keyboard_monitor::{KeyboardDisposition, KeyboardMonitorService};

use crate::session::{Session, SessionDriver};

fn translate_xkb_state(state: &xkb::State, keycode: Keycode) -> (u32, xkb::Keysym, u32) {
    (
        state.serialize_mods(xkb::STATE_MODS_EFFECTIVE),
        state.key_get_one_sym(keycode),
        state.key_get_utf32(keycode),
    )
}

pub fn process_key<D: SessionDriver>(
    session: &mut Session<D>,
    time: Duration,
    keycode: Keycode,
    state: KeyState,
) -> KeyboardDisposition {
    if session.keyboard_monitor.is_none() {
        return KeyboardDisposition::Pass;
    }
    let keyboard = session
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup");
    let (modifiers, keysym, unicode) = keyboard.with_xkb_state(session, |context| {
        let xkb = context.xkb().lock().expect("XKB state lock poisoned");
        // SAFETY: the borrowed XKB state does not outlive Smithay's locked
        // keyboard context.
        let state = unsafe { xkb.state() };
        translate_xkb_state(state, keycode)
    });
    let event = keyboard_monitor::KeyboardEvent {
        time,
        keycode,
        released: state == KeyState::Released,
        modifiers,
        keysym,
        unicode,
    };
    session
        .keyboard_monitor
        .as_ref()
        .expect("checked above")
        .process_key(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xkb_translation_reports_keysym_unicode_and_xkb_keycode_space() {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("US keymap must compile");
        let state = xkb::State::new(&keymap);
        let keycode = Keycode::new(57 + 8);

        let (modifiers, keysym, unicode) = translate_xkb_state(&state, keycode);
        assert_eq!(modifiers, 0);
        assert_eq!(keysym, xkb::Keysym::space);
        assert_eq!(unicode, ' ' as u32);
        assert_eq!(u16::try_from(keycode.raw()), Ok(65));
    }
}
