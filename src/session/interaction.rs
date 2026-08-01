use crate::input::grab::{Grab, ResizeAnchor};
use crate::input::pointer::WheelAccumulator;
use crate::input::{SuppressedButtons, SuppressedKeys};

/// Mutable state shared by compositor-owned input interactions.
///
/// These values advance and retire together across grabs, modal interception,
/// focus changes, and pointer constraints. Grouping them makes that lifecycle
/// explicit and keeps the session coordinator from exposing six independent
/// mutation points.
pub struct InteractionState {
    pub(crate) grab: Grab,
    pub(crate) resize_anchor: Option<ResizeAnchor>,
    pub(crate) suppressed_buttons: SuppressedButtons,
    pub(crate) suppressed_keys: SuppressedKeys,
    pub(crate) wheel_accumulator: WheelAccumulator,
    pub(crate) pointer_constraints: super::pointer::PointerConstraintLifecycle,
}

impl Default for InteractionState {
    fn default() -> Self {
        Self {
            grab: Grab::None,
            resize_anchor: None,
            suppressed_buttons: SuppressedButtons::default(),
            suppressed_keys: SuppressedKeys::default(),
            wheel_accumulator: WheelAccumulator::default(),
            pointer_constraints: super::pointer::PointerConstraintLifecycle::default(),
        }
    }
}
