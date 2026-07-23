use smithay::backend::input::KeyState;
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
};
use smithay::input::touch::{
    DownEvent, MotionEvent as TouchMotionEvent, OrientationEvent, ShapeEvent, TouchTarget, UpEvent,
};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::utils::{IsAlive, Serial};

/// Exists only to satisfy Smithay's `Seat<D: SeatHandler>` generic - nothing
/// outside this module ever constructs or reasons about it. Real, unavoidable
/// ceremony to unlock Seat/KeyboardHandle this early (no wl_display, no real
/// clients yet), not something to pretend away by shoving Seat plumbing into
/// `App`/`TtyBackend`, which have nothing to do with it.
#[allow(dead_code)] // constructed once Keyboard::new() lands
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
#[allow(dead_code)] // used as SeatData's focus type once Keyboard::new() lands
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
