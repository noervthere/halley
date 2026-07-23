pub mod keybinds;

use std::error::Error;

use halley_config::{Action, Modifiers};
use smithay::backend::input::{Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent};
use smithay::input::keyboard::{
    FilterResult, KeyboardHandle, KeyboardTarget, KeysymHandle, ModifiersState, XkbConfig,
};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
};
use smithay::input::touch::{
    DownEvent, MotionEvent as TouchMotionEvent, OrientationEvent, ShapeEvent, TouchTarget, UpEvent,
};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::utils::{IsAlive, SERIAL_COUNTER, Serial};

use keybinds::{BackendKind, ResolvedBind};

/// Exists only to satisfy Smithay's `Seat<D: SeatHandler>` generic - nothing
/// outside this module ever constructs or reasons about it. Real, unavoidable
/// ceremony to unlock Seat/KeyboardHandle this early (no wl_display, no real
/// clients yet), not something to pretend away by shoving Seat plumbing into
/// `App`/`TtyBackend`, which have nothing to do with it.
pub struct SeatData {
    seat_state: SeatState<SeatData>,
}

impl SeatHandler for SeatData {
    type KeyboardFocus = FocusTarget;
    type PointerFocus = FocusTarget;
    type TouchFocus = FocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

/// A placeholder focus target - no real client/surface concept exists yet.
/// Every trait method is a no-op: there is nothing to notify, since focus is
/// never actually assigned to anything this round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusTarget;

impl IsAlive for FocusTarget {
    fn alive(&self) -> bool {
        true
    }
}

impl KeyboardTarget<SeatData> for FocusTarget {
    fn enter(&self, _: &Seat<SeatData>, _: &mut SeatData, _: Vec<KeysymHandle<'_>>, _: Serial) {}
    fn leave(&self, _: &Seat<SeatData>, _: &mut SeatData, _: Serial) {}
    fn key(
        &self,
        _: &Seat<SeatData>,
        _: &mut SeatData,
        _: KeysymHandle<'_>,
        _: KeyState,
        _: Serial,
        _: u32,
    ) {
    }
    fn modifiers(&self, _: &Seat<SeatData>, _: &mut SeatData, _: ModifiersState, _: Serial) {}
}

impl PointerTarget<SeatData> for FocusTarget {
    fn enter(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &MotionEvent) {}
    fn motion(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &MotionEvent) {}
    fn relative_motion(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &RelativeMotionEvent) {}
    fn button(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &ButtonEvent) {}
    fn axis(&self, _: &Seat<SeatData>, _: &mut SeatData, _: AxisFrame) {}
    fn frame(&self, _: &Seat<SeatData>, _: &mut SeatData) {}
    fn gesture_swipe_begin(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GestureSwipeBeginEvent) {}
    fn gesture_swipe_update(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GestureSwipeUpdateEvent) {}
    fn gesture_swipe_end(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GestureSwipeEndEvent) {}
    fn gesture_pinch_begin(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GesturePinchBeginEvent) {}
    fn gesture_pinch_update(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GesturePinchUpdateEvent) {}
    fn gesture_pinch_end(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GesturePinchEndEvent) {}
    fn gesture_hold_begin(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GestureHoldBeginEvent) {}
    fn gesture_hold_end(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &GestureHoldEndEvent) {}
    fn leave(&self, _: &Seat<SeatData>, _: &mut SeatData, _: Serial, _: u32) {}
}

impl TouchTarget<SeatData> for FocusTarget {
    fn down(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &DownEvent, _: Serial) {}
    fn up(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &UpEvent, _: Serial) {}
    fn motion(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &TouchMotionEvent, _: Serial) {}
    fn frame(&self, _: &Seat<SeatData>, _: &mut SeatData, _: Serial) {}
    fn cancel(&self, _: &Seat<SeatData>, _: &mut SeatData, _: Serial) {}
    fn shape(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &ShapeEvent, _: Serial) {}
    fn orientation(&self, _: &Seat<SeatData>, _: &mut SeatData, _: &OrientationEvent, _: Serial) {}
}

fn modifiers_match(state: &ModifiersState, expected: Modifiers) -> bool {
    state.ctrl == expected.ctrl
        && state.alt == expected.alt
        && state.shift == expected.shift
        && state.logo == expected.super_key
}

/// Owns the Seat/KeyboardHandle machinery and a resolved bind table, and
/// turns raw input events into `Option<Action>` - nothing more. The matcher's
/// job ends at "this action was triggered"; it never decides what quitting
/// (or anything else) actually means for the caller's event loop, matching
/// the same narrow-signature discipline as `Renderable::render`.
pub struct Keyboard {
    seat_data: SeatData,
    keyboard: KeyboardHandle<SeatData>,
    binds: Vec<ResolvedBind>,
}

impl Keyboard {
    pub fn new(backend: BackendKind) -> Result<Self, Box<dyn Error>> {
        let mut seat_state = SeatState::new();
        let mut seat: Seat<SeatData> = seat_state.new_seat("seat-0");
        let seat_data = SeatData { seat_state };

        let keyboard = seat.add_keyboard(XkbConfig::default(), 200, 25)?;

        let keybinds = keybinds::load_keybinds();
        let binds = keybinds::resolve_binds(&keybinds, backend);

        Ok(Self {
            seat_data,
            keyboard,
            binds,
        })
    }

    /// Matches only `InputEvent::Keyboard` - no pointer/touch capability is
    /// added this round, so every other variant is ignored. Intercepts only
    /// on `KeyState::Pressed`, so a chord fires once per press, not again on
    /// release.
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) -> Option<Action> {
        let InputEvent::Keyboard { event } = event else {
            return None;
        };

        let keycode = event.key_code();
        let state = event.state();
        let time = event.time_msec();
        let binds = &self.binds;

        self.keyboard.input::<Action, _>(
            &mut self.seat_data,
            keycode,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, mods, handle| {
                if state != KeyState::Pressed {
                    return FilterResult::Forward;
                }
                // Binds are matched against the unshifted, layout-agnostic
                // symbol (mirroring niri's find_configured_bind) - Shift is
                // one of the modifiers compared separately below, so using
                // modified_sym() here would apply it twice and never match
                // any bind that includes Shift (e.g. mod+shift+e).
                let Some(keysym) = handle.raw_latin_sym_or_raw_current_sym() else {
                    return FilterResult::Forward;
                };
                match binds
                    .iter()
                    .find(|bind| bind.keysym == keysym && modifiers_match(mods, bind.modifiers))
                {
                    Some(bind) => FilterResult::Intercept(bind.action),
                    None => FilterResult::Forward,
                }
            },
        )
    }
}
