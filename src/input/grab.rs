use halley_core::camera::Camera;
use halley_core::field::Vec2;
use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
#[cfg(test)]
use smithay::utils::Physical;
use smithay::utils::{Logical, Point, Rectangle, Serial, Size};
use smithay::wayland::seat::WaylandFocus;
use std::time::Duration;

/// What's currently being dragged with the left mouse button held, if
/// anything - `None` the rest of the time. Lives on `App`/`TtyApp` next to
/// `pointer`/`camera`, mirroring how each of those was added for one
/// concrete reason.
pub enum Grab {
    None,
    /// Cursor-to-window offset in screen pixels. Keeping this in screen
    /// space preserves the exact grip point when crossing between outputs
    /// with different zoom scales.
    MoveWindow {
        id: Option<halley_core::field::NodeId>,
        window: Window,
        /// The cluster output while a tiling member is temporarily floating
        /// under pointer authority. Ordinary Field moves leave this unset.
        tiled_output: Option<String>,
        screen_offset: Vec2,
        last_world: Vec2,
        last_update: Duration,
        velocity: Vec2,
    },
    /// A node press that is still eligible to become a single-click restore.
    PendingNode {
        id: halley_core::field::NodeId,
        surface: WlSurface,
        press_screen: Point<f64, Logical>,
        screen_offset: Vec2,
    },
    /// A collapsed marker being carried without restoring its client window.
    MoveNode {
        id: halley_core::field::NodeId,
        surface: WlSurface,
        screen_offset: Vec2,
        last_world: Vec2,
        last_update: Duration,
        velocity: Vec2,
    },
    /// A cluster-core press that remains a click until it crosses the shared
    /// landmark drag threshold.
    PendingClusterCore {
        id: halley_core::cluster::ClusterId,
        output: String,
        press_screen: Point<f64, Logical>,
        screen_offset: Vec2,
    },
    /// A collapsed cluster core carried as one cluster-owned landmark.
    MoveClusterCore {
        id: halley_core::cluster::ClusterId,
        screen_offset: Vec2,
    },
    /// Left-click-drag on empty desktop. The output is captured at press
    /// time so crossing a boundary mid-drag never pans both monitors.
    Pan {
        output: String,
    },
    /// Mod+right-click-drag on a window.
    ResizeWindow(ResizeState),
}

pub(crate) fn screen_grip_offset(
    pointer: (f64, f64),
    visual_location: Point<i32, Logical>,
) -> Vec2 {
    Vec2 {
        x: visual_location.x as f32 - pointer.0 as f32,
        y: visual_location.y as f32 - pointer.1 as f32,
    }
}

pub(crate) fn screen_location_from_grip(
    pointer: (f64, f64),
    screen_offset: Vec2,
) -> Point<i32, Logical> {
    Point::from((
        (pointer.0 + f64::from(screen_offset.x)).round() as i32,
        (pointer.1 + f64::from(screen_offset.y)).round() as i32,
    ))
}

impl Grab {
    /// Whether a collapsed landmark is being pressed or moved. Both nodes and
    /// cluster cores suppress hover presentation while the pointer owns them.
    pub fn landmark_active(&self) -> bool {
        matches!(
            self,
            Self::PendingNode { .. }
                | Self::MoveNode { .. }
                | Self::PendingClusterCore { .. }
                | Self::MoveClusterCore { .. }
        )
    }
}

pub fn belongs_to_surface(grab: &Grab, surface: &WlSurface) -> bool {
    let window = match grab {
        Grab::MoveWindow { window, .. } => Some(window),
        Grab::ResizeWindow(resize) => Some(&resize.window),
        Grab::None
        | Grab::Pan { .. }
        | Grab::PendingNode { .. }
        | Grab::MoveNode { .. }
        | Grab::PendingClusterCore { .. }
        | Grab::MoveClusterCore { .. } => None,
    };
    window.is_some_and(|window| {
        window
            .wl_surface()
            .is_some_and(|candidate| candidate.as_ref() == surface)
    }) || matches!(
        grab,
        Grab::PendingNode {
            surface: candidate,
            ..
        } | Grab::MoveNode {
            surface: candidate,
            ..
        } if candidate == surface
    )
}

/// Which edges a resize drag moves. The opposite edges stay anchored, so
/// dragging the left edge grows the window leftward rather than sliding it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResizeHandle {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeHandle {
    pub fn moves_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    pub fn moves_right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    pub fn moves_top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    pub fn moves_bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }
}

/// Everything a resize drag needs, captured once at grab-start. Both the
/// rect and the cursor are in world coordinates, so the math below stays
/// independent of pan and zoom.
pub struct ResizeState {
    pub window: Window,
    pub handle: ResizeHandle,
    pub button: u32,
    pub start_rect: Rectangle<i32, Logical>,
    pub start_cursor: Vec2,
}

/// Left/top-edge anchoring state, kept *outside* `Grab` because it has to
/// outlive the drag itself.
///
/// Halley needs two states because it sends resize configures inline from
/// the motion handler, so the last serial is already known by the time the
/// button comes up.
pub struct ResizeAnchor {
    pub window: Window,
    pub handle: ResizeHandle,
    pub phase: ResizePhase,
    /// Serial of the most recent configure actually sent to this client
    /// during the drag, or `None` if the drag never changed the size.
    pub last_configure: Option<Serial>,
    /// The compositor's own copy of the window's size, and the *only* usable
    /// "before" side of the anchoring correction.
    ///
    /// It cannot be re-read from the surface at commit time: Smithay swaps
    /// pending cached state into current (`PrivateSurfaceData::commit` in
    /// `wayland/compositor/tree.rs`) *before* dispatching
    /// `CompositorHandler::commit`, so by the time this code runs
    /// `Window::geometry()` already reports the newly committed size and the
    /// correction would always work out to zero. Keeping the compositor's
    /// previous size provides the required before/after pair.
    pub last_size: Size<i32, Logical>,
}

pub enum ResizePhase {
    /// Button still held - every commit re-anchors.
    Ongoing,
    /// Button released, but the client still owes us a commit for the last
    /// configure we sent. Dropping the anchor here instead would let that
    /// final, in-flight resize land unanchored and snap the window sideways
    /// by whatever the client rounded off - exactly the jump anchoring
    /// exists to prevent, just moved to the end of the drag.
    WaitingForLastCommit(Serial),
}

/// Records a configure sent mid-drag, so the release below knows which commit
/// to wait for. A `None` serial means `send_pending_configure` found nothing
/// pending, which leaves the previous serial standing as the latest one.
pub fn note_resize_configure(anchor: &mut Option<ResizeAnchor>, serial: Option<Serial>) {
    if let Some(anchor) = anchor
        && serial.is_some()
    {
        anchor.last_configure = serial;
    }
}

/// Drops the anchor if it belonged to a window that just went away - a
/// released drag can otherwise sit in `WaitingForLastCommit` for a commit
/// that is never coming, since the client closed instead of answering.
pub fn forget_resize_anchor(anchor: &mut Option<ResizeAnchor>, surface: &WlSurface) {
    let belongs_to_surface = anchor.as_ref().is_some_and(|resize| {
        resize
            .window
            .wl_surface()
            .is_some_and(|candidate| candidate.as_ref() == surface)
    });
    if belongs_to_surface {
        *anchor = None;
    }
}

/// The drag ended. Keeps the anchor alive for one more round trip if a
/// configure is still unanswered, and drops it outright if the drag never
/// asked the client for anything.
pub fn release_resize_anchor(anchor: &mut Option<ResizeAnchor>) {
    let Some(resize) = anchor.as_mut() else {
        return;
    };
    match released_phase(resize.last_configure) {
        Some(phase) => resize.phase = phase,
        None => *anchor = None,
    }
}

/// Which phase a released drag moves to, or `None` to retire the anchor
/// immediately because no configure is outstanding.
fn released_phase(last_configure: Option<Serial>) -> Option<ResizePhase> {
    Some(ResizePhase::WaitingForLastCommit(last_configure?))
}

/// Whether the commit just processed is the one a released drag was waiting
/// for. `committed` is the serial the client had acked as of that commit;
/// `is_no_older_than` (rather than equality) because a client is free to skip
/// straight to a newer configure, which acks every older one with it.
fn anchor_is_retired(phase: &ResizePhase, committed: Option<Serial>) -> bool {
    match phase {
        ResizePhase::Ongoing => false,
        ResizePhase::WaitingForLastCommit(waiting) => {
            committed.is_some_and(|committed| committed.is_no_older_than(waiting))
        }
    }
}

/// Floor on interactive resize, matching old halley's own `.max(96.0)` /
/// `.max(72.0)` clamps. A client is free to commit something larger (a
/// terminal quantizing to whole cells, say) - nothing here fights it, since
/// this only bounds what gets *requested*.
pub const MIN_RESIZE_W: i32 = 96;
pub const MIN_RESIZE_H: i32 = 72;

/// Picks which edge or corner a press grabs, by which cell of a 3x3 grid
/// over the window it landed in: corners grab that corner, edge strips grab
/// that edge, and a press in the dead center falls back to the single
/// nearest edge. Ported from old halley's `handle_from_press_position`
/// (`halley-wl/src/input/pointer/resize/handles.rs`) so mod+right-drag picks
/// the same handle there and here.
pub fn handle_from_press_position(rect: Rectangle<i32, Logical>, point: Vec2) -> ResizeHandle {
    let left = rect.loc.x as f32;
    let top = rect.loc.y as f32;
    let width = (rect.size.w as f32).max(1.0);
    let height = (rect.size.h as f32).max(1.0);
    let fx = ((point.x - left) / width).clamp(0.0, 1.0);
    let fy = ((point.y - top) / height).clamp(0.0, 1.0);

    let near = |f: f32| f < 1.0 / 3.0;
    let far = |f: f32| f >= 2.0 / 3.0;

    match (near(fx), far(fx), near(fy), far(fy)) {
        (true, _, true, _) => ResizeHandle::TopLeft,
        (_, true, true, _) => ResizeHandle::TopRight,
        (true, _, _, true) => ResizeHandle::BottomLeft,
        (_, true, _, true) => ResizeHandle::BottomRight,
        (_, _, true, _) => ResizeHandle::Top,
        (_, _, _, true) => ResizeHandle::Bottom,
        (true, _, _, _) => ResizeHandle::Left,
        (_, true, _, _) => ResizeHandle::Right,
        // Dead center - no zone won, so resize whichever edge is closest.
        _ => {
            let to_left = point.x - left;
            let to_right = left + width - point.x;
            let to_top = point.y - top;
            let to_bottom = top + height - point.y;
            let min = to_left.min(to_right).min(to_top).min(to_bottom);
            if min == to_left {
                ResizeHandle::Left
            } else if min == to_right {
                ResizeHandle::Right
            } else if min == to_top {
                ResizeHandle::Top
            } else {
                ResizeHandle::Bottom
            }
        }
    }
}

/// The size to request, given where the cursor has been dragged to. Measured
/// against the grab-start rect rather than accumulated per event, so the
/// window can't drift away from the cursor over a long drag.
pub fn resize_target_size(
    handle: ResizeHandle,
    start_rect: Rectangle<i32, Logical>,
    start_cursor: Vec2,
    world_cursor: Vec2,
) -> Size<i32, Logical> {
    let dx = (world_cursor.x - start_cursor.x).round() as i32;
    let dy = (world_cursor.y - start_cursor.y).round() as i32;

    let mut width = start_rect.size.w;
    let mut height = start_rect.size.h;
    if handle.moves_left() {
        width -= dx;
    }
    if handle.moves_right() {
        width += dx;
    }
    if handle.moves_top() {
        height -= dy;
    }
    if handle.moves_bottom() {
        height += dy;
    }

    Size::from((width.max(MIN_RESIZE_W), height.max(MIN_RESIZE_H)))
}

/// Repositions a window after its client commits a resize, preserving the
/// edge opposite the grab. Using committed sizes here is important: clients
/// can respond asynchronously or quantize a requested size, so anchoring
/// against the request makes left/top resizes visibly jump.
///
/// The correction is `pos += previous_size - committed_size` for the
/// left/top edges only, evaluated once per commit against the size the
/// client actually landed on.
pub fn resize_location_after_commit(
    handle: ResizeHandle,
    current_location: Point<i32, Logical>,
    previous_size: Size<i32, Logical>,
    committed_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let x = if handle.moves_left() {
        current_location.x + previous_size.w - committed_size.w
    } else {
        current_location.x
    };
    let y = if handle.moves_top() {
        current_location.y + previous_size.h - committed_size.h
    } else {
        current_location.y
    };
    Point::from((x, y))
}

/// Call from `CompositorHandler::commit`, after dispatching to Smithay.
/// Applies the anchoring correction against the size this code last saw,
/// re-arms that size for the next commit, then retires the anchor if this was
/// the commit a released drag was waiting on.
///
/// Not scoped to the surface that actually committed: a window's geometry
/// only changes on its own commit, so for any other surface the size is
/// unchanged and the correction works out to zero.
///
/// The retirement check runs *after* the correction so the final commit of
/// a drag is still anchored.
pub fn finish_resize_commit(anchor: &mut Option<ResizeAnchor>, space: &mut Space<Window>) {
    let Some(resize) = anchor.as_mut() else {
        return;
    };

    if let Some(committed) = space.element_geometry(&resize.window)
        && let Some(location) = space.element_location(&resize.window)
    {
        let location =
            resize_location_after_commit(resize.handle, location, resize.last_size, committed.size);
        resize.last_size = committed.size;
        let window = resize.window.clone();
        space.relocate_element(&window, location);
    }

    let retire = anchor.as_ref().is_some_and(|resize| {
        anchor_is_retired(&resize.phase, committed_configure_serial(&resize.window))
    });
    if retire {
        *anchor = None;
    }
}

/// Serial of the configure the client had acked as of the commit currently
/// being processed - i.e. which resize request this frame is the answer to.
fn committed_configure_serial(window: &Window) -> Option<Serial> {
    window
        .toplevel()?
        .with_cached_state(|state| state.last_acked.as_ref().map(|configure| configure.serial))
}

/// Converts a screen-space (physical-pixel) position into world (`Space`)
/// coordinates, given the camera's live center/zoom state and the output's
/// physical size - the exact inverse of `render::camera_rect`'s transform.
/// Needed because the pointer's tracked position is always in screen
/// coordinates, but window positions and grab math need to be in world
/// coordinates now that panning is real.
#[cfg(test)]
fn screen_to_world(screen: (f64, f64), camera: &Camera, output_size: Size<i32, Physical>) -> Vec2 {
    let output_center_x = output_size.w as f32 / 2.0;
    let output_center_y = output_size.h as f32 / 2.0;
    let scale = crate::input::zoom::scale(camera);
    Vec2 {
        x: camera.center.x + (screen.0 as f32 - output_center_x) / scale,
        y: camera.center.y + (screen.1 as f32 - output_center_y) / scale,
    }
}

/// Multi-output form of `screen_to_world`. The pointer is global in
/// Smithay's mapped output layout, while each camera is local to its output.
/// Re-basing that output's pan/zoom into the global layout keeps render and
/// hit-test transforms exact inverses.
pub fn screen_to_world_on_output(
    screen: (f64, f64),
    camera: &Camera,
    output_geometry: Rectangle<i32, Logical>,
) -> Vec2 {
    let local_center = Vec2 {
        x: output_geometry.size.w as f32 / 2.0,
        y: output_geometry.size.h as f32 / 2.0,
    };
    let pan = Vec2 {
        x: camera.center.x - local_center.x,
        y: camera.center.y - local_center.y,
    };
    let output_center = Vec2 {
        x: output_geometry.loc.x as f32 + output_geometry.size.w as f32 / 2.0,
        y: output_geometry.loc.y as f32 + output_geometry.size.h as f32 / 2.0,
    };
    let local_screen = Vec2 {
        x: screen.0 as f32 - output_geometry.loc.x as f32,
        y: screen.1 as f32 - output_geometry.loc.y as f32,
    };
    let scale = crate::input::zoom::scale(camera);

    Vec2 {
        x: output_center.x + pan.x + (local_screen.x - local_center.x) / scale,
        y: output_center.y + pan.y + (local_screen.y - local_center.y) / scale,
    }
}

/// Converts a screen-space motion delta into a world-space delta - used
/// while panning, where only the delta matters (not an absolute position).
/// Scaled by the same factor `screen_to_world` uses, so panning speed stays
/// 1:1 with cursor motion regardless of the current zoom level (matches old
/// halley's own reason for scaling pan deltas by view-size/output-size).
pub fn screen_delta_to_world(dx: f64, dy: f64, camera: &Camera) -> Vec2 {
    let scale = crate::input::zoom::scale(camera);
    Vec2 {
        x: dx as f32 / scale,
        y: dy as f32 / scale,
    }
}

pub fn screen_offset_to_world(offset: Vec2, camera: &Camera) -> Vec2 {
    let scale = crate::input::zoom::scale(camera);
    Vec2 {
        x: offset.x / scale,
        y: offset.y / scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera_at_rest() -> Camera {
        Camera::new(
            Vec2 { x: 640.0, y: 400.0 },
            Vec2 {
                x: 1280.0,
                y: 800.0,
            },
        )
    }

    #[test]
    fn screen_to_world_is_identity_at_rest() {
        let camera = camera_at_rest();
        let output_size = Size::<i32, Physical>::from((1280, 800));
        // Output center maps to camera center (world origin here).
        assert_eq!(
            screen_to_world((640.0, 400.0), &camera, output_size),
            Vec2 { x: 640.0, y: 400.0 }
        );
        // A screen point 100px right/50px down of center is the same world
        // offset when nothing is panned or zoomed.
        let world = screen_to_world((740.0, 450.0), &camera, output_size);
        assert_eq!(world, Vec2 { x: 740.0, y: 450.0 });
    }

    #[test]
    fn screen_to_world_accounts_for_pan() {
        let mut camera = camera_at_rest();
        camera.center = Vec2 { x: 740.0, y: 400.0 };
        let output_size = Size::<i32, Physical>::from((1280, 800));
        // Output center now maps to the panned camera center, not the
        // original world origin.
        assert_eq!(
            screen_to_world((640.0, 400.0), &camera, output_size),
            Vec2 { x: 740.0, y: 400.0 }
        );
    }

    #[test]
    fn screen_to_world_accounts_for_zoom() {
        let mut camera = camera_at_rest();
        // Zoomed out to half scale (view_size double base_size).
        camera.view_size = Vec2 {
            x: 2560.0,
            y: 1600.0,
        };
        let output_size = Size::<i32, Physical>::from((1280, 800));
        // At 0.5x scale, a 100px screen offset is a 200px world offset.
        let world = screen_to_world((740.0, 400.0), &camera, output_size);
        assert_eq!(world, Vec2 { x: 840.0, y: 400.0 });
    }

    #[test]
    fn secondary_output_coordinates_are_global_at_rest() {
        let secondary = Rectangle::<i32, Logical>::new((1280, 0).into(), (1920, 1200).into());
        let camera = Camera::new(
            Vec2 { x: 960.0, y: 600.0 },
            Vec2 {
                x: 1920.0,
                y: 1200.0,
            },
        );

        assert_eq!(
            screen_to_world_on_output((1280.0, 0.0), &camera, secondary),
            Vec2 { x: 1280.0, y: 0.0 }
        );
        assert_eq!(
            screen_to_world_on_output((1600.0, 600.0), &camera, secondary),
            Vec2 {
                x: 1600.0,
                y: 600.0
            }
        );
    }

    #[test]
    fn secondary_output_coordinates_use_its_own_pan_and_zoom() {
        let mut camera = Camera::new(
            Vec2 { x: 960.0, y: 600.0 },
            Vec2 {
                x: 1920.0,
                y: 1200.0,
            },
        );
        camera.center = Vec2 {
            x: 1060.0,
            y: 650.0,
        };
        camera.view_size = Vec2 {
            x: 3840.0,
            y: 2400.0,
        };
        let secondary = Rectangle::<i32, Logical>::new((1280, 0).into(), (1920, 1200).into());

        assert_eq!(
            screen_to_world_on_output((2340.0, 650.0), &camera, secondary),
            Vec2 {
                x: 2540.0,
                y: 750.0
            }
        );
    }

    fn resize_rect() -> Rectangle<i32, Logical> {
        Rectangle::new((100, 100).into(), (300, 300).into())
    }

    #[test]
    fn press_position_picks_the_grabbed_corner() {
        let rect = resize_rect();
        // 3x3 grid over (100,100)-(400,400): thirds fall at 200 and 300.
        assert_eq!(
            handle_from_press_position(rect, Vec2 { x: 120.0, y: 120.0 }),
            ResizeHandle::TopLeft
        );
        assert_eq!(
            handle_from_press_position(rect, Vec2 { x: 380.0, y: 380.0 }),
            ResizeHandle::BottomRight
        );
        assert_eq!(
            handle_from_press_position(rect, Vec2 { x: 380.0, y: 120.0 }),
            ResizeHandle::TopRight
        );
        // Middle column, top row -> the top edge, not a corner.
        assert_eq!(
            handle_from_press_position(rect, Vec2 { x: 250.0, y: 120.0 }),
            ResizeHandle::Top
        );
    }

    #[test]
    fn dead_center_press_falls_back_to_the_nearest_edge() {
        // Exact center of a square window is equidistant - resolves to the
        // first edge checked rather than panicking or picking a corner.
        let handle = handle_from_press_position(resize_rect(), Vec2 { x: 250.0, y: 250.0 });
        assert_eq!(handle, ResizeHandle::Left);
    }

    #[test]
    fn dragging_bottom_right_grows_without_moving_the_window() {
        let rect = resize_rect();
        let start = Vec2 { x: 400.0, y: 400.0 };
        let size = resize_target_size(
            ResizeHandle::BottomRight,
            rect,
            start,
            Vec2 { x: 450.0, y: 430.0 },
        );
        assert_eq!(size, Size::from((350, 330)));
        // Right/bottom drags anchor the top-left, so the window stays put.
        assert_eq!(
            resize_location_after_commit(ResizeHandle::BottomRight, rect.loc, rect.size, size,),
            Point::from((100, 100))
        );
    }

    #[test]
    fn dragging_top_left_moves_the_window_to_keep_the_far_corner_fixed() {
        let rect = resize_rect();
        let start = Vec2 { x: 100.0, y: 100.0 };
        // Drag the top-left corner up and left by 50 - the window grows by
        // 50 in each axis and its origin moves back by the same amount, so
        // the bottom-right corner stays at (400, 400).
        let size = resize_target_size(
            ResizeHandle::TopLeft,
            rect,
            start,
            Vec2 { x: 50.0, y: 50.0 },
        );
        assert_eq!(size, Size::from((350, 350)));
        let loc = resize_location_after_commit(ResizeHandle::TopLeft, rect.loc, rect.size, size);
        assert_eq!(loc, Point::from((50, 50)));
        assert_eq!((loc.x + size.w, loc.y + size.h), (400, 400));
    }

    #[test]
    fn resize_clamps_to_the_minimum_and_stops_anchoring_past_it() {
        let rect = resize_rect();
        let start = Vec2 { x: 100.0, y: 100.0 };
        // Drag far past the opposite corner - size floors instead of going
        // negative or inverting the rect, and the anchored corner holds.
        let size = resize_target_size(
            ResizeHandle::TopLeft,
            rect,
            start,
            Vec2 {
                x: 9000.0,
                y: 9000.0,
            },
        );
        assert_eq!(size, Size::from((MIN_RESIZE_W, MIN_RESIZE_H)));
        let loc = resize_location_after_commit(ResizeHandle::TopLeft, rect.loc, rect.size, size);
        assert_eq!((loc.x + size.w, loc.y + size.h), (400, 400));
    }

    #[test]
    fn commit_size_is_authoritative_for_left_edge_anchoring() {
        let previous_size = Size::from((300, 300));
        // A terminal may commit 344 px after receiving a 350 px request.
        let committed_size = Size::from((344, 300));
        let location = resize_location_after_commit(
            ResizeHandle::Left,
            Point::from((100, 100)),
            previous_size,
            committed_size,
        );

        assert_eq!(location, Point::from((56, 100)));
        assert_eq!(location.x + committed_size.w, 400);
    }

    #[test]
    fn anchoring_diffs_against_the_tracked_size_not_the_committed_one() {
        // Regression: the "before" size used to be re-read from the surface
        // inside the commit handler, but Smithay has already swapped the new
        // geometry in by then - so every correction came out zero and a
        // left-edge drag dragged the *right* edge instead. Walk a drag as a
        // run of commits and hold the compositor's own tracked size.
        let mut location = Point::from((100, 100));
        let mut tracked = Size::from((300, 300));

        for committed in [
            Size::from((280, 300)),
            Size::from((264, 300)),
            Size::from((248, 300)),
        ] {
            location = resize_location_after_commit(
                ResizeHandle::BottomLeft,
                location,
                tracked,
                committed,
            );
            tracked = committed;
            // The un-dragged right edge holds at its original 400 throughout.
            assert_eq!(location.x + committed.w, 400);
            assert_eq!(location.y, 100);
        }

        // Feeding the committed size in as the "before" side - the bug - moves
        // nothing, leaving the right edge to walk in with the left one.
        let stuck = resize_location_after_commit(
            ResizeHandle::BottomLeft,
            Point::from((100, 100)),
            Size::from((248, 300)),
            Size::from((248, 300)),
        );
        assert_eq!(stuck, Point::from((100, 100)));
        assert_ne!(stuck.x + 248, 400);
    }

    #[test]
    fn releasing_a_drag_keeps_anchoring_until_the_last_configure_is_answered() {
        // Button up with a configure still in flight: the anchor has to
        // survive, or the client's final commit lands unanchored and the
        // window snaps sideways at the end of every left/top drag.
        let phase = released_phase(Some(Serial::from(7)))
            .expect("an outstanding configure keeps the anchor alive");

        assert!(!anchor_is_retired(&phase, None));
        assert!(!anchor_is_retired(&phase, Some(Serial::from(6))));
        assert!(anchor_is_retired(&phase, Some(Serial::from(7))));
        // Clients may skip ahead; a newer ack covers the one being waited on.
        assert!(anchor_is_retired(&phase, Some(Serial::from(9))));
    }

    #[test]
    fn releasing_a_drag_that_never_configured_retires_the_anchor() {
        assert!(released_phase(None).is_none());
    }

    #[test]
    fn an_ongoing_drag_is_never_retired_by_a_commit() {
        assert!(!anchor_is_retired(
            &ResizePhase::Ongoing,
            Some(Serial::from(9000))
        ));
    }

    #[test]
    fn screen_delta_to_world_scales_by_zoom() {
        let mut camera = camera_at_rest();
        assert_eq!(
            screen_delta_to_world(100.0, 50.0, &camera),
            Vec2 { x: 100.0, y: 50.0 }
        );

        camera.view_size = Vec2 {
            x: 2560.0,
            y: 1600.0,
        };
        assert_eq!(
            screen_delta_to_world(100.0, 50.0, &camera),
            Vec2 { x: 200.0, y: 100.0 }
        );
    }

    #[test]
    fn screen_grab_offset_stays_visually_fixed_across_zoom_levels() {
        let mut camera = camera_at_rest();
        let offset = Vec2 {
            x: -200.0,
            y: -80.0,
        };
        assert_eq!(
            screen_offset_to_world(offset, &camera),
            Vec2 {
                x: -200.0,
                y: -80.0
            }
        );

        camera.view_size = Vec2 {
            x: 2560.0,
            y: 1600.0,
        };
        assert_eq!(
            screen_offset_to_world(offset, &camera),
            Vec2 {
                x: -400.0,
                y: -160.0
            }
        );
    }

    #[test]
    fn output_local_grip_follows_the_pointer_without_field_camera_conversion() {
        let pointer = (460.0, 280.0);
        let visual_location = Point::from((320, 180));
        let offset = screen_grip_offset(pointer, visual_location);

        assert_eq!(
            offset,
            Vec2 {
                x: -140.0,
                y: -100.0
            }
        );
        assert_eq!(
            screen_location_from_grip((700.0, 510.0), offset),
            Point::from((560, 410))
        );
    }
}
