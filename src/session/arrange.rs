use halley_core::camera::Camera;
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::{Session, SessionDriver};

const INFEASIBLE_COST: i64 = i64::MAX / 16;
const UNREACHED_COST: i64 = i64::MAX / 4;

#[derive(Clone, Debug, Default)]
pub(crate) struct UndoSnapshot {
    restores: Vec<crate::presentation::maximize::FieldRestore>,
}

impl UndoSnapshot {
    pub(crate) fn clear(&mut self) {
        self.restores.clear();
    }
}

#[derive(Clone)]
struct Candidate {
    window: Window,
    surface: WlSurface,
    current: Rectangle<i32, Logical>,
    center: Point<i32, Logical>,
}

pub(crate) fn arrange_visible<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
) -> bool {
    if session.clusters.active_on(output_name).is_some()
        || !matches!(session.interactions.grab, crate::input::grab::Grab::None)
    {
        return false;
    }
    session.interactions.field_arrange_undo.clear();
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == output_name)
        .cloned()
    else {
        return false;
    };
    let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
        return false;
    };
    let Some(camera) = session.cameras.get(output_name) else {
        return false;
    };
    let work_area = smithay::desktop::layer_map_for_output(&output).non_exclusive_zone();
    let configured_gap = session.settings.field.gap.ceil() as i32;
    let Some((visible_outer, gap)) =
        visible_work_area_outer(camera, output_geometry, work_area, configured_gap)
    else {
        return false;
    };

    session.nodes.sync_from_space(&session.wayland.space);
    let mut candidates = session
        .nodes
        .records()
        .filter(|record| {
            record.attached
                && !record.collapsed
                && record.output == output_name
                && session.nodes.field.is_visible(record.id)
                && !session.clusters.is_member(record.id)
                && !super::node_user_pinned(session, record.id)
                && !session.fullscreen.is_fullscreen_or_pending(&record.surface)
                && !session.maximize.contains(&record.surface)
                && !crate::input::grab::belongs_to_surface(
                    &session.interactions.grab,
                    &record.surface,
                )
        })
        .filter_map(|record| {
            let current = session.wayland.space.element_geometry(&record.window)?;
            let outer = crate::titlebar::outer_rect_for_client(
                &record.window,
                current,
                &session.settings.decorations,
                &session.settings.font,
            );
            let center = rect_center(outer);
            visible_outer.contains(center).then(|| Candidate {
                window: record.window.clone(),
                surface: record.surface.clone(),
                current,
                center,
            })
        })
        .collect::<Vec<_>>();

    let assignment = loop {
        if candidates.len() < 2 {
            return false;
        }
        let Some(regions) = mosaic_regions(visible_outer, candidates.len(), gap) else {
            return false;
        };
        let target_clients = candidates
            .iter()
            .map(|candidate| {
                regions
                    .iter()
                    .map(|region| {
                        crate::titlebar::client_rect_for_outer(
                            &candidate.window,
                            *region,
                            &session.settings.decorations,
                            &session.settings.font,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let costs = candidates
            .iter()
            .enumerate()
            .map(|(candidate_index, candidate)| {
                regions
                    .iter()
                    .enumerate()
                    .map(|(region_index, region)| {
                        let target = target_clients[candidate_index][region_index];
                        if !window_size_is_accepted(&candidate.window, target.size) {
                            INFEASIBLE_COST
                        } else {
                            let feasible_ceiling =
                                INFEASIBLE_COST / (candidates.len() as i64 + 1) - 1;
                            squared_distance(candidate.center, rect_center(*region))
                                .min(feasible_ceiling)
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let assignment = minimum_cost_assignment(&costs);
        if assignment
            .iter()
            .enumerate()
            .all(|(candidate, region)| costs[candidate][*region] < INFEASIBLE_COST)
        {
            break assignment
                .into_iter()
                .enumerate()
                .map(|(candidate, region)| (candidate, target_clients[candidate][region]))
                .collect::<Vec<_>>();
        }

        let remove = costs
            .iter()
            .enumerate()
            .min_by_key(|(index, row)| {
                (
                    row.iter().filter(|cost| **cost < INFEASIBLE_COST).count(),
                    std::cmp::Reverse(
                        candidates[*index].current.size.w as i64
                            * candidates[*index].current.size.h as i64,
                    ),
                )
            })
            .map(|(index, _)| index)
            .expect("candidate list is non-empty");
        candidates.remove(remove);
    };

    let restores = candidates
        .iter()
        .map(|candidate| crate::presentation::maximize::FieldRestore {
            surface: candidate.surface.clone(),
            geometry: candidate.current,
            output: output_name.to_string(),
        })
        .collect::<Vec<_>>();
    for (candidate_index, target) in assignment {
        super::configure_field_geometry(
            session,
            &crate::presentation::maximize::FieldRestore {
                surface: candidates[candidate_index].surface.clone(),
                geometry: target,
                output: output_name.to_string(),
            },
        );
    }
    session.interactions.field_arrange_undo = UndoSnapshot { restores };
    session.request_output_redraw(&output);
    true
}

pub(crate) fn undo_last<D: SessionDriver>(session: &mut Session<D>) -> bool {
    let snapshot = std::mem::take(&mut session.interactions.field_arrange_undo);
    if snapshot.restores.is_empty() {
        return false;
    }
    let mut restored = false;
    for request in snapshot.restores {
        let eligible = session
            .nodes
            .id_for_surface(&request.surface)
            .and_then(|id| session.nodes.record(id).map(|record| (id, record)))
            .is_some_and(|(id, record)| {
                record.attached
                    && !record.collapsed
                    && !session.clusters.is_member(id)
                    && !session.fullscreen.is_fullscreen_or_pending(&record.surface)
                    && !session.maximize.contains(&record.surface)
            });
        if eligible {
            super::configure_field_geometry(session, &request);
            restored = true;
        }
    }
    if restored {
        session.request_redraw();
    }
    restored
}

fn window_size_is_accepted(window: &Window, requested: Size<i32, Logical>) -> bool {
    if requested.w < 1 || requested.h < 1 {
        return false;
    }
    if crate::xwayland::is_x11(window) {
        return crate::xwayland::constrain_window_size(window, requested) == requested;
    }
    let Some(toplevel) = window.toplevel() else {
        return false;
    };
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        let mut cached = states
            .cached_state
            .get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
        let state = cached.current();
        let minimum_ok = (state.min_size.w <= 0 || requested.w >= state.min_size.w)
            && (state.min_size.h <= 0 || requested.h >= state.min_size.h);
        let maximum_ok = (state.max_size.w <= 0 || requested.w <= state.max_size.w)
            && (state.max_size.h <= 0 || requested.h <= state.max_size.h);
        minimum_ok && maximum_ok
    })
}

fn visible_work_area_outer(
    camera: &Camera,
    output_geometry: Rectangle<i32, Logical>,
    work_area: Rectangle<i32, Logical>,
    gap: i32,
) -> Option<(Rectangle<i32, Logical>, i32)> {
    let gap = gap.max(0);
    let width = work_area.size.w.checked_sub(gap.checked_mul(2)?)?;
    let height = work_area.size.h.checked_sub(gap.checked_mul(2)?)?;
    if width < 2 || height < 2 {
        return None;
    }
    let screen_outer = Rectangle::new(
        output_geometry.loc + work_area.loc + Point::from((gap, gap)),
        (width, height).into(),
    );
    let top_left = crate::input::grab::screen_to_world_on_output(
        (f64::from(screen_outer.loc.x), f64::from(screen_outer.loc.y)),
        camera,
        output_geometry,
    );
    let bottom_right = crate::input::grab::screen_to_world_on_output(
        (
            f64::from(screen_outer.loc.x + screen_outer.size.w),
            f64::from(screen_outer.loc.y + screen_outer.size.h),
        ),
        camera,
        output_geometry,
    );
    let left = top_left.x.round() as i32;
    let top = top_left.y.round() as i32;
    let right = bottom_right.x.round() as i32;
    let bottom = bottom_right.y.round() as i32;
    let outer = Rectangle::new(
        (left, top).into(),
        (right.checked_sub(left)?, bottom.checked_sub(top)?).into(),
    );
    if outer.size.w < 2 || outer.size.h < 2 {
        return None;
    }
    let scale = crate::input::zoom::scale(camera).max(0.05);
    let world_gap = ((gap as f32) / scale).ceil() as i32;
    Some((outer, world_gap))
}

fn mosaic_regions(
    outer: Rectangle<i32, Logical>,
    count: usize,
    gap: i32,
) -> Option<Vec<Rectangle<i32, Logical>>> {
    if count < 2 {
        return None;
    }
    if count == 3 {
        let columns = split_axis(outer.loc.x, outer.size.w, 2, gap)?;
        let right_rows = split_axis(outer.loc.y, outer.size.h, 2, gap)?;
        return Some(vec![
            Rectangle::new(
                (columns[0].0, outer.loc.y).into(),
                (columns[0].1, outer.size.h).into(),
            ),
            Rectangle::new(
                (columns[1].0, right_rows[0].0).into(),
                (columns[1].1, right_rows[0].1).into(),
            ),
            Rectangle::new(
                (columns[1].0, right_rows[1].0).into(),
                (columns[1].1, right_rows[1].1).into(),
            ),
        ]);
    }

    let columns = ceil_sqrt(count);
    let rows = count.div_ceil(columns);
    let row_heights = split_axis(outer.loc.y, outer.size.h, rows, gap)?;
    let base = count / rows;
    let extra = count % rows;
    let mut regions = Vec::with_capacity(count);
    for (row, (top, height)) in row_heights.into_iter().enumerate() {
        let in_row = base + usize::from(row < extra);
        let widths = split_axis(outer.loc.x, outer.size.w, in_row, gap)?;
        regions.extend(
            widths
                .into_iter()
                .map(|(left, width)| Rectangle::new((left, top).into(), (width, height).into())),
        );
    }
    Some(regions)
}

fn split_axis(start: i32, length: i32, count: usize, gap: i32) -> Option<Vec<(i32, i32)>> {
    let count = i32::try_from(count).ok()?;
    let gap = gap.max(0);
    let available = length.checked_sub(gap.checked_mul(count.checked_sub(1)?)?)?;
    if available < count {
        return None;
    }
    let base = available / count;
    let remainder = available % count;
    let mut cursor = start;
    let mut segments = Vec::with_capacity(count as usize);
    for index in 0..count {
        let size = base + i32::from(index < remainder);
        segments.push((cursor, size));
        cursor = cursor.checked_add(size)?.checked_add(gap)?;
    }
    Some(segments)
}

fn ceil_sqrt(value: usize) -> usize {
    let mut root = 1usize;
    while root.saturating_mul(root) < value {
        root += 1;
    }
    root
}

fn rect_center(rect: Rectangle<i32, Logical>) -> Point<i32, Logical> {
    (rect.loc.x + rect.size.w / 2, rect.loc.y + rect.size.h / 2).into()
}

fn squared_distance(a: Point<i32, Logical>, b: Point<i32, Logical>) -> i64 {
    let dx = i64::from(a.x) - i64::from(b.x);
    let dy = i64::from(a.y) - i64::from(b.y);
    dx.saturating_mul(dx)
        .saturating_add(dy.saturating_mul(dy))
        .min(INFEASIBLE_COST - 1)
}

fn minimum_cost_assignment(costs: &[Vec<i64>]) -> Vec<usize> {
    let count = costs.len();
    debug_assert!(costs.iter().all(|row| row.len() == count));
    let mut row_potential = vec![0i64; count + 1];
    let mut column_potential = vec![0i64; count + 1];
    let mut matched_row = vec![0usize; count + 1];
    let mut previous_column = vec![0usize; count + 1];

    for row in 1..=count {
        matched_row[0] = row;
        let mut column = 0usize;
        let mut minimum = vec![UNREACHED_COST; count + 1];
        let mut used = vec![false; count + 1];
        loop {
            used[column] = true;
            let active_row = matched_row[column];
            let mut delta = UNREACHED_COST;
            let mut next_column = 0usize;
            for candidate_column in 1..=count {
                if used[candidate_column] {
                    continue;
                }
                let reduced = costs[active_row - 1][candidate_column - 1]
                    .saturating_sub(row_potential[active_row])
                    .saturating_sub(column_potential[candidate_column]);
                if reduced < minimum[candidate_column] {
                    minimum[candidate_column] = reduced;
                    previous_column[candidate_column] = column;
                }
                if minimum[candidate_column] < delta {
                    delta = minimum[candidate_column];
                    next_column = candidate_column;
                }
            }
            for candidate_column in 0..=count {
                if used[candidate_column] {
                    row_potential[matched_row[candidate_column]] =
                        row_potential[matched_row[candidate_column]].saturating_add(delta);
                    column_potential[candidate_column] =
                        column_potential[candidate_column].saturating_sub(delta);
                } else {
                    minimum[candidate_column] = minimum[candidate_column].saturating_sub(delta);
                }
            }
            column = next_column;
            if matched_row[column] == 0 {
                break;
            }
        }
        loop {
            let prior = previous_column[column];
            matched_row[column] = matched_row[prior];
            column = prior;
            if column == 0 {
                break;
            }
        }
    }

    let mut assignment = vec![0usize; count];
    for column in 1..=count {
        assignment[matched_row[column] - 1] = column - 1;
    }
    assignment
}

#[cfg(test)]
mod tests {
    use super::{minimum_cost_assignment, mosaic_regions};
    use smithay::utils::{Logical, Rectangle};

    #[test]
    fn two_windows_fill_halves_with_gap() {
        let regions = mosaic_regions(
            Rectangle::<i32, Logical>::new((0, 0).into(), (1000, 800).into()),
            2,
            20,
        )
        .unwrap();
        assert_eq!(regions[0], Rectangle::new((0, 0).into(), (490, 800).into()));
        assert_eq!(
            regions[1],
            Rectangle::new((510, 0).into(), (490, 800).into())
        );
    }

    #[test]
    fn three_windows_use_large_left_and_split_right() {
        let regions = mosaic_regions(
            Rectangle::<i32, Logical>::new((0, 0).into(), (1000, 800).into()),
            3,
            20,
        )
        .unwrap();
        assert_eq!(regions[0], Rectangle::new((0, 0).into(), (490, 800).into()));
        assert_eq!(
            regions[1],
            Rectangle::new((510, 0).into(), (490, 390).into())
        );
        assert_eq!(
            regions[2],
            Rectangle::new((510, 410).into(), (490, 390).into())
        );
    }

    #[test]
    fn five_windows_form_balanced_three_two_mosaic() {
        let regions = mosaic_regions(
            Rectangle::<i32, Logical>::new((0, 0).into(), (1000, 800).into()),
            5,
            20,
        )
        .unwrap();
        assert_eq!(regions.len(), 5);
        assert_eq!(regions[0].size.h, 390);
        assert_eq!(regions[2].loc.y, 0);
        assert_eq!(regions[3].loc.y, 410);
        assert_eq!(regions[3].size.w, 490);
        assert_eq!(regions[4].loc.x, 510);
    }

    #[test]
    fn assignment_minimizes_total_window_travel() {
        let assignment =
            minimum_cost_assignment(&[vec![100, 1, 50], vec![1, 100, 50], vec![50, 50, 1]]);
        assert_eq!(assignment, vec![1, 0, 2]);
    }

    #[test]
    fn assignment_can_avoid_infeasible_targets() {
        let impossible = super::INFEASIBLE_COST;
        let assignment = minimum_cost_assignment(&[vec![impossible, 1], vec![1, impossible]]);
        assert_eq!(assignment, vec![1, 0]);
    }

    #[test]
    fn assignment_terminates_when_no_target_is_feasible() {
        let impossible = super::INFEASIBLE_COST;
        let mut assignment =
            minimum_cost_assignment(&[vec![impossible, impossible], vec![impossible, impossible]]);
        assignment.sort_unstable();
        assert_eq!(assignment, vec![0, 1]);
    }
}
