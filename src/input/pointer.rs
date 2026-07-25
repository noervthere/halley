use smithay::backend::input::{
    AbsolutePositionEvent, Axis, InputBackend, InputEvent, PointerAxisEvent, PointerMotionEvent,
};
use smithay::desktop::{Space, Window, WindowSurfaceType};
use smithay::input::pointer::AxisFrame;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};

use crate::camera::OutputCameras;

/// The result of projecting Halley's screen-space cursor through one
/// output's camera into Smithay's world space.
///
/// Smithay requires both the pointer location and the focused surface's
/// origin to use the same global coordinate system. Keeping them in world
/// coordinates makes their difference the client's unscaled surface-local
/// coordinate even while the output is panned or zoomed.
pub struct PointerRoute {
    pub location: Point<f64, Logical>,
    pub focus: Option<(WlSurface, Point<f64, Logical>)>,
}

/// Screen-space cursor tracking. Client-facing focus, buttons, implicit
/// grabs, and axes are owned by Smithay's `PointerHandle`; this type only
/// tracks where Halley draws the hardware/software cursor.
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

    /// Absolute events (winit's host window mouse, or absolute-mode tty
    /// devices like touchpads/tablets) set the position directly; relative
    /// events (a typical tty/libinput mouse) accumulate a delta. Both are
    /// kept in Smithay's global logical output-layout coordinates and
    /// constrained to mapped output geometries.
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

/// Finds the client surface visually under `screen_position`.
///
/// Window ownership is output-local, matching rendering and compositor grab
/// hit-testing: a window assigned to another output cannot intercept input
/// merely because its world-space geometry overlaps this camera's view.
pub fn route_to_client(
    space: &Space<Window>,
    cameras: &OutputCameras,
    primary: &Output,
    screen_position: (f64, f64),
) -> Option<PointerRoute> {
    let output = space.output_under(screen_position).next()?;
    let output_geometry = space.output_geometry(output)?;
    let camera = cameras.get(&output.name())?;
    let world =
        crate::input::grab::screen_to_world_on_output(screen_position, camera, output_geometry);
    let location = Point::<f64, Logical>::from((world.x as f64, world.y as f64));

    let window_and_origin =
        crate::input::grab::window_under_on_output(space, output, primary, world);
    let focus = window_and_origin
        .as_ref()
        .and_then(|(window, render_location)| {
            window
                .surface_under(location - render_location.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, surface_location)| {
                    (surface, (surface_location + *render_location).to_f64())
                })
        });

    Some(PointerRoute { location, focus })
}

/// Converts one backend scroll event into the complete Smithay/Wayland axis
/// frame used by both sessions. This follows Smithay's Anvil example:
/// continuous values are preferred, wheel `v120` data is retained, and
/// finger-source zeroes become explicit stop events.
pub fn axis_frame<B, E>(event: &E) -> AxisFrame
where
    B: InputBackend,
    E: PointerAxisEvent<B>,
{
    let horizontal = event
        .amount(Axis::Horizontal)
        .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
    let vertical = event
        .amount(Axis::Vertical)
        .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);

    let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
    if horizontal != 0.0 {
        frame = frame
            .relative_direction(Axis::Horizontal, event.relative_direction(Axis::Horizontal))
            .value(Axis::Horizontal, horizontal);
        if let Some(v120) = event.amount_v120(Axis::Horizontal) {
            frame = frame.v120(Axis::Horizontal, v120 as i32);
        }
    }
    if vertical != 0.0 {
        frame = frame
            .relative_direction(Axis::Vertical, event.relative_direction(Axis::Vertical))
            .value(Axis::Vertical, vertical);
        if let Some(v120) = event.amount_v120(Axis::Vertical) {
            frame = frame.v120(Axis::Vertical, v120 as i32);
        }
    }
    if event.source() == smithay::backend::input::AxisSource::Finger {
        if event.amount(Axis::Horizontal) == Some(0.0) {
            frame = frame.stop(Axis::Horizontal);
        }
        if event.amount(Axis::Vertical) == Some(0.0) {
            frame = frame.stop(Axis::Vertical);
        }
    }
    frame
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
    use std::path::PathBuf;

    use smithay::backend::input::{
        Axis, AxisRelativeDirection, AxisSource, Device, DeviceCapability, Event, InputBackend,
        PointerAxisEvent, UnusedEvent,
    };
    use smithay::utils::Rectangle;

    use super::{axis_frame, clamp_to_outputs, desktop_bounds};

    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct TestDevice;

    impl Device for TestDevice {
        fn id(&self) -> String {
            "test-pointer".into()
        }

        fn name(&self) -> String {
            "test pointer".into()
        }

        fn has_capability(&self, capability: DeviceCapability) -> bool {
            capability == DeviceCapability::Pointer
        }

        fn usb_id(&self) -> Option<(u32, u32)> {
            None
        }

        fn syspath(&self) -> Option<PathBuf> {
            None
        }
    }

    struct TestBackend;

    impl InputBackend for TestBackend {
        type Device = TestDevice;
        type KeyboardKeyEvent = UnusedEvent;
        type PointerAxisEvent = TestAxisEvent;
        type PointerButtonEvent = UnusedEvent;
        type PointerMotionEvent = UnusedEvent;
        type PointerMotionAbsoluteEvent = UnusedEvent;
        type GestureSwipeBeginEvent = UnusedEvent;
        type GestureSwipeUpdateEvent = UnusedEvent;
        type GestureSwipeEndEvent = UnusedEvent;
        type GesturePinchBeginEvent = UnusedEvent;
        type GesturePinchUpdateEvent = UnusedEvent;
        type GesturePinchEndEvent = UnusedEvent;
        type GestureHoldBeginEvent = UnusedEvent;
        type GestureHoldEndEvent = UnusedEvent;
        type TouchDownEvent = UnusedEvent;
        type TouchUpEvent = UnusedEvent;
        type TouchMotionEvent = UnusedEvent;
        type TouchCancelEvent = UnusedEvent;
        type TouchFrameEvent = UnusedEvent;
        type TabletToolAxisEvent = UnusedEvent;
        type TabletToolProximityEvent = UnusedEvent;
        type TabletToolTipEvent = UnusedEvent;
        type TabletToolButtonEvent = UnusedEvent;
        type SwitchToggleEvent = UnusedEvent;
        type SpecialEvent = ();
    }

    struct TestAxisEvent {
        source: AxisSource,
        horizontal: Option<f64>,
        vertical: Option<f64>,
        horizontal_v120: Option<f64>,
        vertical_v120: Option<f64>,
        horizontal_direction: AxisRelativeDirection,
        vertical_direction: AxisRelativeDirection,
    }

    impl Event<TestBackend> for TestAxisEvent {
        fn time(&self) -> u64 {
            42_000
        }

        fn device(&self) -> TestDevice {
            TestDevice
        }
    }

    impl PointerAxisEvent<TestBackend> for TestAxisEvent {
        fn amount(&self, axis: Axis) -> Option<f64> {
            match axis {
                Axis::Horizontal => self.horizontal,
                Axis::Vertical => self.vertical,
            }
        }

        fn amount_v120(&self, axis: Axis) -> Option<f64> {
            match axis {
                Axis::Horizontal => self.horizontal_v120,
                Axis::Vertical => self.vertical_v120,
            }
        }

        fn source(&self) -> AxisSource {
            self.source
        }

        fn relative_direction(&self, axis: Axis) -> AxisRelativeDirection {
            match axis {
                Axis::Horizontal => self.horizontal_direction,
                Axis::Vertical => self.vertical_direction,
            }
        }
    }

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

    #[test]
    fn wheel_axis_frame_keeps_v120_and_derives_continuous_values() {
        let event = TestAxisEvent {
            source: AxisSource::Wheel,
            horizontal: None,
            vertical: None,
            horizontal_v120: Some(-240.0),
            vertical_v120: Some(120.0),
            horizontal_direction: AxisRelativeDirection::Inverted,
            vertical_direction: AxisRelativeDirection::Identical,
        };

        let frame = axis_frame(&event);
        assert_eq!(frame.time, 42);
        assert_eq!(frame.source, Some(AxisSource::Wheel));
        assert_eq!(frame.axis, (-30.0, 15.0));
        assert_eq!(frame.v120, Some((-240, 120)));
        assert_eq!(
            frame.relative_direction,
            (
                AxisRelativeDirection::Inverted,
                AxisRelativeDirection::Identical
            )
        );
        assert_eq!(frame.stop, (false, false));
    }

    #[test]
    fn finger_axis_frame_marks_zero_axes_stopped() {
        let event = TestAxisEvent {
            source: AxisSource::Finger,
            horizontal: Some(0.0),
            vertical: Some(0.0),
            horizontal_v120: None,
            vertical_v120: None,
            horizontal_direction: AxisRelativeDirection::Identical,
            vertical_direction: AxisRelativeDirection::Identical,
        };

        let frame = axis_frame(&event);
        assert_eq!(frame.source, Some(AxisSource::Finger));
        assert_eq!(frame.axis, (0.0, 0.0));
        assert_eq!(frame.v120, None);
        assert_eq!(frame.stop, (true, true));
    }

    fn configured_outputs() -> [Rectangle<i32, smithay::utils::Logical>; 2] {
        [
            Rectangle::new((0, 0).into(), (2560, 1440).into()),
            Rectangle::new((2560, 0).into(), (1920, 1200).into()),
        ]
    }
}
