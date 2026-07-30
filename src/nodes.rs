use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use halley_core::decay::{DecayClass, DecayTracker, TimedDecayPolicy};
use halley_core::field::{Field, NodeId, NodeState, Vec2};
use halley_core::viewport::FocusRing as CoreFocusRing;
use smithay::desktop::{PopupManager, Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use smithay::wayland::shell::xdg::dialog::ToplevelDialogHint;

#[path = "nodes/dynamics.rs"]
mod dynamics;

const OUTSIDE_THRESHOLD: f32 = 0.90;
pub const NODE_DIAMETER_PX: f32 = 51.0;
const LANDMARK_SLIDE_MS: u64 = 520;
const RELEASE_LOCK_MS: u64 = 350;

fn release_lock_deadline(now: Duration) -> Duration {
    now.saturating_add(Duration::from_millis(RELEASE_LOCK_MS))
}

fn release_lock_is_active(until: Duration, now: Duration) -> bool {
    now < until
}

fn logical_focus_after_collapse(
    focused: Option<NodeId>,
    collapsed: NodeId,
    client_was_focused: bool,
) -> Option<NodeId> {
    if client_was_focused || focused == Some(collapsed) {
        Some(collapsed)
    } else {
        focused
    }
}

#[derive(Clone, Copy)]
struct LandmarkSlide {
    from: Vec2,
    to: Vec2,
    started: Duration,
}

#[derive(Clone)]
pub struct NodeRecord {
    pub id: NodeId,
    pub window: Window,
    pub surface: WlSurface,
    pub output: String,
    pub geometry: Rectangle<i32, Logical>,
    pub collapsed: bool,
    pub attached: bool,
    pub title: String,
    pub app_id: Option<String>,
    pub collapsed_at: Duration,
    pub collapsed_stack_index: Option<usize>,
}

pub struct NodesState {
    pub field: Field,
    records: HashMap<NodeId, NodeRecord>,
    by_surface: HashMap<WlSurface, NodeId>,
    decay: DecayTracker,
    pub config: halley_config::Nodes,
    pub decay_config: halley_config::Decay,
    pub focus_rings: halley_config::FocusRings,
    pub landmarks: halley_config::LandmarkPlacement,
    pub physics: halley_config::Physics,
    pub debug: halley_config::Debug,
    pub animation: halley_config::NodeAnimation,
    pub animations_enabled: bool,
    pub hovered: Option<NodeId>,
    focused: Option<NodeId>,
    last_focus_ms: HashMap<NodeId, u64>,
    label_hover: RefCell<HashMap<NodeId, f32>>,
    landmark_slides: RefCell<HashMap<NodeId, LandmarkSlide>>,
    physics_velocity: HashMap<NodeId, Vec2>,
    physics_last_tick: Duration,
    release_locks: HashMap<NodeId, Duration>,
    ring_preview_until: HashMap<String, Duration>,
}

impl NodesState {
    pub fn new(config: &halley_config::RuntimeConfig) -> Self {
        Self {
            field: Field::new(),
            records: HashMap::new(),
            by_surface: HashMap::new(),
            decay: DecayTracker::default(),
            config: config.nodes,
            decay_config: config.decay,
            focus_rings: config.focus_rings.clone(),
            landmarks: halley_config::LandmarkPlacement {
                gap_px: config.field.gap,
            },
            physics: config.physics,
            debug: config.debug,
            animation: config.animations.node,
            animations_enabled: config.animations.enabled,
            hovered: None,
            focused: None,
            last_focus_ms: HashMap::new(),
            label_hover: RefCell::new(HashMap::new()),
            landmark_slides: RefCell::new(HashMap::new()),
            physics_velocity: HashMap::new(),
            physics_last_tick: crate::frame_clock::monotonic_now(),
            release_locks: HashMap::new(),
            ring_preview_until: HashMap::new(),
        }
    }

    pub fn reload(&mut self, config: &halley_config::RuntimeConfig, now: Duration) -> bool {
        let ring_changed = self.focus_rings != config.focus_rings;
        let configured_outputs = self
            .records
            .values()
            .map(|record| record.output.clone())
            .chain(self.focus_rings.by_output.keys().cloned())
            .chain(config.focus_rings.by_output.keys().cloned())
            .collect::<HashSet<_>>();
        let changed_ring_outputs = configured_outputs
            .into_iter()
            .filter(|output| {
                self.focus_rings.for_output(output) != config.focus_rings.for_output(output)
            })
            .collect::<HashSet<_>>();
        let redraw = ring_changed
            || self.config != config.nodes
            || self.debug != config.debug
            || self.animation != config.animations.node
            || self.animations_enabled != config.animations.enabled;
        if ring_changed {
            let until = now.saturating_add(Duration::from_millis(1_500));
            for output in &changed_ring_outputs {
                self.ring_preview_until.insert(output.clone(), until);
            }
        }
        let next_landmarks = halley_config::LandmarkPlacement {
            gap_px: config.field.gap,
        };
        let landmarks_changed = self.landmarks != next_landmarks;
        let physics_changed = self.physics != config.physics;
        if physics_changed {
            self.physics_velocity.clear();
            self.physics_last_tick = now;
            self.release_locks.clear();
        }
        if self.decay_config != config.decay {
            self.decay.reset();
        } else if ring_changed {
            let changed_ids = self
                .records
                .values()
                .filter(|record| changed_ring_outputs.contains(&record.output))
                .map(|record| record.id)
                .collect::<Vec<_>>();
            for id in changed_ids {
                self.decay.remove(id);
            }
        }
        self.config = config.nodes;
        self.decay_config = config.decay;
        self.focus_rings = config.focus_rings.clone();
        self.landmarks = next_landmarks;
        self.physics = config.physics;
        self.debug = config.debug;
        self.animation = config.animations.node;
        self.animations_enabled = config.animations.enabled;
        redraw || landmarks_changed || physics_changed
    }

    pub fn focus_ring_for_output(&self, output: &str) -> halley_config::FocusRing {
        self.focus_rings.for_output(output)
    }

    pub fn ring_is_previewed(&self, output: &str, now: Duration) -> bool {
        self.ring_preview_until
            .get(output)
            .is_some_and(|until| now < *until)
    }

    pub fn register_mapped(&mut self, space: &Space<Window>, surface: &WlSurface, now_ms: u64) {
        let Some(window) = space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == surface)
            })
            .cloned()
        else {
            return;
        };
        if crate::xwayland::is_override_redirect(&window) {
            return;
        }
        let Some(geometry) = space.element_geometry(&window) else {
            return;
        };
        let output = crate::wayland::window_output_name(&window).unwrap_or_default();
        let (title, app_id) = metadata(&window);
        if let Some(id) = self.by_surface.get(surface).copied() {
            if let Some(record) = self.records.get_mut(&id) {
                record.window = window;
                record.output = output;
                record.geometry = geometry;
                record.attached = true;
                record.title = title.clone();
                record.app_id = app_id;
            }
            let _ = self.field.set_detached(id, false);
            if let Some(node) = self.field.node_mut(id) {
                node.label = title;
                node.pos = rect_center(geometry);
                node.intrinsic_size = vec_size(geometry);
            }
            let _ = self.field.touch(id, now_ms);
            return;
        }

        let id = self
            .field
            .spawn_surface(title.clone(), rect_center(geometry), vec_size(geometry));
        let _ = self.field.touch(id, now_ms);
        self.by_surface.insert(surface.clone(), id);
        self.records.insert(
            id,
            NodeRecord {
                id,
                window,
                surface: surface.clone(),
                output,
                geometry,
                collapsed: false,
                attached: true,
                title,
                app_id,
                collapsed_at: Duration::ZERO,
                collapsed_stack_index: None,
            },
        );
    }

    pub fn sync_from_space(&mut self, space: &Space<Window>) {
        let ids = self.records.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            if !record.attached {
                continue;
            }
            let (title, app_id) = metadata(&record.window);
            record.title = title.clone();
            record.app_id = app_id;
            if let Some(node) = self.field.node_mut(id) {
                node.label = title;
            }
            if record.collapsed {
                continue;
            }
            let Some(geometry) = space.element_geometry(&record.window) else {
                continue;
            };
            record.geometry = geometry;
            record.output = crate::wayland::window_output_name(&record.window).unwrap_or_default();
            if let Some(node) = self.field.node_mut(id) {
                node.pos = rect_center(geometry);
                node.intrinsic_size = vec_size(geometry);
                if node.state == NodeState::Active {
                    node.footprint = node.intrinsic_size;
                }
            }
        }
    }

    pub fn mark_detached(&mut self, surface: &WlSurface) {
        let Some(id) = self.by_surface.get(surface).copied() else {
            return;
        };
        if let Some(record) = self.records.get_mut(&id) {
            record.attached = false;
        }
        let _ = self.field.set_detached(id, true);
        self.release_locks.remove(&id);
        self.decay.remove(id);
        if self.focused == Some(id) {
            self.focused = None;
        }
    }

    pub fn remove_surface(&mut self, surface: &WlSurface) -> Option<NodeRecord> {
        let id = self.by_surface.remove(surface)?;
        self.label_hover.borrow_mut().remove(&id);
        self.landmark_slides.borrow_mut().remove(&id);
        self.physics_velocity.remove(&id);
        self.release_locks.remove(&id);
        self.decay.remove(id);
        self.last_focus_ms.remove(&id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.field.remove(id);
        self.records.remove(&id)
    }

    pub fn id_for_surface(&self, surface: &WlSurface) -> Option<NodeId> {
        self.by_surface.get(surface).copied()
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focused
            .filter(|id| self.records.get(id).is_some_and(|record| record.attached))
    }

    pub fn focused_on_output(&self, output: &str) -> Option<NodeId> {
        self.focused()
            .filter(|id| {
                self.records
                    .get(id)
                    .is_some_and(|record| record.output == output)
            })
            .or_else(|| {
                self.last_focus_ms
                    .iter()
                    .filter_map(|(id, focused_at)| {
                        self.records
                            .get(id)
                            .filter(|record| record.attached && record.output == output)
                            .map(|_| (*focused_at, id.as_u64(), *id))
                    })
                    .max_by_key(|(focused_at, stable_id, _)| (*focused_at, *stable_id))
                    .map(|(_, _, id)| id)
            })
    }

    pub fn last_focus_ms(&self) -> &HashMap<NodeId, u64> {
        &self.last_focus_ms
    }

    pub fn focus(&mut self, id: Option<NodeId>, now_ms: u64) -> bool {
        let next = id.filter(|id| self.records.get(id).is_some_and(|record| record.attached));
        if self.focused == next {
            return false;
        }
        self.focused = next;
        if let Some(id) = next {
            self.last_focus_ms.insert(id, now_ms);
        }
        true
    }

    pub fn focus_surface(&mut self, surface: &WlSurface, now_ms: u64) -> bool {
        self.focus(self.id_for_surface(surface), now_ms)
    }

    pub fn record(&self, id: NodeId) -> Option<&NodeRecord> {
        self.records.get(&id)
    }

    pub fn record_mut(&mut self, id: NodeId) -> Option<&mut NodeRecord> {
        self.records.get_mut(&id)
    }

    pub fn records(&self) -> impl Iterator<Item = &NodeRecord> {
        self.records.values()
    }

    pub fn clear_direct_motion(&mut self, id: NodeId) {
        self.landmark_slides.borrow_mut().remove(&id);
        self.physics_velocity.remove(&id);
        self.release_locks.remove(&id);
    }

    pub fn lock_released_window(&mut self, id: NodeId, now: Duration) {
        if self
            .records
            .get(&id)
            .is_some_and(|record| record.attached && !record.collapsed)
        {
            self.release_locks.insert(id, release_lock_deadline(now));
        }
    }

    fn expire_release_locks(&mut self, now: Duration) -> bool {
        let before = self.release_locks.len();
        self.release_locks
            .retain(|_, until| release_lock_is_active(*until, now));
        self.release_locks.len() != before
    }

    fn release_locked(&self, id: NodeId) -> bool {
        self.release_locks.contains_key(&id)
    }

    pub fn has_physics_on_output(&self, output: &str, now: Duration) -> bool {
        self.physics.enabled
            && (self.physics_velocity.keys().any(|id| {
                self.records
                    .get(id)
                    .is_some_and(|record| record.output == output)
            }) || self.release_locks.iter().any(|(id, until)| {
                release_lock_is_active(*until, now)
                    && self
                        .records
                        .get(id)
                        .is_some_and(|record| record.output == output)
            }))
    }

    pub fn collapsed_on_output(&self, output: &str) -> impl Iterator<Item = &NodeRecord> {
        self.records
            .values()
            .filter(move |record| record.collapsed && record.attached && record.output == output)
    }

    pub fn is_animating_on_output(&self, output: &str, now: Duration) -> bool {
        let node_transition = self.animations_enabled
            && self.animation.enabled
            && self.animation.duration_ms > 0
            && self.collapsed_on_output(output).any(|record| {
                now.saturating_sub(record.collapsed_at)
                    < Duration::from_millis(u64::from(self.animation.duration_ms))
            });
        let states = self.label_hover.borrow();
        let hover_transition = self.config.show_labels == halley_config::NodeDisplayPolicy::Hover
            && self.collapsed_on_output(output).any(|record| {
                let mix = states.get(&record.id).copied().unwrap_or(0.0);
                let target = if self.hovered == Some(record.id) {
                    1.0
                } else {
                    0.0
                };
                (mix - target).abs() > 0.002
            });
        let icon_transition = self
            .collapsed_on_output(output)
            .any(|record| now.saturating_sub(record.collapsed_at) < Duration::from_millis(1_220));
        let slide_transition = self.collapsed_on_output(output).any(|record| {
            self.landmark_slides
                .borrow()
                .get(&record.id)
                .is_some_and(|slide| {
                    now.saturating_sub(slide.started) < Duration::from_millis(LANDMARK_SLIDE_MS)
                })
        });
        node_transition
            || hover_transition
            || icon_transition
            || slide_transition
            || self.has_physics_on_output(output, now)
    }

    pub fn label_hover_mix(&self, id: NodeId, highlighted: bool) -> f32 {
        let mut states = self.label_hover.borrow_mut();
        let mix = states.entry(id).or_insert(0.0);
        let target = if highlighted { 1.0 } else { 0.0 };
        let rate = if highlighted { 0.06 } else { 0.10 };
        *mix += (target - *mix) * rate;
        if (*mix - target).abs() < 0.002 {
            *mix = target;
        }
        *mix
    }

    pub fn landmark_position(&self, id: NodeId, target: Vec2, now: Duration) -> Vec2 {
        let mut slides = self.landmark_slides.borrow_mut();
        let Some(slide) = slides.get(&id).copied() else {
            return target;
        };
        let t =
            now.saturating_sub(slide.started).as_secs_f32() / (LANDMARK_SLIDE_MS as f32 / 1_000.0);
        if t >= 1.0 {
            slides.remove(&id);
            return target;
        }
        let denominator = 1.0 - 6.0 * (-5.0_f32).exp();
        let damped = ((1.0 - (1.0 + 5.0 * t) * (-5.0 * t).exp()) / denominator).clamp(0.0, 1.0);
        Vec2 {
            x: slide.from.x + (slide.to.x - slide.from.x) * damped,
            y: slide.from.y + (slide.to.y - slide.from.y) * damped,
        }
    }

    fn start_landmark_slide(&self, id: NodeId, from: Vec2, to: Vec2, now: Duration) {
        if from == to || !self.animations_enabled || !self.animation.enabled {
            self.landmark_slides.borrow_mut().remove(&id);
        } else {
            self.landmark_slides.borrow_mut().insert(
                id,
                LandmarkSlide {
                    from,
                    to,
                    started: now,
                },
            );
        }
    }

    pub fn hit_test(
        &self,
        output: &Output,
        output_geometry: Rectangle<i32, Logical>,
        camera: &halley_core::camera::Camera,
        screen: Point<f64, Logical>,
    ) -> Option<NodeId> {
        self.collapsed_on_output(&output.name())
            .filter_map(|record| {
                let node = self.field.node(record.id)?;
                let position = self.landmark_position(
                    record.id,
                    node.pos,
                    crate::frame_clock::monotonic_now(),
                );
                let center = screen_from_world(position, camera, output_geometry);
                let rect = marker_rect(
                    center,
                    &node.label,
                    self.config.show_labels,
                    self.hovered == Some(record.id),
                );
                rect.to_f64().contains(screen).then_some(record.id)
            })
            .max_by_key(|id| id.as_u64())
    }

    pub fn decay_candidates(
        &mut self,
        camera_centers: &HashMap<String, Vec2>,
        focused: Option<&WlSurface>,
        protected: impl Fn(&WlSurface) -> bool,
        now_ms: u64,
    ) -> Vec<NodeId> {
        if !self.decay_config.enabled {
            self.decay.reset();
            return Vec::new();
        }
        let policy = TimedDecayPolicy {
            inside_ms: self.decay_config.inside_delay_seconds.saturating_mul(1_000),
            outside_ms: self
                .decay_config
                .outside_delay_seconds
                .saturating_mul(1_000),
        };
        let records = self.records.values().cloned().collect::<Vec<_>>();
        let mut ready = Vec::new();
        for record in records {
            if record.collapsed || !record.attached {
                self.decay.remove(record.id);
                continue;
            }
            let class = if focused == Some(&record.surface) || protected(&record.surface) {
                DecayClass::Protected
            } else {
                let configured_ring = self.focus_rings.for_output(&record.output);
                let ring = CoreFocusRing::new(
                    configured_ring.radius_x,
                    configured_ring.radius_y,
                    configured_ring.offset_x,
                    configured_ring.offset_y,
                );
                let center = camera_centers
                    .get(&record.output)
                    .copied()
                    .unwrap_or(Vec2 { x: 0.0, y: 0.0 });
                let node = self.field.node(record.id).expect("record has field node");
                if ring.outside_fraction(center, node.pos, node.intrinsic_size) >= OUTSIDE_THRESHOLD
                {
                    DecayClass::OutsideRing
                } else {
                    DecayClass::InsideRing
                }
            };
            if self
                .decay
                .update(record.id, class, now_ms, policy)
                .is_some()
            {
                ready.push(record.id);
            }
        }
        ready
    }

    pub fn set_collapsed(&mut self, id: NodeId, collapsed: bool, now_ms: u64) -> bool {
        let Some(record) = self.records.get_mut(&id) else {
            return false;
        };
        record.collapsed = collapsed;
        self.decay.remove(id);
        self.release_locks.remove(&id);
        if collapsed {
            self.field.set_state(id, NodeState::Node)
        } else {
            self.landmark_slides.borrow_mut().remove(&id);
            self.field.touch(id, now_ms)
        }
    }

    fn nearest_free_position(&self, id: NodeId, desired: Vec2, scale: f32) -> Vec2 {
        let output = self
            .record(id)
            .map(|record| record.output.as_str())
            .unwrap_or_default();
        let scale = scale.max(0.05);
        let node_spacing = (NODE_DIAMETER_PX + self.landmarks.gap_px) / scale;
        let window_clearance = (NODE_DIAMETER_PX * 0.5 + self.landmarks.gap_px) / scale;
        let occupied_nodes = self
            .records
            .values()
            .filter(|record| {
                record.id != id && record.collapsed && record.attached && record.output == output
            })
            .filter_map(|record| self.field.node(record.id).map(|node| node.pos))
            .collect::<Vec<_>>();
        let occupied_windows = self
            .records
            .values()
            .filter(|record| {
                record.id != id && !record.collapsed && record.attached && record.output == output
            })
            .map(|record| record.geometry)
            .collect::<Vec<_>>();
        nearest_free_landmark(
            desired,
            &occupied_nodes,
            &occupied_windows,
            node_spacing,
            window_clearance,
        )
    }

    pub(crate) fn nearest_free_active_rect(
        &self,
        id: NodeId,
        desired: Rectangle<i32, Logical>,
        output: &str,
        scale: f32,
    ) -> Rectangle<i32, Logical> {
        let clearance = (NODE_DIAMETER_PX * 0.5 + self.landmarks.gap_px) / scale.max(0.05);
        let nodes = self
            .records
            .values()
            .filter(|record| {
                record.id != id && record.collapsed && record.attached && record.output == output
            })
            .filter_map(|record| self.field.node(record.id).map(|node| node.pos))
            .collect::<Vec<_>>();
        nearest_free_window_rect(desired, &nodes, clearance)
    }
}

fn nearest_free_landmark(
    desired: Vec2,
    occupied_nodes: &[Vec2],
    occupied_windows: &[Rectangle<i32, Logical>],
    node_spacing: f32,
    window_clearance: f32,
) -> Vec2 {
    let free = |candidate: Vec2| {
        occupied_nodes.iter().all(|other| {
            let dx = candidate.x - other.x;
            let dy = candidate.y - other.y;
            dx * dx + dy * dy >= node_spacing * node_spacing
        }) && occupied_windows
            .iter()
            .all(|rect| !circle_intersects_rect(candidate, window_clearance, *rect))
    };
    if free(desired) {
        return desired;
    }
    for radius in 1_i32.. {
        for x in -radius..=radius {
            for y in -radius..=radius {
                if x.abs() != radius && y.abs() != radius {
                    continue;
                }
                let candidate = Vec2 {
                    x: desired.x + x as f32 * node_spacing,
                    y: desired.y + y as f32 * node_spacing,
                };
                if free(candidate) {
                    return candidate;
                }
            }
        }
    }
    unreachable!("unbounded landmark search must eventually find free space")
}

fn circle_intersects_rect(center: Vec2, radius: f32, rect: Rectangle<i32, Logical>) -> bool {
    let min_x = rect.loc.x as f32;
    let min_y = rect.loc.y as f32;
    let max_x = min_x + rect.size.w as f32;
    let max_y = min_y + rect.size.h as f32;
    let nearest_x = center.x.clamp(min_x, max_x);
    let nearest_y = center.y.clamp(min_y, max_y);
    let dx = center.x - nearest_x;
    let dy = center.y - nearest_y;
    dx * dx + dy * dy < radius * radius
}

fn nearest_free_window_rect(
    desired: Rectangle<i32, Logical>,
    nodes: &[Vec2],
    clearance: f32,
) -> Rectangle<i32, Logical> {
    let free = |candidate: Rectangle<i32, Logical>| {
        nodes
            .iter()
            .all(|node| !circle_intersects_rect(*node, clearance, candidate))
    };
    if free(desired) {
        return desired;
    }
    let step = (clearance * 2.0).round().max(1.0) as i32;
    for radius in 1_i32.. {
        for x in -radius..=radius {
            for y in -radius..=radius {
                if x.abs() != radius && y.abs() != radius {
                    continue;
                }
                let candidate = Rectangle::new(
                    desired.loc + Point::<i32, Logical>::from((x * step, y * step)),
                    desired.size,
                );
                if free(candidate) {
                    return candidate;
                }
            }
        }
    }
    unreachable!("unbounded active-window search must eventually find free space")
}

pub fn marker_rect(
    center: Point<i32, Logical>,
    _label: &str,
    _labels: halley_config::NodeDisplayPolicy,
    _hovered: bool,
) -> Rectangle<i32, Logical> {
    let diameter = NODE_DIAMETER_PX.round() as i32;
    Rectangle::new(
        (center.x - diameter / 2, center.y - diameter / 2).into(),
        (diameter, diameter).into(),
    )
}

pub fn screen_from_world(
    world: Vec2,
    camera: &halley_core::camera::Camera,
    output_geometry: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let scale = crate::camera::scale(camera);
    let global_center =
        crate::camera::global_center((camera.center.x, camera.center.y).into(), output_geometry);
    (
        output_geometry.loc.x
            + output_geometry.size.w / 2
            + ((world.x - global_center.x) * scale).round() as i32,
        output_geometry.loc.y
            + output_geometry.size.h / 2
            + ((world.y - global_center.y) * scale).round() as i32,
    )
        .into()
}

fn metadata(window: &Window) -> (String, Option<String>) {
    if let Some(toplevel) = window.toplevel() {
        return with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok())
                .map(|data| {
                    let app_id = data.app_id.clone();
                    let title = data
                        .title
                        .clone()
                        .or_else(|| app_id.clone())
                        .unwrap_or_else(|| "Untitled".to_string());
                    (title, app_id)
                })
                .unwrap_or_else(|| ("Untitled".to_string(), None))
        });
    }
    if let Some(x11) = window.x11_surface() {
        let title = x11.title();
        let app_id = (!x11.class().is_empty()).then(|| x11.class());
        return (
            if title.is_empty() {
                app_id.clone().unwrap_or_else(|| "Untitled".to_string())
            } else {
                title
            },
            app_id,
        );
    }
    ("Untitled".to_string(), None)
}

fn rect_center(rect: Rectangle<i32, Logical>) -> Vec2 {
    Vec2 {
        x: rect.loc.x as f32 + rect.size.w as f32 / 2.0,
        y: rect.loc.y as f32 + rect.size.h as f32 / 2.0,
    }
}

fn vec_size(rect: Rectangle<i32, Logical>) -> Vec2 {
    Vec2 {
        x: rect.size.w.max(1) as f32,
        y: rect.size.h.max(1) as f32,
    }
}

pub fn reconcile_landmarks<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    only_output: Option<&str>,
) {
    reconcile_landmarks_inner(session, only_output, None);
}

pub fn reconcile_landmarks_at_scale<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    output: &str,
    scale: f32,
) {
    reconcile_landmarks_inner(session, Some(output), Some(scale));
}

fn reconcile_landmarks_inner<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    only_output: Option<&str>,
    scale_override: Option<f32>,
) {
    session.nodes.sync_from_space(&session.wayland.space);
    let candidates = session
        .nodes
        .records()
        .filter(|record| {
            record.collapsed
                && record.attached
                && only_output.is_none_or(|output| record.output == output)
        })
        .filter_map(|record| {
            session
                .nodes
                .field
                .node(record.id)
                .map(|node| (record.id, record.output.clone(), node.pos))
        })
        .collect::<Vec<_>>();
    let now = crate::frame_clock::monotonic_now();
    for (id, output, current) in candidates {
        let scale = scale_override.unwrap_or_else(|| {
            session
                .cameras
                .get(&output)
                .map(crate::camera::scale)
                .unwrap_or(1.0)
        });
        let destination = session.nodes.nearest_free_position(id, current, scale);
        if destination == current {
            continue;
        }
        if let Some(node) = session.nodes.field.node_mut(id) {
            node.pos = destination;
        }
        session
            .nodes
            .start_landmark_slide(id, current, destination, now);
    }
}

fn dynamics_bodies<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
) -> Vec<dynamics::Body> {
    session.nodes.sync_from_space(&session.wayland.space);
    session
        .nodes
        .records()
        .filter(|record| {
            record.attached
                && (record.collapsed
                    || (!session.fullscreen.is_fullscreen_or_pending(&record.surface)
                        && !session.maximize.contains(&record.surface)))
        })
        .filter_map(|record| {
            let node = session.nodes.field.node(record.id)?;
            let scale = session
                .cameras
                .get(&record.output)
                .map(crate::camera::scale)
                .unwrap_or(1.0)
                .max(0.05);
            let (kind, half) = if record.collapsed {
                (
                    dynamics::BodyKind::Node,
                    Vec2 {
                        x: NODE_DIAMETER_PX * 0.5 / scale,
                        y: NODE_DIAMETER_PX * 0.5 / scale,
                    },
                )
            } else {
                (
                    dynamics::BodyKind::Window,
                    Vec2 {
                        x: record.geometry.size.w.max(1) as f32 * 0.5,
                        y: record.geometry.size.h.max(1) as f32 * 0.5,
                    },
                )
            };
            Some(dynamics::Body {
                id: record.id,
                kind,
                pos: node.pos,
                half,
                gap: session.nodes.landmarks.gap_px / scale,
                pinned: node.pinned || session.nodes.release_locked(record.id),
                output: record.output.clone(),
            })
        })
        .collect()
}

fn apply_dynamics_positions<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    positions: HashMap<NodeId, Vec2>,
    authority: Option<NodeId>,
) -> HashSet<String> {
    let changes = positions
        .into_iter()
        .filter_map(|(id, position)| {
            let record = session.nodes.record(id)?;
            let current = session.nodes.field.node(id)?.pos;
            ((position.x - current.x).abs() > 0.001 || (position.y - current.y).abs() > 0.001).then(
                || {
                    (
                        id,
                        position,
                        record.collapsed,
                        record.output.clone(),
                        record.window.clone(),
                        record.geometry.size,
                    )
                },
            )
        })
        .collect::<Vec<_>>();
    let mut outputs = HashSet::new();
    for (id, position, collapsed, output, window, size) in changes {
        outputs.insert(output);
        if let Some(node) = session.nodes.field.node_mut(id) {
            node.pos = position;
        }
        session.nodes.landmark_slides.borrow_mut().remove(&id);
        if collapsed {
            continue;
        }
        let location = Point::<i32, Logical>::from((
            (position.x - size.w as f32 * 0.5).round() as i32,
            (position.y - size.h as f32 * 0.5).round() as i32,
        ));
        // `Space::map_element` always moves an existing element to the top of
        // its z-index, even with `activate = false`. Physics must only change
        // position: otherwise a node pushing a window makes that window jump
        // layers on every solver frame.
        session.wayland.space.relocate_element(&window, location);
        if let Some(record) = session.nodes.record_mut(id) {
            record.geometry = Rectangle::new(location, size);
        }
        if window.x11_surface().is_some() {
            crate::xwayland::configure_window(&window, Rectangle::new(location, size));
        }
        if authority != Some(id) {
            // A physically displaced Wayland window needs only compositor
            // placement; its client-controlled size remains unchanged.
            crate::wayland::popup::update_reactive_for_window(
                &session.wayland,
                &session.cameras,
                &window,
            );
        }
    }
    outputs
}

fn physics_frame_delta(last: Duration, now: Duration) -> f32 {
    now.saturating_sub(last).as_secs_f32().min(1.0 / 30.0)
}

pub(crate) fn move_grabbed_body_rigid<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    desired: Vec2,
) -> bool {
    let bodies = dynamics_bodies(session);
    if !bodies.iter().any(|body| body.id == id) {
        return false;
    }
    session.nodes.physics_velocity.clear();
    let positions = dynamics::solve_static_swept(bodies, id, desired);
    let changed = !apply_dynamics_positions(session, positions, Some(id)).is_empty();
    if changed {
        session.request_redraw();
    }
    changed
}

pub(crate) fn tick_physics<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    now: Duration,
) -> bool {
    if !session.nodes.physics.enabled {
        session.nodes.physics_velocity.clear();
        session.nodes.release_locks.clear();
        session.nodes.physics_last_tick = now;
        return false;
    }
    let authority = match &session.grab {
        crate::input::grab::Grab::MoveWindow {
            id: Some(id),
            last_world,
            velocity,
            ..
        }
        | crate::input::grab::Grab::MoveNode {
            id,
            last_world,
            velocity,
            ..
        } => Some((*id, *last_world, *velocity)),
        _ => None,
    };
    let expired_lock = session.nodes.expire_release_locks(now);
    if session.nodes.physics_velocity.is_empty()
        && authority.is_none()
        && session.nodes.release_locks.is_empty()
        && !expired_lock
    {
        session.nodes.physics_last_tick = now;
        return false;
    }
    let bodies = dynamics_bodies(session);
    let live = bodies.iter().map(|body| body.id).collect::<HashSet<_>>();
    session
        .nodes
        .physics_velocity
        .retain(|id, _| live.contains(id));
    let dt = physics_frame_delta(session.nodes.physics_last_tick, now);
    session.nodes.physics_last_tick = now;
    let positions = if let Some(authority) = authority {
        dynamics::solve_physics_swept(
            bodies,
            &mut session.nodes.physics_velocity,
            authority,
            dt,
            session.nodes.physics.damping,
        )
    } else {
        if dt <= f32::EPSILON && !expired_lock {
            return !session.nodes.physics_velocity.is_empty()
                || !session.nodes.release_locks.is_empty();
        }
        dynamics::solve_physics(
            &bodies,
            &mut session.nodes.physics_velocity,
            None,
            dt,
            session.nodes.physics.damping,
        )
    };
    let _ = apply_dynamics_positions(session, positions, authority.map(|(id, _, _)| id));
    authority.is_some()
        || !session.nodes.physics_velocity.is_empty()
        || !session.nodes.release_locks.is_empty()
}

pub(crate) fn set_collapsed_output<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    output: &Output,
) {
    let changed = session
        .nodes
        .record(id)
        .is_some_and(|record| record.output != output.name());
    if !changed {
        return;
    }
    let window = session.nodes.record(id).map(|record| record.window.clone());
    if let Some(record) = session.nodes.record_mut(id) {
        record.output = output.name();
    }
    if let Some(window) = window {
        crate::wayland::set_window_output(&window, output);
    }
    session.nodes.clear_direct_motion(id);
}

pub fn toggle_focused_on_output<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    output: Option<&str>,
    serial: smithay::utils::Serial,
) {
    let id = match output {
        Some(output) => session.nodes.focused_on_output(output),
        None => session.nodes.focused(),
    };
    let Some(id) = id else {
        return;
    };
    if session
        .nodes
        .record(id)
        .is_some_and(|record| record.collapsed)
    {
        let _ = restore(session, id, serial);
    } else {
        let _ = collapse(session, id, serial);
    }
}

pub fn close_focused_on_output<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    output: Option<&str>,
) {
    let id = match output {
        Some(output) => session.nodes.focused_on_output(output),
        None => session.nodes.focused(),
    };
    let Some(record) = id.and_then(|id| session.nodes.record(id)) else {
        return;
    };
    if let Some(toplevel) = record.window.toplevel() {
        toplevel.send_close();
    } else {
        crate::xwayland::close_window(&record.window);
    }
}

pub fn collapse<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    serial: smithay::utils::Serial,
) -> bool {
    let Some(record) = session.nodes.record(id).cloned() else {
        return false;
    };
    if record.collapsed
        || !record.attached
        || session.fullscreen.is_fullscreen_or_pending(&record.surface)
    {
        return false;
    }
    let Some(geometry) = session.wayland.space.element_geometry(&record.window) else {
        return false;
    };
    let Some(stack_index) = session
        .wayland
        .space
        .elements()
        .position(|candidate| candidate == &record.window)
    else {
        return false;
    };
    let client_was_focused = session.wayland.focused_window.as_ref() == Some(&record.surface);
    let logical_focus =
        logical_focus_after_collapse(session.nodes.focused(), id, client_was_focused);

    let _ = crate::session::closing::capture_window(session, &record.window);
    session.maximize.remove(&record.surface);
    crate::session::cancel_grab_for_surface(session, &record.surface);
    if client_was_focused {
        // A collapsed surface must not keep receiving keyboard input, but the
        // node remains Halley's logical command/focus target.
        crate::window::clear_focus(&mut session.wayland);
    }
    session.wayland.space.unmap_elem(&record.window);
    if record.window.toplevel().is_some() {
        session
            .wayland
            .collapsed
            .insert(record.surface.clone(), record.window.clone());
    } else if let Some(surface) = record.window.x11_surface()
        && let Err(err) = surface.set_hidden(true)
    {
        eventline::warn!("xwayland: failed to mark collapsed window hidden: {err}");
    }
    let collapse_origin = rect_center(geometry);
    let scale = session
        .cameras
        .get(&record.output)
        .map(crate::camera::scale)
        .unwrap_or(1.0);
    let node_position = session
        .nodes
        .nearest_free_position(id, collapse_origin, scale);
    if let Some(node) = session.nodes.field.node_mut(id) {
        node.pos = node_position;
        node.intrinsic_size = vec_size(geometry);
    }
    if let Some(record) = session.nodes.record_mut(id) {
        record.geometry = geometry;
        record.collapsed_stack_index = Some(stack_index);
    }
    let now_ms = session.start_time.elapsed().as_millis() as u64;
    if !session.nodes.set_collapsed(id, true, now_ms) {
        return false;
    }
    if let Some(record) = session.nodes.record_mut(id) {
        record.collapsed_at = crate::frame_clock::monotonic_now();
    }
    session.nodes.start_landmark_slide(
        id,
        collapse_origin,
        node_position,
        crate::frame_clock::monotonic_now(),
    );
    session
        .window_close_animations
        .retarget_pending_to_node(&record.surface, node_position);
    let _ = crate::session::closing::start(session, &record.surface);
    session.nodes.focus(logical_focus, now_ms);
    crate::session::sync_keyboard_focus(session, serial);
    crate::session::reconcile_pointer_constraints(session);
    session.request_redraw();
    true
}

pub fn restore<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    serial: smithay::utils::Serial,
) -> bool {
    restore_with_centering(session, id, serial, None)
}

pub fn restore_for_close<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    serial: smithay::utils::Serial,
) -> bool {
    restore_with_centering(
        session,
        id,
        serial,
        Some(halley_config::RestoreCentering::Never),
    )
}

fn restore_with_centering<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    serial: smithay::utils::Serial,
    centering: Option<halley_config::RestoreCentering>,
) -> bool {
    let Some(record) = session.nodes.record(id).cloned() else {
        return false;
    };
    if !record.collapsed || !record.attached {
        return false;
    }
    let Some(node) = session.nodes.field.node(id).cloned() else {
        return false;
    };
    let size = record.geometry.size;
    let location = Point::<i32, Logical>::from((
        (node.pos.x - size.w as f32 / 2.0).round() as i32,
        (node.pos.y - size.h as f32 / 2.0).round() as i32,
    ));
    session
        .wayland
        .space
        .map_element(record.window.clone(), location, true);
    if let Some(surface) = record.window.x11_surface()
        && let Err(err) = surface.set_hidden(false)
    {
        eventline::warn!("xwayland: failed to clear restored window hidden state: {err}");
    }
    session.wayland.collapsed.remove(&record.surface);
    let now = crate::frame_clock::monotonic_now();
    let now_ms = session.start_time.elapsed().as_millis() as u64;
    let _ = session.nodes.set_collapsed(id, false, now_ms);
    if let Some(record) = session.nodes.record_mut(id) {
        record.collapsed_stack_index = None;
    }
    reconcile_landmarks(session, Some(&record.output));
    crate::session::closing::mapped(session, &record.surface);
    crate::window::focus_and_raise(&mut session.wayland, &record.window);
    session.xwayland.raise_window(&record.window);

    let output = {
        session
            .wayland
            .space
            .outputs()
            .find(|output| output.name() == record.output)
            .cloned()
    };
    if let Some(output) = output {
        let _ = crate::session::opening::start(session, record.surface.clone(), &output, now);
        let should_center = match centering.unwrap_or(session.nodes.config.restore_centering) {
            halley_config::RestoreCentering::Never => false,
            halley_config::RestoreCentering::Always => true,
            halley_config::RestoreCentering::IfOffscreen => {
                let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
                    return true;
                };
                let Some(camera) = session.cameras.get(&record.output) else {
                    return true;
                };
                !output_geometry.contains(screen_from_world(node.pos, camera, output_geometry))
            }
        };
        if should_center
            && let Some(output_geometry) = session.wayland.space.output_geometry(&output)
            && let Some(camera) = session.cameras.get_mut(&record.output)
        {
            camera.pan_vel = Vec2 { x: 0.0, y: 0.0 };
            camera.target_center = Vec2 {
                x: node.pos.x - output_geometry.loc.x as f32,
                y: node.pos.y - output_geometry.loc.y as f32,
            };
        }
    }
    crate::session::sync_keyboard_focus(session, serial);
    crate::session::reconcile_pointer_constraints(session);
    session.request_redraw();
    true
}

pub fn pan_after_close_restore<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    policy: halley_config::CloseRestorePan,
) {
    if policy == halley_config::CloseRestorePan::Never {
        return;
    }
    let Some(record) = session.nodes.record(id).cloned() else {
        return;
    };
    if session.fullscreen.is_fullscreen_or_pending(&record.surface)
        || session.maximize.contains(&record.surface)
    {
        return;
    }
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == record.output)
        .cloned()
    else {
        return;
    };
    let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
        return;
    };
    let Some(view) = session.cameras.view(&record.output) else {
        return;
    };
    let geometry = session
        .wayland
        .space
        .element_geometry(&record.window)
        .unwrap_or(record.geometry);
    let viewport = crate::camera::world_viewport(view, output_geometry);
    let delta = match policy {
        halley_config::CloseRestorePan::Never => return,
        halley_config::CloseRestorePan::IfOffscreen => {
            if viewport.intersection(geometry).is_some() {
                return;
            }
            minimal_reveal_delta(viewport, geometry, 24)
        }
        halley_config::CloseRestorePan::Always => Vec2 {
            x: geometry.loc.x as f32 + geometry.size.w as f32 * 0.5
                - (viewport.loc.x as f32 + viewport.size.w as f32 * 0.5),
            y: geometry.loc.y as f32 + geometry.size.h as f32 * 0.5
                - (viewport.loc.y as f32 + viewport.size.h as f32 * 0.5),
        },
    };
    if let Some(camera) = session.cameras.get_mut(&record.output) {
        camera.pan_vel = Vec2 { x: 0.0, y: 0.0 };
        camera.target_center = Vec2 {
            x: camera.center.x + delta.x,
            y: camera.center.y + delta.y,
        };
    }
}

/// Activate a Bearings target in one operation. Collapsed nodes follow the
/// configured restore-centering policy; live windows are focused immediately
/// and the camera only moves far enough to reveal their current bounds.
pub fn focus_or_restore_from_bearing<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    serial: smithay::utils::Serial,
) -> bool {
    let Some(record) = session.nodes.record(id).cloned() else {
        return false;
    };
    if record.collapsed {
        return restore(session, id, serial);
    }
    if !record.attached {
        return false;
    }

    crate::session::focus_window(session, &record.window, serial);
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == record.output)
        .cloned()
    else {
        session.request_redraw();
        return true;
    };
    let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
        session.request_redraw();
        return true;
    };
    let Some(view) = session.cameras.view(&record.output) else {
        session.request_redraw();
        return true;
    };
    let geometry = session
        .wayland
        .space
        .element_geometry(&record.window)
        .unwrap_or(record.geometry);
    let delta = minimal_reveal_delta(
        crate::camera::world_viewport(view, output_geometry),
        geometry,
        24,
    );
    if (delta.x != 0.0 || delta.y != 0.0)
        && let Some(camera) = session.cameras.get_mut(&record.output)
    {
        camera.pan_vel = Vec2 { x: 0.0, y: 0.0 };
        camera.target_center = Vec2 {
            x: camera.center.x + delta.x,
            y: camera.center.y + delta.y,
        };
    }
    session.request_redraw();
    true
}

/// Makes an Alt+Tab target visible without adding a second animation track.
/// The camera snaps only by the minimum reveal delta; the focus-cycle overlay
/// already owns the visible close transition.
pub fn reveal_for_focus_cycle<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
) {
    let Some(record) = session.nodes.record(id).cloned() else {
        return;
    };
    if session.fullscreen.is_fullscreen_or_pending(&record.surface)
        || session.maximize.contains(&record.surface)
    {
        return;
    }
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == record.output)
        .cloned()
    else {
        return;
    };
    let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
        return;
    };
    let Some(view) = session.cameras.view(&record.output) else {
        return;
    };
    let geometry = session
        .wayland
        .space
        .element_geometry(&record.window)
        .unwrap_or(record.geometry);
    let delta = minimal_reveal_delta(
        crate::camera::world_viewport(view, output_geometry),
        geometry,
        24,
    );
    if delta.x == 0.0 && delta.y == 0.0 {
        return;
    }
    if let Some(camera) = session.cameras.get_mut(&record.output) {
        camera.center = Vec2 {
            x: camera.center.x + delta.x,
            y: camera.center.y + delta.y,
        };
        camera.target_center = camera.center;
        camera.pan_vel = Vec2 { x: 0.0, y: 0.0 };
    }
}

fn minimal_reveal_delta(
    viewport: Rectangle<i32, Logical>,
    target: Rectangle<i32, Logical>,
    margin: i32,
) -> Vec2 {
    fn axis_delta(
        view_start: i32,
        view_extent: i32,
        target_start: i32,
        target_extent: i32,
        margin: i32,
    ) -> f32 {
        let available = (view_extent - margin.saturating_mul(2)).max(1);
        if target_extent > available {
            return (target_start as f32 + target_extent as f32 * 0.5)
                - (view_start as f32 + view_extent as f32 * 0.5);
        }
        let minimum = view_start + margin;
        let maximum = view_start + view_extent - margin;
        if target_start < minimum {
            (target_start - minimum) as f32
        } else if target_start + target_extent > maximum {
            (target_start + target_extent - maximum) as f32
        } else {
            0.0
        }
    }

    Vec2 {
        x: axis_delta(
            viewport.loc.x,
            viewport.size.w,
            target.loc.x,
            target.size.w,
            margin,
        ),
        y: axis_delta(
            viewport.loc.y,
            viewport.size.h,
            target.loc.y,
            target.size.h,
            margin,
        ),
    }
}

pub fn tick_decay<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
) -> bool {
    session.nodes.sync_from_space(&session.wayland.space);
    let mut centers = HashMap::new();
    for record in session.nodes.records() {
        if centers.contains_key(&record.output) {
            continue;
        }
        let Some(output) = session
            .wayland
            .space
            .outputs()
            .find(|output| output.name() == record.output)
        else {
            continue;
        };
        let Some(output_geometry) = session.wayland.space.output_geometry(output) else {
            continue;
        };
        let Some(view) = session.cameras.view(&record.output) else {
            continue;
        };
        let global = crate::camera::global_center(view.center, output_geometry);
        centers.insert(
            record.output.clone(),
            Vec2 {
                x: global.x,
                y: global.y,
            },
        );
    }
    let focused = session.wayland.focused_window.clone();
    let protected = session
        .nodes
        .records()
        .filter(|record| {
            session.fullscreen.is_fullscreen_or_pending(&record.surface)
                || session.maximize.contains(&record.surface)
                || crate::input::grab::belongs_to_surface(&session.grab, &record.surface)
        })
        .map(|record| record.surface.clone())
        .collect::<Vec<_>>();
    let now_ms = session.start_time.elapsed().as_millis() as u64;
    let ready = session.nodes.decay_candidates(
        &centers,
        focused.as_ref(),
        |surface| protected.iter().any(|candidate| candidate == surface),
        now_ms,
    );
    let mut changed = false;
    for id in ready {
        changed |= collapse(session, id, smithay::utils::SERIAL_COUNTER.next_serial());
    }
    changed
}

pub fn handle_request<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    request: halley_ipc::NodeRequest,
) -> halley_ipc::Response {
    session.nodes.sync_from_space(&session.wayland.space);
    match request {
        halley_ipc::NodeRequest::List { output } => {
            let outputs = match requested_outputs(session, output.as_deref()) {
                Ok(outputs) => outputs,
                Err(error) => return halley_ipc::Response::Error(error),
            };
            halley_ipc::Response::NodeList(halley_ipc::NodeListResponse {
                outputs: outputs
                    .into_iter()
                    .map(|output| {
                        let mut ids = session
                            .nodes
                            .records()
                            .filter(|record| record.output == output)
                            .map(|record| record.id)
                            .collect::<Vec<_>>();
                        ids.sort_by_key(|id| id.as_u64());
                        halley_ipc::NodeOutputGroup {
                            output,
                            nodes: ids
                                .into_iter()
                                .filter_map(|id| node_info(session, id))
                                .collect(),
                        }
                    })
                    .collect(),
            })
        }
        halley_ipc::NodeRequest::Info { selector, output } => {
            match resolve(session, selector.as_ref(), output.as_deref()) {
                Ok(id) => node_info(session, id)
                    .map(halley_ipc::Response::NodeInfo)
                    .unwrap_or_else(|| halley_ipc::Response::Error("node disappeared".to_string())),
                Err(error) => halley_ipc::Response::Error(error),
            }
        }
        halley_ipc::NodeRequest::Focus { selector, output } => {
            let id = match resolve(session, selector.as_ref(), output.as_deref()) {
                Ok(id) => id,
                Err(error) => return halley_ipc::Response::Error(error),
            };
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            let focused = if session
                .nodes
                .record(id)
                .is_some_and(|record| record.collapsed)
            {
                restore(session, id, serial)
            } else if let Some(record) = session.nodes.record(id).cloned() {
                crate::session::focus_window(session, &record.window, serial);
                session.request_redraw();
                true
            } else {
                false
            };
            if focused {
                halley_ipc::Response::Ack
            } else {
                halley_ipc::Response::Error(format!("failed to focus node {id}"))
            }
        }
        halley_ipc::NodeRequest::Move {
            direction,
            selector,
            output,
        } => {
            let id = match resolve(session, selector.as_ref(), output.as_deref()) {
                Ok(id) => id,
                Err(error) => return halley_ipc::Response::Error(error),
            };
            if move_node(session, id, direction) {
                halley_ipc::Response::Ack
            } else {
                halley_ipc::Response::Error(format!("failed to move node {id}"))
            }
        }
        halley_ipc::NodeRequest::Close { selector, output } => {
            let id = match resolve(session, selector.as_ref(), output.as_deref()) {
                Ok(id) => id,
                Err(error) => return halley_ipc::Response::Error(error),
            };
            let Some(record) = session.nodes.record(id) else {
                return halley_ipc::Response::Error(format!("node {id} disappeared"));
            };
            if let Some(toplevel) = record.window.toplevel() {
                toplevel.send_close();
            } else {
                crate::xwayland::close_window(&record.window);
            }
            halley_ipc::Response::Ack
        }
        halley_ipc::NodeRequest::Collapse { selector, output } => {
            change_node_state(session, selector.as_ref(), output.as_deref(), Some(true))
        }
        halley_ipc::NodeRequest::Restore { selector, output } => {
            change_node_state(session, selector.as_ref(), output.as_deref(), Some(false))
        }
        halley_ipc::NodeRequest::Toggle { selector, output } => {
            change_node_state(session, selector.as_ref(), output.as_deref(), None)
        }
    }
}

fn change_node_state<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    selector: Option<&halley_ipc::NodeSelector>,
    output: Option<&str>,
    collapsed: Option<bool>,
) -> halley_ipc::Response {
    let id = match resolve(session, selector, output) {
        Ok(id) => id,
        Err(error) => return halley_ipc::Response::Error(error),
    };
    let Some(current) = session.nodes.record(id).map(|record| record.collapsed) else {
        return halley_ipc::Response::Error(format!("node {id} disappeared"));
    };
    let desired = collapsed.unwrap_or(!current);
    if desired == current {
        return halley_ipc::Response::Ack;
    }
    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
    let changed = if desired {
        collapse(session, id, serial)
    } else {
        restore(session, id, serial)
    };
    if changed {
        halley_ipc::Response::Ack
    } else {
        let action = if desired { "collapse" } else { "restore" };
        halley_ipc::Response::Error(format!("failed to {action} node {id}"))
    }
}

fn requested_outputs<D: crate::session::SessionDriver>(
    session: &crate::session::Session<D>,
    requested: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut outputs = session
        .wayland
        .space
        .outputs()
        .map(|output| output.name())
        .collect::<Vec<_>>();
    outputs.sort();
    if let Some(requested) = requested {
        if outputs.iter().any(|output| output == requested) {
            Ok(vec![requested.to_string()])
        } else {
            Err(format!("unknown output {requested:?}"))
        }
    } else {
        Ok(outputs)
    }
}

fn resolve<D: crate::session::SessionDriver>(
    session: &crate::session::Session<D>,
    selector: Option<&halley_ipc::NodeSelector>,
    output: Option<&str>,
) -> Result<NodeId, String> {
    if let Some(output) = output {
        requested_outputs(session, Some(output))?;
    }
    let on_output = |record: &&NodeRecord| output.is_none_or(|name| record.output == name);
    let records = session
        .nodes
        .records()
        .filter(on_output)
        .collect::<Vec<_>>();
    let direct = match selector {
        None | Some(halley_ipc::NodeSelector::Focused) => session.nodes.focused().filter(|id| {
            session
                .nodes
                .record(*id)
                .is_some_and(|record| output.is_none_or(|name| record.output == name))
        }),
        Some(halley_ipc::NodeSelector::Latest) => records
            .iter()
            .map(|record| record.id)
            .max_by_key(|id| id.as_u64()),
        Some(halley_ipc::NodeSelector::Id(raw)) => records
            .iter()
            .find(|record| record.id.as_u64() == *raw)
            .map(|record| record.id),
        Some(halley_ipc::NodeSelector::Title(text)) => {
            return unique_match(
                records
                    .iter()
                    .filter(|record| contains_case_insensitive(&record.title, text))
                    .map(|record| record.id)
                    .collect(),
                &format!("title:{text}"),
            );
        }
        Some(halley_ipc::NodeSelector::App(text)) => {
            return unique_match(
                records
                    .iter()
                    .filter(|record| {
                        record
                            .app_id
                            .as_deref()
                            .is_some_and(|app| contains_case_insensitive(app, text))
                    })
                    .map(|record| record.id)
                    .collect(),
                &format!("app:{text}"),
            );
        }
    };
    direct
        .or_else(|| {
            (selector.is_none()).then(|| {
                records
                    .iter()
                    .map(|record| record.id)
                    .max_by_key(|id| id.as_u64())
            })?
        })
        .ok_or_else(|| "no node matched the selector".to_string())
}

fn unique_match(ids: Vec<NodeId>, label: &str) -> Result<NodeId, String> {
    match ids.as_slice() {
        [id] => Ok(*id),
        [] => Err(format!("no node matched selector {label}")),
        _ => Err(format!("selector {label} matched multiple nodes")),
    }
}

fn contains_case_insensitive(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.to_lowercase())
}

fn move_node<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    id: NodeId,
    direction: halley_ipc::NodeMoveDirection,
) -> bool {
    let Some(record) = session.nodes.record(id).cloned() else {
        return false;
    };
    let (dx, dy) = match direction {
        halley_ipc::NodeMoveDirection::Left => (-80, 0),
        halley_ipc::NodeMoveDirection::Right => (80, 0),
        halley_ipc::NodeMoveDirection::Up => (0, -80),
        halley_ipc::NodeMoveDirection::Down => (0, 80),
    };
    if record.collapsed {
        let Some(current) = session.nodes.field.node(id).map(|node| node.pos) else {
            return false;
        };
        let desired = Vec2 {
            x: current.x + dx as f32,
            y: current.y + dy as f32,
        };
        let scale = session
            .cameras
            .get(&record.output)
            .map(crate::camera::scale)
            .unwrap_or(1.0);
        let destination = session.nodes.nearest_free_position(id, desired, scale);
        if let Some(node) = session.nodes.field.node_mut(id) {
            node.pos = destination;
        }
        session.nodes.start_landmark_slide(
            id,
            current,
            destination,
            crate::frame_clock::monotonic_now(),
        );
    } else {
        let Some(location) = session.wayland.space.element_location(&record.window) else {
            return false;
        };
        let desired = location + Point::<i32, Logical>::from((dx, dy));
        let scale = session
            .cameras
            .get(&record.output)
            .map(crate::camera::scale)
            .unwrap_or(1.0);
        let next = session
            .nodes
            .nearest_free_active_rect(
                id,
                Rectangle::new(desired, record.geometry.size),
                &record.output,
                scale,
            )
            .loc;
        session.wayland.space.relocate_element(&record.window, next);
        if record.window.x11_surface().is_some() {
            crate::xwayland::configure_window(
                &record.window,
                Rectangle::new(next, record.geometry.size),
            );
        }
    }
    session.request_redraw();
    true
}

fn node_info<D: crate::session::SessionDriver>(
    session: &crate::session::Session<D>,
    id: NodeId,
) -> Option<halley_ipc::NodeInfo> {
    let record = session.nodes.record(id)?;
    let node = session.nodes.field.node(id)?;
    let latest = session
        .nodes
        .records()
        .filter(|candidate| candidate.output == record.output)
        .map(|candidate| candidate.id)
        .max_by_key(|candidate| candidate.as_u64())
        == Some(id);
    let (role, family, modal, parent) = relation_metadata(session, record);
    Some(halley_ipc::NodeInfo {
        id: id.as_u64(),
        title: record.title.clone(),
        app_id: record.app_id.clone(),
        output: Some(record.output.clone()),
        kind: halley_ipc::NodeKind::Surface,
        state: if record.collapsed {
            halley_ipc::NodeState::Node
        } else {
            halley_ipc::NodeState::Active
        },
        visible: record.attached,
        focused: session.nodes.focused() == Some(id),
        latest,
        pinned: false,
        role,
        protocol_family: family,
        modal,
        parent: parent.clone(),
        transient_for: parent,
        child_popup_count: PopupManager::popups_for_surface(&record.surface).count(),
        pos_x: node.pos.x,
        pos_y: node.pos.y,
        width: node.intrinsic_size.x,
        height: node.intrinsic_size.y,
    })
}

fn relation_metadata<D: crate::session::SessionDriver>(
    session: &crate::session::Session<D>,
    record: &NodeRecord,
) -> (
    halley_ipc::NodeRole,
    halley_ipc::NodeProtocolFamily,
    bool,
    Option<halley_ipc::NodeRelationInfo>,
) {
    if record.window.x11_surface().is_some() {
        return (
            halley_ipc::NodeRole::NormalToplevel,
            halley_ipc::NodeProtocolFamily::Xwayland,
            false,
            None,
        );
    }
    let (parent, modal) = with_states(&record.surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .map(|data| {
                (
                    data.parent.clone(),
                    data.dialog_hint == ToplevelDialogHint::Modal,
                )
            })
            .unwrap_or((None, false))
    });
    let relation = parent.map(|parent| halley_ipc::NodeRelationInfo {
        node_id: session
            .nodes
            .id_for_surface(&crate::wayland::compositor::root_surface(&parent))
            .map(NodeId::as_u64),
    });
    (
        if relation.is_some() || modal {
            halley_ipc::NodeRole::Dialog
        } else {
            halley_ipc::NodeRole::NormalToplevel
        },
        halley_ipc::NodeProtocolFamily::XdgToplevel,
        modal,
        relation,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        logical_focus_after_collapse, minimal_reveal_delta, nearest_free_landmark,
        nearest_free_window_rect, physics_frame_delta, release_lock_deadline,
        release_lock_is_active,
    };
    use halley_core::field::{NodeId, Vec2};
    use smithay::utils::{Logical, Rectangle};
    use std::time::Duration;

    #[test]
    fn physics_frame_delta_never_invents_time_for_fast_input() {
        let last = Duration::from_secs(10);
        assert!(
            (physics_frame_delta(last, last + Duration::from_millis(1)) - 0.001).abs()
                < f32::EPSILON
        );
        assert_eq!(physics_frame_delta(last, last), 0.0);
        assert_eq!(
            physics_frame_delta(last, last + Duration::from_secs(1)),
            1.0 / 30.0
        );
    }

    #[test]
    fn released_window_lock_expires_at_old_halley_deadline() {
        let now = Duration::from_secs(10);
        let until = release_lock_deadline(now);
        assert_eq!(until, now + Duration::from_millis(350));
        assert!(release_lock_is_active(
            until,
            now + Duration::from_millis(349)
        ));
        assert!(!release_lock_is_active(until, until));
    }

    #[test]
    fn collapsing_focused_window_preserves_its_logical_node_focus() {
        let collapsed = NodeId::new(7);
        let other = NodeId::new(9);
        assert_eq!(
            logical_focus_after_collapse(Some(other), collapsed, true),
            Some(collapsed)
        );
        assert_eq!(
            logical_focus_after_collapse(Some(collapsed), collapsed, false),
            Some(collapsed)
        );
    }

    #[test]
    fn collapsing_unfocused_window_does_not_steal_focus() {
        let collapsed = NodeId::new(7);
        let focused = NodeId::new(9);
        assert_eq!(
            logical_focus_after_collapse(Some(focused), collapsed, false),
            Some(focused)
        );
        assert_eq!(logical_focus_after_collapse(None, collapsed, false), None);
    }

    #[test]
    fn bearings_only_pan_enough_to_reveal_a_live_window() {
        let viewport = Rectangle::<i32, Logical>::new((0, 0).into(), (1_000, 700).into());
        assert_eq!(
            minimal_reveal_delta(
                viewport,
                Rectangle::new((1_050, 200).into(), (300, 200).into()),
                24,
            ),
            Vec2 { x: 374.0, y: 0.0 }
        );
        assert_eq!(
            minimal_reveal_delta(
                viewport,
                Rectangle::new((100, 100).into(), (300, 200).into()),
                24,
            ),
            Vec2 { x: 0.0, y: 0.0 }
        );
    }

    #[test]
    fn placement_keeps_the_requested_position_when_it_is_free() {
        let desired = Vec2 { x: 10.0, y: 20.0 };
        assert_eq!(
            nearest_free_landmark(desired, &[], &[], 71.0, 45.5),
            desired
        );
    }

    #[test]
    fn placement_moves_an_overlapping_node_to_the_nearest_grid_ring() {
        let desired = Vec2 { x: 10.0, y: 20.0 };
        let spacing = 71.0;
        let placed = nearest_free_landmark(desired, &[desired], &[], spacing, 45.5);
        let dx = placed.x - desired.x;
        let dy = placed.y - desired.y;
        assert!(dx * dx + dy * dy >= spacing * spacing);
        assert!(dx.abs() <= spacing && dy.abs() <= spacing);
    }

    #[test]
    fn active_window_moves_away_from_a_landmark() {
        let desired = Rectangle::<i32, Logical>::new((0, 0).into(), (200, 120).into());
        let placed = nearest_free_window_rect(desired, &[Vec2 { x: 100.0, y: 60.0 }], 45.5);
        assert_ne!(placed, desired);
    }

    #[test]
    fn landmark_placement_treats_active_window_as_a_blocker() {
        let desired = Vec2 { x: 100.0, y: 60.0 };
        let window = Rectangle::<i32, Logical>::new((0, 0).into(), (200, 120).into());
        assert_ne!(
            nearest_free_landmark(desired, &[], &[window], 71.0, 45.5),
            desired
        );
    }
}
