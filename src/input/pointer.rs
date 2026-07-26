use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, InputBackend, InputEvent, PointerAxisEvent,
    PointerMotionEvent,
};
use smithay::desktop::space::SpaceElement;
use smithay::desktop::{layer_map_for_output, LayerSurface, Space, Window, WindowSurfaceType};
use smithay::input::pointer::AxisFrame;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::shell::wlr_layer::Layer;

use crate::camera::OutputCameras;
use crate::input::keybinds::WheelDirection;

#[derive(Debug)]
pub enum PointerTarget {
    Layer(LayerSurface),
    Window(Window),
    Background,
}

pub type SurfaceFocus = (WlSurface, Point<f64, Logical>);
type LayerHit = (LayerSurface, SurfaceFocus);

/// A pointer route in the coordinate system used by its focused surface.
///
/// Camera-transformed windows use world coordinates so their client-local
/// position remains unscaled. Screen-fixed layer surfaces use global output
/// layout coordinates. In both cases `location` and the focus origin share
/// one coordinate system, which is Smithay's required invariant.
#[derive(Debug)]
pub struct PointerRoute {
    pub output: Output,
    pub location: Point<f64, Logical>,
    pub focus: Option<SurfaceFocus>,
    pub target: PointerTarget,
    pub visual_geometry: Option<Rectangle<i32, Logical>>,
}

/// Screen-space cursor tracking. Client-facing focus, buttons, implicit
/// grabs, and axes are owned by Smithay's `PointerHandle`; this type only
/// tracks where Halley draws the hardware/software cursor.
pub struct Pointer {
    position: (f64, f64),
}

const WHEEL_V120_PER_TICK: f64 = 120.0;

/// Accumulates high-resolution physical wheel deltas into discrete notches.
/// Each axis is independent, and reversing direction drops the unfinished
/// fraction from the previous direction.
#[derive(Debug, Default)]
pub struct WheelAccumulator {
    horizontal: f64,
    vertical: f64,
}

impl WheelAccumulator {
    pub fn accumulate(&mut self, axis: Axis, delta_v120: f64) -> i32 {
        let pending = match axis {
            Axis::Horizontal => &mut self.horizontal,
            Axis::Vertical => &mut self.vertical,
        };
        if *pending != 0.0 && delta_v120 != 0.0 && pending.signum() != delta_v120.signum() {
            *pending = 0.0;
        }
        *pending += delta_v120;
        let ticks = (*pending / WHEEL_V120_PER_TICK).trunc() as i32;
        *pending -= f64::from(ticks) * WHEEL_V120_PER_TICK;
        ticks
    }

    pub fn reset(&mut self, axis: Axis) {
        match axis {
            Axis::Horizontal => self.horizontal = 0.0,
            Axis::Vertical => self.vertical = 0.0,
        }
    }

    pub fn reset_all(&mut self) {
        self.horizontal = 0.0;
        self.vertical = 0.0;
    }
}

fn wheel_delta_v120<B, E>(event: &E, axis: Axis) -> f64
where
    B: InputBackend,
    E: PointerAxisEvent<B>,
{
    event
        .amount_v120(axis)
        .or_else(|| event.amount(axis).map(|amount| amount * WHEEL_V120_PER_TICK / 15.0))
        .unwrap_or(0.0)
}

fn wheel_direction(axis: Axis, delta_v120: f64) -> Option<WheelDirection> {
    match (axis, delta_v120.total_cmp(&0.0)) {
        (_, std::cmp::Ordering::Equal) => None,
        (Axis::Horizontal, std::cmp::Ordering::Less) => Some(WheelDirection::Left),
        (Axis::Horizontal, std::cmp::Ordering::Greater) => Some(WheelDirection::Right),
        (Axis::Vertical, std::cmp::Ordering::Less) => Some(WheelDirection::Up),
        (Axis::Vertical, std::cmp::Ordering::Greater) => Some(WheelDirection::Down),
    }
}

/// Backend-independent result of applying configured wheel bindings to one
/// axis event. Sessions execute the returned actions and forward only the
/// axes left enabled here.
pub struct WheelBindingResult<T> {
    pub forward_horizontal: bool,
    pub forward_vertical: bool,
    pub actions: Vec<(WheelDirection, T)>,
}

impl<T> Default for WheelBindingResult<T> {
    fn default() -> Self {
        Self {
            forward_horizontal: true,
            forward_vertical: true,
            actions: Vec::new(),
        }
    }
}

/// Applies physical-wheel source filtering, direction matching, high-
/// resolution accumulation, and per-axis forwarding policy in one place.
/// The callback keeps this low-level policy independent of Halley's action
/// type and bind-table representation.
pub fn process_wheel_bindings<B, E, T, F>(
    event: &E,
    accumulator: &mut WheelAccumulator,
    bindings_enabled: bool,
    mut action_for_direction: F,
) -> WheelBindingResult<T>
where
    B: InputBackend,
    E: PointerAxisEvent<B>,
    T: Clone,
    F: FnMut(WheelDirection) -> Option<T>,
{
    let mut result = WheelBindingResult::default();
    if event.source() != AxisSource::Wheel || !bindings_enabled {
        accumulator.reset_all();
        return result;
    }

    for axis in [Axis::Horizontal, Axis::Vertical] {
        let delta = wheel_delta_v120(event, axis);
        let Some(direction) = wheel_direction(axis, delta) else {
            continue;
        };
        let Some(action) = action_for_direction(direction) else {
            accumulator.reset(axis);
            continue;
        };

        match axis {
            Axis::Horizontal => result.forward_horizontal = false,
            Axis::Vertical => result.forward_vertical = false,
        }
        let ticks = accumulator.accumulate(axis, delta);
        for _ in 0..ticks.unsigned_abs() {
            result.actions.push((direction, action.clone()));
        }
    }
    result
}

impl Pointer {
    pub fn new(initial: (f64, f64)) -> Self {
        Self { position: initial }
    }

    pub fn position(&self) -> (f64, f64) {
        self.position
    }

    pub fn set_position(&mut self, position: (f64, f64)) {
        self.position = position;
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
    fullscreen: &crate::wayland::fullscreen::FullscreenManager,
    focused: Option<&WlSurface>,
    now: std::time::Duration,
    screen_position: (f64, f64),
) -> Option<PointerRoute> {
    let output = space.output_under(screen_position).next()?;
    let output_geometry = space.output_geometry(output)?;
    let screen_location = Point::<f64, Logical>::from(screen_position);
    let output_local = screen_location - output_geometry.loc.to_f64();

    if let Some((layer, focus)) = layer_under(
        output,
        output_geometry.loc,
        output_local,
        [Layer::Overlay],
    ) {
        return Some(PointerRoute {
            output: output.clone(),
            location: screen_location,
            focus: Some(focus),
            target: PointerTarget::Layer(layer),
            visual_geometry: None,
        });
    }

    if !fullscreen.covers_top(focused, output, now)
        && let Some((layer, focus)) = layer_under(
            output,
            output_geometry.loc,
            output_local,
            [Layer::Top],
        )
    {
        return Some(PointerRoute {
            output: output.clone(),
            location: screen_location,
            focus: Some(focus),
            target: PointerTarget::Layer(layer),
            visual_geometry: None,
        });
    }

    if let Some(route) = window_under(
        space,
        cameras,
        primary,
        fullscreen,
        output,
        output_local,
        now,
    ) {
        return Some(route);
    }

    let camera = cameras.get(&output.name())?;
    let world =
        crate::input::grab::screen_to_world_on_output(screen_position, camera, output_geometry);
    let location = Point::<f64, Logical>::from((world.x as f64, world.y as f64));

    if let Some((layer, focus)) = layer_under(
        output,
        output_geometry.loc,
        output_local,
        [Layer::Bottom, Layer::Background],
    ) {
        return Some(PointerRoute {
            output: output.clone(),
            location: screen_location,
            focus: Some(focus),
            target: PointerTarget::Layer(layer),
            visual_geometry: None,
        });
    }

    Some(PointerRoute {
        output: output.clone(),
        location,
        focus: None,
        target: PointerTarget::Background,
        visual_geometry: None,
    })
}

fn layer_under(
    output: &Output,
    output_location: Point<i32, Logical>,
    output_local: Point<f64, Logical>,
    layers: impl IntoIterator<Item = Layer>,
) -> Option<LayerHit> {
    let map = layer_map_for_output(output);
    for layer_kind in layers {
        for layer in map.layers_on(layer_kind).rev() {
            let Some(geometry) = map.layer_geometry(layer) else {
                continue;
            };
            let Some((surface, surface_location)) =
                layer.surface_under(output_local - geometry.loc.to_f64(), WindowSurfaceType::ALL)
            else {
                continue;
            };
            let origin = output_location + geometry.loc + surface_location;
            return Some((layer.clone(), (surface, origin.to_f64())));
        }
    }
    None
}

/// Resolves normal and fullscreen windows in one front-to-back pass.
///
/// Presentation transforms differ, but stacking order does not. A separate
/// fullscreen-first pass lets a fullscreen surface underneath a newly
/// raised normal window steal axes and clicks through that window.
fn window_under(
    space: &Space<Window>,
    cameras: &OutputCameras,
    primary: &Output,
    fullscreen: &crate::wayland::fullscreen::FullscreenManager,
    output: &Output,
    output_local: Point<f64, Logical>,
    now: std::time::Duration,
) -> Option<PointerRoute> {
    let output_geometry = space.output_geometry(output)?;
    let view = cameras.view(&output.name())?;
    let camera_center = crate::camera::global_center(view.center, output_geometry);
    let output_size = output_geometry.size.to_physical(1);
    let screen_position = (
        output_geometry.loc.x as f64 + output_local.x,
        output_geometry.loc.y as f64 + output_local.y,
    );
    let world = crate::input::grab::screen_to_world_on_output(
        screen_position,
        cameras.get(&output.name())?,
        output_geometry,
    );
    let normal_location = Point::<f64, Logical>::from((world.x as f64, world.y as f64));

    for window in space.elements().rev() {
        if !crate::wayland::window_is_on_output(window, output, primary) {
            continue;
        }
        let Some(toplevel) = window.toplevel() else {
            continue;
        };
        let Some(source_geometry) = space.element_geometry(window) else {
            continue;
        };
        let (location, hit, visual_geometry) =
            match fullscreen.presentation(toplevel.wl_surface(), output, now) {
            Some(presentation) => {
                let windowed_geometry = presentation
                    .windowed_geometry
                    .unwrap_or(source_geometry)
                    .to_physical(1);
                let windowed_screen = crate::backend::camera_rect(
                    windowed_geometry,
                    camera_center,
                    output_size,
                    view.scale,
                );
                let visual = presentation.client_rect(windowed_screen, output_size);
                (
                    inverse_map_point(output_local, visual, source_geometry),
                    visual.to_f64().contains(output_local.to_physical(1.0)),
                    visual,
                )
            }
            None => {
                let Some(bbox) = space.element_bbox(window) else {
                    continue;
                };
                (
                    normal_location,
                    bbox.to_f64().contains(normal_location),
                    crate::backend::camera_rect(
                        source_geometry.to_physical(1),
                        camera_center,
                        output_size,
                        view.scale,
                    ),
                )
            }
        };
        if !hit {
            continue;
        }

        let Some(element_location) = space.element_location(window) else {
            continue;
        };
        let render_location = element_location - window.geometry().loc;
        if !window.is_in_input_region(&(location - render_location.to_f64())) {
            continue;
        }
        let focus = window
            .surface_under(
                location - render_location.to_f64(),
                WindowSurfaceType::ALL,
            )
            .map(|(surface, surface_location)| {
                (surface, (surface_location + render_location).to_f64())
            });
        return Some(PointerRoute {
            output: output.clone(),
            location,
            focus,
            target: PointerTarget::Window(window.clone()),
            visual_geometry: Some(Rectangle::new(
                output_geometry.loc + visual_geometry.loc.to_logical(1),
                visual_geometry.size.to_logical(1),
            )),
        });
    }
    None
}

fn inverse_map_point(
    point: Point<f64, Logical>,
    visual: Rectangle<i32, smithay::utils::Physical>,
    source: Rectangle<i32, Logical>,
) -> Point<f64, Logical> {
    let scale_x = f64::from(source.size.w) / f64::from(visual.size.w.max(1));
    let scale_y = f64::from(source.size.h) / f64::from(visual.size.h.max(1));
    (
        f64::from(source.loc.x) + (point.x - f64::from(visual.loc.x)) * scale_x,
        f64::from(source.loc.y) + (point.y - f64::from(visual.loc.y)) * scale_y,
    )
        .into()
}

/// Converts one backend scroll event into the complete Smithay/Wayland axis
/// frame used by both sessions. This follows Smithay's Anvil example:
/// continuous values are preferred, wheel `v120` data is retained, and
/// finger-source zeroes become explicit stop events.
#[cfg(test)]
pub fn axis_frame<B, E>(event: &E) -> AxisFrame
where
    B: InputBackend,
    E: PointerAxisEvent<B>,
{
    axis_frame_filtered(event, true, true)
}

/// Builds a client-facing frame while omitting axes consumed by compositor
/// keybinds. This preserves a diagonal event's unbound axis.
pub fn axis_frame_filtered<B, E>(
    event: &E,
    include_horizontal: bool,
    include_vertical: bool,
) -> AxisFrame
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
    if include_horizontal && horizontal != 0.0 {
        frame = frame
            .relative_direction(Axis::Horizontal, event.relative_direction(Axis::Horizontal))
            .value(Axis::Horizontal, horizontal);
        if let Some(v120) = event.amount_v120(Axis::Horizontal) {
            frame = frame.v120(Axis::Horizontal, v120 as i32);
        }
    }
    if include_vertical && vertical != 0.0 {
        frame = frame
            .relative_direction(Axis::Vertical, event.relative_direction(Axis::Vertical))
            .value(Axis::Vertical, vertical);
        if let Some(v120) = event.amount_v120(Axis::Vertical) {
            frame = frame.v120(Axis::Vertical, v120 as i32);
        }
    }
    if event.source() == smithay::backend::input::AxisSource::Finger {
        if include_horizontal && event.amount(Axis::Horizontal) == Some(0.0) {
            frame = frame.stop(Axis::Horizontal);
        }
        if include_vertical && event.amount(Axis::Vertical) == Some(0.0) {
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
    use smithay::utils::{Logical, Physical, Rectangle};

    use super::{
        WheelAccumulator, axis_frame, axis_frame_filtered, clamp_to_outputs, desktop_bounds,
        inverse_map_point, process_wheel_bindings, wheel_delta_v120, wheel_direction,
    };
    use crate::input::keybinds::WheelDirection;

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
    fn fullscreen_pointer_coordinates_invert_the_visual_transform() {
        let visual =
            Rectangle::<i32, Physical>::new((100, 50).into(), (1600, 1200).into());
        let source =
            Rectangle::<i32, Logical>::new((400, 200).into(), (800, 600).into());

        assert_eq!(
            inverse_map_point((900.0, 650.0).into(), visual, source),
            (800.0, 500.0).into()
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
    fn filtered_axis_frame_preserves_only_the_unbound_axis() {
        let event = TestAxisEvent {
            source: AxisSource::Wheel,
            horizontal: None,
            vertical: None,
            horizontal_v120: Some(-120.0),
            vertical_v120: Some(120.0),
            horizontal_direction: AxisRelativeDirection::Identical,
            vertical_direction: AxisRelativeDirection::Identical,
        };

        let frame = axis_frame_filtered(&event, true, false);
        assert_eq!(frame.axis, (-15.0, 0.0));
        assert_eq!(frame.v120, Some((-120, 0)));
    }

    #[test]
    fn wheel_accumulator_handles_partial_multiple_and_reversed_notches() {
        let mut accumulator = WheelAccumulator::default();
        assert_eq!(accumulator.accumulate(Axis::Vertical, 30.0), 0);
        assert_eq!(accumulator.accumulate(Axis::Vertical, 90.0), 1);
        assert_eq!(accumulator.accumulate(Axis::Vertical, 300.0), 2);
        assert_eq!(accumulator.accumulate(Axis::Vertical, -30.0), 0);
        assert_eq!(accumulator.accumulate(Axis::Vertical, -90.0), -1);
    }

    #[test]
    fn wheel_accumulator_keeps_axes_independent_and_resets() {
        let mut accumulator = WheelAccumulator::default();
        assert_eq!(accumulator.accumulate(Axis::Horizontal, 60.0), 0);
        assert_eq!(accumulator.accumulate(Axis::Vertical, 120.0), 1);
        assert_eq!(accumulator.accumulate(Axis::Horizontal, 60.0), 1);
        accumulator.accumulate(Axis::Vertical, 60.0);
        accumulator.reset(Axis::Vertical);
        assert_eq!(accumulator.accumulate(Axis::Vertical, 60.0), 0);
        accumulator.reset_all();
        assert_eq!(accumulator.accumulate(Axis::Horizontal, 60.0), 0);
    }

    #[test]
    fn wheel_helpers_derive_v120_and_map_logical_directions() {
        let event = TestAxisEvent {
            source: AxisSource::Wheel,
            horizontal: Some(-15.0),
            vertical: Some(30.0),
            horizontal_v120: None,
            vertical_v120: None,
            horizontal_direction: AxisRelativeDirection::Identical,
            vertical_direction: AxisRelativeDirection::Identical,
        };
        assert_eq!(wheel_delta_v120(&event, Axis::Horizontal), -120.0);
        assert_eq!(wheel_delta_v120(&event, Axis::Vertical), 240.0);
        assert_eq!(
            wheel_direction(Axis::Horizontal, -1.0),
            Some(WheelDirection::Left)
        );
        assert_eq!(
            wheel_direction(Axis::Horizontal, 1.0),
            Some(WheelDirection::Right)
        );
        assert_eq!(
            wheel_direction(Axis::Vertical, -1.0),
            Some(WheelDirection::Up)
        );
        assert_eq!(
            wheel_direction(Axis::Vertical, 1.0),
            Some(WheelDirection::Down)
        );
        assert_eq!(wheel_direction(Axis::Vertical, 0.0), None);
    }

    #[test]
    fn wheel_binding_policy_consumes_only_matches_and_emits_full_notches() {
        let wheel = |source, vertical_v120| TestAxisEvent {
            source,
            horizontal: None,
            vertical: None,
            horizontal_v120: None,
            vertical_v120: Some(vertical_v120),
            horizontal_direction: AxisRelativeDirection::Identical,
            vertical_direction: AxisRelativeDirection::Identical,
        };
        let mut accumulator = WheelAccumulator::default();

        let first = process_wheel_bindings(
            &wheel(AxisSource::Wheel, 60.0),
            &mut accumulator,
            true,
            |direction| (direction == WheelDirection::Down).then_some("zoom-out"),
        );
        assert!(first.forward_horizontal);
        assert!(!first.forward_vertical);
        assert!(first.actions.is_empty());

        let second = process_wheel_bindings(
            &wheel(AxisSource::Wheel, 60.0),
            &mut accumulator,
            true,
            |direction| (direction == WheelDirection::Down).then_some("zoom-out"),
        );
        assert_eq!(
            second.actions,
            vec![(WheelDirection::Down, "zoom-out")]
        );

        let unbound = process_wheel_bindings(
            &wheel(AxisSource::Wheel, -120.0),
            &mut accumulator,
            true,
            |_| None::<&str>,
        );
        assert!(unbound.forward_vertical);

        process_wheel_bindings(
            &wheel(AxisSource::Wheel, 60.0),
            &mut accumulator,
            true,
            |_| Some("zoom-out"),
        );
        let bypassed = process_wheel_bindings(
            &wheel(AxisSource::Wheel, 120.0),
            &mut accumulator,
            false,
            |_| Some("zoom-out"),
        );
        assert!(bypassed.forward_vertical);
        assert!(bypassed.actions.is_empty());
        let after_reset = process_wheel_bindings(
            &wheel(AxisSource::Wheel, 60.0),
            &mut accumulator,
            true,
            |_| Some("zoom-out"),
        );
        assert!(after_reset.actions.is_empty());
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
