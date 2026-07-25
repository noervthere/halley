use smithay::backend::input::{
    AbsolutePositionEvent, InputBackend, InputEvent, PointerMotionEvent,
};
use smithay::desktop::{Space, Window};
use smithay::utils::{Logical, Point, Rectangle};

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
    /// mouse) accumulate a delta. Both are kept in Smithay's global logical
    /// `Space` coordinates and constrained to its mapped output geometries.
    pub fn process_input_event<I: InputBackend>(
        &mut self,
        event: &InputEvent<I>,
        space: &Space<Window>,
    ) {
        if !matches!(
            event,
            InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. }
        ) {
            return;
        }

        let outputs: Vec<_> = space
            .outputs()
            .filter_map(|output| space.output_geometry(output))
            .collect();

        match event {
            InputEvent::PointerMotion { event } => {
                let delta = event.delta();
                self.position = clamp_to_outputs(
                    (self.position.0 + delta.x, self.position.1 + delta.y),
                    &outputs,
                );
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(bounds) = desktop_bounds(&outputs) else {
                    return;
                };
                let pos = event.position_transformed(bounds.size) + bounds.loc.to_f64();
                self.position = clamp_to_outputs((pos.x, pos.y), &outputs);
            }
            _ => {}
        }
    }
}

fn desktop_bounds(outputs: &[Rectangle<i32, Logical>]) -> Option<Rectangle<i32, Logical>> {
    outputs.iter().copied().reduce(Rectangle::merge)
}

fn clamp_to_outputs(
    position: (f64, f64),
    outputs: &[Rectangle<i32, Logical>],
) -> (f64, f64) {
    let point = Point::<f64, Logical>::from(position);
    if outputs.iter().any(|output| output.to_f64().contains(point)) {
        return position;
    }

    outputs
        .iter()
        .map(|output| {
            // `Rectangle::contains` is upper-bound-exclusive. Keeping the
            // constrained point one logical pixel inside that edge ensures
            // the same geometry will select an output for cursor rendering.
            let min = output.loc.to_f64();
            let max = Point::<f64, Logical>::from((
                (output.loc.x + output.size.w - 1) as f64,
                (output.loc.y + output.size.h - 1) as f64,
            ));
            let constrained = Point::<f64, Logical>::from((
                point.x.clamp(min.x, max.x),
                point.y.clamp(min.y, max.y),
            ));
            let dx = point.x - constrained.x;
            let dy = point.y - constrained.y;
            (dx * dx + dy * dy, constrained)
        })
        .min_by(|(left, _), (right, _)| left.total_cmp(right))
        .map_or(position, |(_, constrained)| constrained.into())
}

#[cfg(test)]
mod tests {
    use smithay::utils::Rectangle;

    use super::{clamp_to_outputs, desktop_bounds};

    #[test]
    fn clamp_leaves_positions_on_either_configured_output_unchanged() {
        let outputs = configured_outputs();
        assert_eq!(
            clamp_to_outputs((100.0, 200.0), &outputs),
            (100.0, 200.0)
        );
        assert_eq!(
            clamp_to_outputs((3000.0, 800.0), &outputs),
            (3000.0, 800.0)
        );
    }

    #[test]
    fn clamp_crosses_the_shared_edge_between_configured_outputs() {
        let outputs = configured_outputs();
        assert_eq!(
            clamp_to_outputs((2559.0, 600.0), &outputs),
            (2559.0, 600.0)
        );
        assert_eq!(
            clamp_to_outputs((2560.0, 600.0), &outputs),
            (2560.0, 600.0)
        );
    }

    #[test]
    fn clamp_pins_to_the_shorter_secondary_output() {
        let outputs = configured_outputs();
        assert_eq!(
            clamp_to_outputs((3000.0, 1300.0), &outputs),
            (3000.0, 1199.0)
        );
        assert_eq!(
            clamp_to_outputs((5000.0, -50.0), &outputs),
            (4479.0, 0.0)
        );
    }

    #[test]
    fn desktop_bounds_cover_both_configured_outputs() {
        assert_eq!(
            desktop_bounds(&configured_outputs()),
            Some(Rectangle::new((0, 0).into(), (4480, 1440).into()))
        );
    }

    fn configured_outputs() -> [Rectangle<i32, smithay::utils::Logical>; 2] {
        [
            Rectangle::new((0, 0).into(), (2560, 1440).into()),
            Rectangle::new((2560, 0).into(), (1920, 1200).into()),
        ]
    }
}
