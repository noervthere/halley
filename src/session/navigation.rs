use halley_core::field::Vec2;
use halley_core::world::PortalDir;
use smithay::utils::SERIAL_COUNTER;

use super::{Session, SessionDriver};

fn portal_direction(direction: halley_config::Direction) -> PortalDir {
    match direction {
        halley_config::Direction::Left => PortalDir::W,
        halley_config::Direction::Right => PortalDir::E,
        halley_config::Direction::Up => PortalDir::N,
        halley_config::Direction::Down => PortalDir::S,
    }
}

/// Selects a directional neighbour in the output's Field. Cluster members are
/// deliberately excluded: while a cluster is closed their Field geometry is
/// only storage, and while it is open cluster navigation owns the action.
pub(super) fn focus_directional_field<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
    direction: halley_config::Direction,
) -> bool {
    let Some(current) = session.nodes.focused_on_output(output_name) else {
        return false;
    };
    if session.clusters.is_member(current) {
        return false;
    }
    let Some(current_rect) = halley_core::focus::node_field_rect(&session.nodes.field, current)
    else {
        return false;
    };
    let direction = portal_direction(direction);
    let target = session
        .nodes
        .field
        .nodes()
        .keys()
        .copied()
        .filter(|id| *id != current && !session.clusters.is_member(*id))
        .filter(|id| {
            session.nodes.record(*id).is_some_and(|record| {
                record.attached && record.output == output_name && !record.collapsed
            })
        })
        .filter_map(|id| {
            let rect = halley_core::focus::node_field_rect(&session.nodes.field, id)?;
            let score =
                halley_core::focus::directional_candidate_score(current_rect, rect, direction)?;
            Some((score, id.as_u64(), id))
        })
        .min_by(|a, b| {
            a.0.0
                .total_cmp(&b.0.0)
                .then_with(|| a.0.1.total_cmp(&b.0.1))
                .then_with(|| a.0.2.total_cmp(&b.0.2))
                .then_with(|| a.0.3.total_cmp(&b.0.3))
                .then_with(|| a.1.cmp(&b.1))
        })
        .map(|(_, _, id)| id);

    target.is_some_and(|id| {
        crate::nodes::focus_or_reveal_node(session, id, SERIAL_COUNTER.next_serial())
    })
}

/// Focuses the output's most recent Field window and pans that output's camera
/// to its stored center. It never changes window geometry or restores a
/// collapsed node, and camera ownership makes this a no-op in cluster,
/// fullscreen, and maximized presentations.
pub(super) fn center_last_focused<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
) -> bool {
    let Some(id) = session.nodes.focused_on_output(output_name) else {
        return false;
    };
    if session.clusters.is_member(id) {
        return false;
    }
    let Some(record) = session.nodes.record(id).cloned() else {
        return false;
    };
    let Some(node) = session.nodes.field.node(id) else {
        return false;
    };
    if !record.attached || record.output != output_name {
        return false;
    }
    let target = node.pos;
    let Some(camera) = session.cameras.get_mut(output_name) else {
        return false;
    };
    camera.pan_vel = Vec2 { x: 0.0, y: 0.0 };
    camera.target_center = target;

    if record.collapsed {
        crate::window::clear_focus(&mut session.wayland);
        session
            .nodes
            .focus(Some(id), session.start_time.elapsed().as_millis() as u64);
        super::sync_keyboard_focus(session, SERIAL_COUNTER.next_serial());
    } else {
        super::focus_window(session, &record.window, SERIAL_COUNTER.next_serial());
    }
    session.request_redraw();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_config_directions_to_field_directions() {
        assert_eq!(
            portal_direction(halley_config::Direction::Left),
            PortalDir::W
        );
        assert_eq!(
            portal_direction(halley_config::Direction::Right),
            PortalDir::E
        );
        assert_eq!(portal_direction(halley_config::Direction::Up), PortalDir::N);
        assert_eq!(
            portal_direction(halley_config::Direction::Down),
            PortalDir::S
        );
    }
}
