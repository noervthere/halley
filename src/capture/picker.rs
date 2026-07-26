use smithay::utils::{Logical, Point, Rectangle, Size};

const EDGE_GRAB_DISTANCE: f64 = 7.0;
const CORNER_GRAB_DISTANCE: f64 = 12.0;
const MOVE_THRESHOLD: f64 = 4.0;
const MIN_REGION_SIZE: i32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Move,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug)]
struct Interaction {
    handle: Handle,
    start_pointer: Point<f64, Logical>,
    start_region: Rectangle<i32, Logical>,
    moving: bool,
}

/// Geometry and pointer policy for the interactive area selector.
///
/// It intentionally knows nothing about screenshots, IPC, or rendering.
/// Callers draw `region()`, route pointer events here while `is_active()`,
/// and remember a region only after their capture operation succeeds.
#[derive(Debug, Default)]
pub struct RegionPicker {
    bounds: Option<Rectangle<i32, Logical>>,
    region: Option<Rectangle<i32, Logical>>,
    remembered: Option<Rectangle<i32, Logical>>,
    interaction: Option<Interaction>,
}

impl RegionPicker {
    pub fn is_active(&self) -> bool {
        self.region.is_some()
    }

    pub fn region(&self) -> Option<Rectangle<i32, Logical>> {
        self.region
    }

    pub fn begin(
        &mut self,
        desktop_bounds: Rectangle<i32, Logical>,
        active_output: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        self.bounds = Some(desktop_bounds);
        self.interaction = None;
        let initial = self
            .remembered
            .map(|region| clamp_region(region, desktop_bounds))
            .unwrap_or_else(|| centered_half(active_output));
        let initial = clamp_region(initial, desktop_bounds);
        self.region = Some(initial);
        initial
    }

    /// Keeps active and remembered geometry valid after an output-layout
    /// change. Regions may span outputs and transparent gaps, so clamping is
    /// against the desktop's enclosing rectangle.
    pub fn update_bounds(&mut self, desktop_bounds: Rectangle<i32, Logical>) {
        self.bounds = Some(desktop_bounds);
        self.region = self
            .region
            .map(|region| clamp_region(region, desktop_bounds));
        self.remembered = self
            .remembered
            .map(|region| clamp_region(region, desktop_bounds));
        self.interaction = None;
    }

    /// Starts a potential move or resize. Every press is consumed while the
    /// picker is active, but presses outside the region deliberately do
    /// nothing.
    pub fn press(&mut self, pointer: Point<f64, Logical>) -> bool {
        let Some(region) = self.region else {
            return false;
        };
        self.interaction = hit_test(region, pointer).map(|handle| Interaction {
            handle,
            start_pointer: pointer,
            start_region: region,
            moving: handle != Handle::Move,
        });
        true
    }

    pub fn motion(&mut self, pointer: Point<f64, Logical>) -> bool {
        let (Some(bounds), Some(mut interaction)) = (self.bounds, self.interaction) else {
            return self.is_active();
        };
        let delta = pointer - interaction.start_pointer;
        if interaction.handle == Handle::Move && !interaction.moving {
            interaction.moving = delta.x.hypot(delta.y) >= MOVE_THRESHOLD;
            self.interaction = Some(interaction);
            if !interaction.moving {
                return true;
            }
        }

        self.region = Some(transform_region(interaction, delta, bounds));
        true
    }

    pub fn release(&mut self) -> bool {
        if !self.is_active() {
            return false;
        }
        self.interaction = None;
        true
    }

    /// Finishes selection without changing session memory. The caller must
    /// invoke `remember_successful` only once the screenshot is safely
    /// written.
    pub fn accept(&mut self) -> Option<Rectangle<i32, Logical>> {
        self.interaction = None;
        self.region.take()
    }

    pub fn cancel(&mut self) -> bool {
        self.interaction = None;
        self.region.take().is_some()
    }

    pub fn remember_successful(&mut self, region: Rectangle<i32, Logical>) {
        self.remembered = Some(match self.bounds {
            Some(bounds) => clamp_region(region, bounds),
            None => region,
        });
    }

    #[cfg(test)]
    fn remembered(&self) -> Option<Rectangle<i32, Logical>> {
        self.remembered
    }
}

fn centered_half(output: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let size = Size::from((
        (output.size.w / 2).max(MIN_REGION_SIZE).min(output.size.w),
        (output.size.h / 2).max(MIN_REGION_SIZE).min(output.size.h),
    ));
    Rectangle::new(
        (
            output.loc.x + (output.size.w - size.w) / 2,
            output.loc.y + (output.size.h - size.h) / 2,
        )
            .into(),
        size,
    )
}

fn clamp_region(
    region: Rectangle<i32, Logical>,
    bounds: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let size = Size::from((
        region.size.w.max(1).min(bounds.size.w.max(1)),
        region.size.h.max(1).min(bounds.size.h.max(1)),
    ));
    let x = region
        .loc
        .x
        .clamp(bounds.loc.x, bounds.loc.x + bounds.size.w - size.w);
    let y = region
        .loc
        .y
        .clamp(bounds.loc.y, bounds.loc.y + bounds.size.h - size.h);
    Rectangle::new((x, y).into(), size)
}

fn hit_test(region: Rectangle<i32, Logical>, pointer: Point<f64, Logical>) -> Option<Handle> {
    let left = f64::from(region.loc.x);
    let top = f64::from(region.loc.y);
    let right = f64::from(region.loc.x + region.size.w);
    let bottom = f64::from(region.loc.y + region.size.h);

    for (x, y, handle) in [
        (left, top, Handle::TopLeft),
        (right, top, Handle::TopRight),
        (left, bottom, Handle::BottomLeft),
        (right, bottom, Handle::BottomRight),
    ] {
        if (pointer.x - x).hypot(pointer.y - y) <= CORNER_GRAB_DISTANCE {
            return Some(handle);
        }
    }

    let within_x = pointer.x >= left && pointer.x <= right;
    let within_y = pointer.y >= top && pointer.y <= bottom;
    if within_x && (pointer.y - top).abs() <= EDGE_GRAB_DISTANCE {
        return Some(Handle::Top);
    }
    if within_x && (pointer.y - bottom).abs() <= EDGE_GRAB_DISTANCE {
        return Some(Handle::Bottom);
    }
    if within_y && (pointer.x - left).abs() <= EDGE_GRAB_DISTANCE {
        return Some(Handle::Left);
    }
    if within_y && (pointer.x - right).abs() <= EDGE_GRAB_DISTANCE {
        return Some(Handle::Right);
    }
    (within_x && within_y).then_some(Handle::Move)
}

fn transform_region(
    interaction: Interaction,
    delta: Point<f64, Logical>,
    bounds: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let dx = delta.x.round() as i32;
    let dy = delta.y.round() as i32;
    let start = interaction.start_region;
    if interaction.handle == Handle::Move {
        return clamp_region(
            Rectangle::new((start.loc.x + dx, start.loc.y + dy).into(), start.size),
            bounds,
        );
    }

    let mut left = start.loc.x;
    let mut top = start.loc.y;
    let mut right = start.loc.x + start.size.w;
    let mut bottom = start.loc.y + start.size.h;
    let bounds_right = bounds.loc.x + bounds.size.w;
    let bounds_bottom = bounds.loc.y + bounds.size.h;

    if matches!(
        interaction.handle,
        Handle::Left | Handle::TopLeft | Handle::BottomLeft
    ) {
        left = (left + dx).clamp(bounds.loc.x, right - MIN_REGION_SIZE);
    }
    if matches!(
        interaction.handle,
        Handle::Right | Handle::TopRight | Handle::BottomRight
    ) {
        right = (right + dx).clamp(left + MIN_REGION_SIZE, bounds_right);
    }
    if matches!(
        interaction.handle,
        Handle::Top | Handle::TopLeft | Handle::TopRight
    ) {
        top = (top + dy).clamp(bounds.loc.y, bottom - MIN_REGION_SIZE);
    }
    if matches!(
        interaction.handle,
        Handle::Bottom | Handle::BottomLeft | Handle::BottomRight
    ) {
        bottom = (bottom + dy).clamp(top + MIN_REGION_SIZE, bounds_bottom);
    }

    Rectangle::new((left, top).into(), (right - left, bottom - top).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Rectangle<i32, Logical> {
        Rectangle::new((0, 0).into(), (2000, 1000).into())
    }

    fn output() -> Rectangle<i32, Logical> {
        Rectangle::new((1000, 0).into(), (1000, 1000).into())
    }

    #[test]
    fn first_region_is_centered_at_half_the_active_output() {
        let mut picker = RegionPicker::default();
        assert_eq!(
            picker.begin(bounds(), output()),
            Rectangle::new((1250, 250).into(), (500, 500).into())
        );
    }

    #[test]
    fn clicking_outside_or_inside_without_dragging_is_a_no_op() {
        let mut picker = RegionPicker::default();
        let initial = picker.begin(bounds(), output());

        assert!(picker.press((100.0, 100.0).into()));
        picker.motion((500.0, 500.0).into());
        picker.release();
        assert_eq!(picker.region(), Some(initial));

        picker.press((1400.0, 400.0).into());
        picker.release();
        assert_eq!(picker.region(), Some(initial));
    }

    #[test]
    fn moving_preserves_the_grab_point_after_a_small_threshold() {
        let mut picker = RegionPicker::default();
        let initial = picker.begin(bounds(), output());
        picker.press((1300.0, 300.0).into());
        picker.motion((1302.0, 302.0).into());
        assert_eq!(picker.region(), Some(initial));

        picker.motion((1400.0, 350.0).into());
        assert_eq!(
            picker.region(),
            Some(Rectangle::new((1350, 300).into(), (500, 500).into()))
        );
    }

    #[test]
    fn corner_and_side_grips_resize_while_opposite_edges_stay_fixed() {
        let mut picker = RegionPicker::default();
        picker.begin(bounds(), output());
        picker.press((1250.0, 250.0).into());
        picker.motion((1200.0, 200.0).into());
        picker.release();
        assert_eq!(
            picker.region(),
            Some(Rectangle::new((1200, 200).into(), (550, 550).into()))
        );

        picker.press((1750.0, 500.0).into());
        picker.motion((1800.0, 500.0).into());
        assert_eq!(
            picker.region(),
            Some(Rectangle::new((1200, 200).into(), (600, 550).into()))
        );
    }

    #[test]
    fn accepting_does_not_remember_until_capture_succeeds() {
        let mut picker = RegionPicker::default();
        let selected = picker.begin(bounds(), output());
        assert_eq!(picker.accept(), Some(selected));
        assert_eq!(picker.remembered(), None);

        picker.remember_successful(selected);
        assert_eq!(picker.remembered(), Some(selected));
    }

    #[test]
    fn cancelling_clears_only_the_active_selection() {
        let mut picker = RegionPicker::default();
        let selected = picker.begin(bounds(), output());
        picker.remember_successful(selected);

        assert!(picker.cancel());
        assert!(!picker.is_active());
        assert_eq!(picker.remembered(), Some(selected));
        assert!(!picker.cancel());
    }

    #[test]
    fn remembered_region_is_clamped_after_layout_changes() {
        let mut picker = RegionPicker::default();
        let selected = picker.begin(bounds(), output());
        picker.remember_successful(selected);
        picker.update_bounds(Rectangle::new((0, 0).into(), (800, 600).into()));

        assert_eq!(
            picker.remembered(),
            Some(Rectangle::new((300, 100).into(), (500, 500).into()))
        );
    }
}
