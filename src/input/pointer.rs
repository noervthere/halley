use smithay::backend::input::{
    AbsolutePositionEvent, InputBackend, InputEvent, PointerMotionEvent,
};
use smithay::utils::{Logical, Size};

/// Just "where should we draw the cursor" - no `Seat::add_pointer()`/
/// `PointerHandle` yet, since that machinery exists to route focused pointer
/// events to a client surface, and no client/surface concept exists yet.
/// Pulling that weight in now would repeat the "add machinery before it's
/// needed" mistake `Renderable` was designed to avoid.
pub struct Pointer {
    position: (f64, f64),
}

impl Pointer {
    pub fn new(initial: (f64, f64)) -> Self {
        Self { position: initial }
    }

    pub fn position(&self) -> (f64, f64) {
        self.position
    }

    /// Matches only `PointerMotion`/`PointerMotionAbsolute` - buttons, axes,
    /// and gestures are out of scope this round (motion is enough to prove
    /// the compositor is alive, not frozen). Absolute events (winit's host
    /// window mouse, or absolute-mode tty devices like touchpads/tablets)
    /// set the position directly; relative events (a typical tty/libinput
    /// mouse) accumulate a delta, clamped to stay within `output_size`.
    pub fn process_input_event<I: InputBackend>(
        &mut self,
        event: &InputEvent<I>,
        output_size: Size<i32, Logical>,
    ) {
        match event {
            InputEvent::PointerMotion { event } => {
                let delta = event.delta();
                self.position = clamp(
                    (self.position.0 + delta.x, self.position.1 + delta.y),
                    output_size,
                );
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let pos = event.position_transformed(output_size);
                self.position = clamp((pos.x, pos.y), output_size);
            }
            _ => {}
        }
    }
}

fn clamp(position: (f64, f64), bounds: Size<i32, Logical>) -> (f64, f64) {
    (
        position.0.clamp(0.0, bounds.w as f64),
        position.1.clamp(0.0, bounds.h as f64),
    )
}

#[cfg(test)]
mod tests {
    use smithay::utils::Size;

    use super::clamp;

    #[test]
    fn clamp_leaves_in_bounds_position_unchanged() {
        let bounds = Size::<i32, smithay::utils::Logical>::from((800, 600));
        assert_eq!(clamp((100.0, 200.0), bounds), (100.0, 200.0));
    }

    #[test]
    fn clamp_pins_to_bounds_when_out_of_range() {
        let bounds = Size::<i32, smithay::utils::Logical>::from((800, 600));
        assert_eq!(clamp((-50.0, 900.0), bounds), (0.0, 600.0));
        assert_eq!(clamp((1000.0, -10.0), bounds), (800.0, 0.0));
    }
}
