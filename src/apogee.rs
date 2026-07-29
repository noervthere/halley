use std::collections::HashMap;
use std::time::Duration;

use halley_core::field::NodeId;
use smithay::desktop::{Space, Window};
use smithay::utils::{Logical, Point, Rectangle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub struct Tile {
    pub id: NodeId,
    pub output: String,
    pub target: Rectangle<i32, Logical>,
}

#[derive(Clone, Copy, Debug)]
struct Settle {
    from: f32,
    to: f32,
    started_at: Duration,
    duration: Duration,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub tiles: Vec<Tile>,
    pub hovered: Option<NodeId>,
    pub selected: Option<NodeId>,
    settle: Option<Settle>,
    manual_progress: Option<f32>,
    pending_activation: Option<NodeId>,
}

impl Session {
    pub fn progress(&self, now: Duration) -> f32 {
        if let Some(progress) = self.manual_progress {
            return progress.clamp(0.0, 1.0);
        }
        let Some(settle) = self.settle else {
            return 1.0;
        };
        if settle.duration.is_zero() {
            return settle.to;
        }
        let t = (now.saturating_sub(settle.started_at).as_secs_f32()
            / settle.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        let eased = ease_in_out_cubic(t);
        settle.from + (settle.to - settle.from) * eased
    }

    pub fn is_closing(&self) -> bool {
        self.settle.is_some_and(|settle| settle.to == 0.0)
    }

    pub fn tile(&self, id: NodeId) -> Option<&Tile> {
        self.tiles.iter().find(|tile| tile.id == id)
    }

    pub fn hit_test(&self, position: Point<f64, Logical>) -> Option<NodeId> {
        self.tiles
            .iter()
            .find(|tile| tile.target.to_f64().contains(position))
            .map(|tile| tile.id)
    }
}

#[derive(Default)]
pub struct ApogeeState {
    session: Option<Session>,
}

impl ApogeeState {
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    pub fn is_interactive(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.manual_progress.is_some())
    }

    pub fn open(
        &mut self,
        space: &Space<Window>,
        nodes: &crate::nodes::NodesState,
        config: halley_config::Apogee,
        now: Duration,
    ) -> bool {
        if !config.enabled || self.session.is_some() {
            return false;
        }
        let tiles = build_layout(space, nodes, config);
        if tiles.is_empty() {
            return false;
        }
        let first = tiles.first().map(|tile| tile.id);
        self.session = Some(Session {
            tiles,
            hovered: None,
            selected: first,
            settle: Some(Settle {
                from: 0.0,
                to: 1.0,
                started_at: now,
                duration: Duration::from_millis(config.transition_ms),
            }),
            manual_progress: None,
            pending_activation: None,
        });
        true
    }

    pub fn begin_interactive(
        &mut self,
        space: &Space<Window>,
        nodes: &crate::nodes::NodesState,
        config: halley_config::Apogee,
    ) -> bool {
        if !config.enabled {
            return false;
        }
        if self.session.is_none() {
            let tiles = build_layout(space, nodes, config);
            if tiles.is_empty() {
                return false;
            }
            let first = tiles.first().map(|tile| tile.id);
            self.session = Some(Session {
                tiles,
                hovered: None,
                selected: first,
                settle: None,
                manual_progress: Some(0.0),
                pending_activation: None,
            });
        }
        self.session.as_mut().is_some_and(|session| {
            session.settle = None;
            session.manual_progress = Some(session.manual_progress.unwrap_or(0.0));
            true
        })
    }

    pub fn set_interactive_progress(&mut self, progress: f32) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let next = progress.clamp(0.0, 1.0);
        let changed = session.manual_progress != Some(next);
        session.manual_progress = Some(next);
        changed
    }

    pub fn finish_interactive(
        &mut self,
        commit: bool,
        config: halley_config::Apogee,
        now: Duration,
    ) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let from = session.manual_progress.take().unwrap_or(0.0);
        let to = if commit { 1.0 } else { 0.0 };
        session.settle = Some(Settle {
            from,
            to,
            started_at: now,
            duration: proportional_duration(config.transition_ms, (to - from).abs()),
        });
        true
    }

    pub fn close(
        &mut self,
        activation: Option<NodeId>,
        config: halley_config::Apogee,
        now: Duration,
    ) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if session.is_closing() {
            return false;
        }
        let from = session.progress(now);
        session.manual_progress = None;
        session.pending_activation = activation;
        session.settle = Some(Settle {
            from,
            to: 0.0,
            started_at: now,
            duration: proportional_duration(config.transition_ms, from),
        });
        true
    }

    pub fn hover(&mut self, position: Point<f64, Logical>) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let hovered = session.hit_test(position);
        let changed = session.hovered != hovered;
        session.hovered = hovered;
        if let Some(id) = hovered {
            session.selected = Some(id);
        }
        changed
    }

    pub fn activate_at(
        &mut self,
        position: Point<f64, Logical>,
        config: halley_config::Apogee,
        now: Duration,
    ) -> bool {
        let target = self
            .session
            .as_ref()
            .and_then(|session| session.hit_test(position));
        self.close(target, config, now)
    }

    pub fn move_selection(&mut self, direction: Direction) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let Some(current) = session
            .selected
            .and_then(|id| session.tile(id))
            .or_else(|| session.tiles.first())
            .cloned()
        else {
            return false;
        };
        let center = rect_center(current.target);
        let next = session
            .tiles
            .iter()
            .filter(|tile| tile.id != current.id)
            .filter_map(|tile| {
                let candidate = rect_center(tile.target);
                let dx = candidate.x - center.x;
                let dy = candidate.y - center.y;
                let eligible = match direction {
                    Direction::Left => dx < 0.0,
                    Direction::Right => dx > 0.0,
                    Direction::Up => dy < 0.0,
                    Direction::Down => dy > 0.0,
                };
                if !eligible {
                    return None;
                }
                let (primary, secondary) = match direction {
                    Direction::Left | Direction::Right => (dx.abs(), dy.abs()),
                    Direction::Up | Direction::Down => (dy.abs(), dx.abs()),
                };
                Some((primary + secondary * 0.35, tile.id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        let Some(next) = next else {
            return false;
        };
        session.selected = Some(next);
        session.hovered = None;
        true
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.session.as_ref().and_then(|session| session.selected)
    }

    pub fn tick(&mut self, now: Duration) -> Tick {
        let Some(session) = self.session.as_ref() else {
            return Tick::Idle;
        };
        let progress = session.progress(now);
        let settling = session
            .settle
            .is_some_and(|settle| now.saturating_sub(settle.started_at) < settle.duration);
        if session.is_closing() && !settling && progress <= f32::EPSILON {
            let activation = session.pending_activation;
            self.session = None;
            return Tick::Closed(activation);
        }
        Tick::Active {
            animating: settling,
        }
    }
}

pub enum Tick {
    Idle,
    Active { animating: bool },
    Closed(Option<NodeId>),
}

fn build_layout(
    space: &Space<Window>,
    nodes: &crate::nodes::NodesState,
    config: halley_config::Apogee,
) -> Vec<Tile> {
    let mut by_output = HashMap::<String, Vec<NodeId>>::new();
    for record in nodes.records().filter(|record| record.attached) {
        by_output
            .entry(record.output.clone())
            .or_default()
            .push(record.id);
    }
    for ids in by_output.values_mut() {
        ids.sort_by_key(|id| {
            (
                std::cmp::Reverse(nodes.last_focus_ms().get(id).copied().unwrap_or(0)),
                std::cmp::Reverse(id.as_u64()),
            )
        });
    }

    let mut tiles = Vec::new();
    for output in space.outputs() {
        let Some(output_rect) = space.output_geometry(output) else {
            continue;
        };
        let Some(ids) = by_output.get(&output.name()) else {
            continue;
        };
        tiles.extend(layout_output(
            ids,
            output.name(),
            output_rect,
            nodes,
            config,
        ));
    }
    tiles
}

fn layout_output(
    ids: &[NodeId],
    output: String,
    bounds: Rectangle<i32, Logical>,
    nodes: &crate::nodes::NodesState,
    config: halley_config::Apogee,
) -> Vec<Tile> {
    if ids.is_empty() {
        return Vec::new();
    }
    let gap = config.gap.round() as i32;
    let rows = (ids.len() as u32).min(config.max_rows).max(1) as usize;
    let columns = ids.len().div_ceil(rows);
    let outer = gap.max(16);
    let available_w = (bounds.size.w - outer * 2 - gap * (columns as i32 - 1)).max(1);
    let available_h = (bounds.size.h - outer * 2 - gap * (rows as i32 - 1)).max(1);
    let cell_w = (available_w / columns as i32).max(1);
    let cell_h = (available_h / rows as i32).max(1);
    ids.iter()
        .enumerate()
        .filter_map(|(index, id)| {
            let record = nodes.record(*id)?;
            let column = index / rows;
            let row = index % rows;
            let aspect =
                record.geometry.size.w.max(1) as f32 / record.geometry.size.h.max(1) as f32;
            let preview_h = (cell_h - 48).max(1);
            let scale = (cell_w as f32 / record.geometry.size.w.max(1) as f32)
                .min(preview_h as f32 / record.geometry.size.h.max(1) as f32);
            let width = (record.geometry.size.w as f32 * scale)
                .round()
                .clamp(1.0, cell_w as f32) as i32;
            let height = (width as f32 / aspect).round().clamp(1.0, preview_h as f32) as i32;
            let cell_x = bounds.loc.x + outer + column as i32 * (cell_w + gap);
            let cell_y = bounds.loc.y + outer + row as i32 * (cell_h + gap);
            Some(Tile {
                id: *id,
                output: output.clone(),
                target: Rectangle::new(
                    (
                        cell_x + (cell_w - width) / 2,
                        cell_y + (cell_h - height) / 2,
                    )
                        .into(),
                    (width, height).into(),
                ),
            })
        })
        .collect()
}

fn proportional_duration(base_ms: u64, distance: f32) -> Duration {
    Duration::from_millis((base_ms as f32 * distance.clamp(0.0, 1.0)).round() as u64)
}

fn rect_center(rect: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    (
        rect.loc.x as f64 + rect.size.w as f64 * 0.5,
        rect.loc.y as f64 + rect.size.h as f64 * 0.5,
    )
        .into()
}

fn ease_in_out_cubic(value: f32) -> f32 {
    if value < 0.5 {
        4.0 * value * value * value
    } else {
        1.0 - (-2.0 * value + 2.0).powi(3) * 0.5
    }
}

pub fn toggle<D: crate::session::SessionDriver>(session: &mut crate::session::Session<D>) -> bool {
    let now = crate::frame_clock::monotonic_now();
    let changed = if session.apogee.is_active() {
        session.apogee.close(None, session.apogee_config, now)
    } else if session.capture.is_active() || session.focus_cycle.is_open() {
        false
    } else {
        session.nodes.sync_from_space(&session.wayland.space);
        session.apogee.open(
            &session.wayland.space,
            &session.nodes,
            session.apogee_config,
            now,
        )
    };
    if changed {
        session.request_redraw();
    }
    changed
}

pub fn cancel<D: crate::session::SessionDriver>(session: &mut crate::session::Session<D>) -> bool {
    let changed = session.apogee.close(
        None,
        session.apogee_config,
        crate::frame_clock::monotonic_now(),
    );
    if changed {
        session.request_redraw();
    }
    changed
}

pub fn select<D: crate::session::SessionDriver>(session: &mut crate::session::Session<D>) -> bool {
    let target = session.apogee.selected();
    let changed = session.apogee.close(
        target,
        session.apogee_config,
        crate::frame_clock::monotonic_now(),
    );
    if changed {
        session.request_redraw();
    }
    changed
}

pub fn pointer_motion<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    position: (f64, f64),
) -> bool {
    let changed = session.apogee.hover(Point::from(position));
    if changed {
        session.request_redraw();
    }
    changed
}

pub fn pointer_press<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    position: (f64, f64),
) -> bool {
    let changed = session.apogee.activate_at(
        Point::from(position),
        session.apogee_config,
        crate::frame_clock::monotonic_now(),
    );
    if changed {
        session.request_redraw();
    }
    changed
}

pub fn move_selection<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    direction: Direction,
) -> bool {
    let changed = session.apogee.move_selection(direction);
    if changed {
        session.request_redraw();
    }
    changed
}

pub fn tick<D: crate::session::SessionDriver>(
    session: &mut crate::session::Session<D>,
    now: Duration,
) -> bool {
    match session.apogee.tick(now) {
        Tick::Idle => false,
        Tick::Active { animating } => animating,
        Tick::Closed(target) => {
            if let Some(target) = target
                && let Some(record) = session.nodes.record(target).cloned()
            {
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                if record.collapsed {
                    let _ = crate::nodes::restore(session, target, serial);
                } else {
                    crate::session::focus_window(session, &record.window, serial);
                }
            }
            session.request_redraw();
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Session, Settle, Tile};
    use halley_core::field::NodeId;
    use smithay::utils::Rectangle;
    use std::time::Duration;

    #[test]
    fn reversing_manual_progress_has_no_discontinuity() {
        let mut session = Session {
            tiles: vec![Tile {
                id: NodeId::new(1),
                output: "DP-1".into(),
                target: Rectangle::new((0, 0).into(), (100, 100).into()),
            }],
            hovered: None,
            selected: Some(NodeId::new(1)),
            settle: None,
            manual_progress: Some(0.7),
            pending_activation: None,
        };
        assert_eq!(session.progress(Duration::ZERO), 0.7);
        session.manual_progress = Some(0.2);
        assert_eq!(session.progress(Duration::from_secs(20)), 0.2);
        session.manual_progress = Some(0.0);
        assert_eq!(session.progress(Duration::from_secs(30)), 0.0);
    }

    #[test]
    fn proportional_close_starts_at_the_live_progress() {
        let session = Session {
            tiles: Vec::new(),
            hovered: None,
            selected: None,
            settle: Some(Settle {
                from: 0.4,
                to: 0.0,
                started_at: Duration::from_secs(2),
                duration: Duration::from_millis(128),
            }),
            manual_progress: None,
            pending_activation: None,
        };
        assert_eq!(session.progress(Duration::from_secs(2)), 0.4);
        assert_eq!(
            session.progress(Duration::from_secs(2) + Duration::from_millis(128)),
            0.0
        );
    }
}
