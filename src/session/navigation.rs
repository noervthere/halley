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

fn output_local_center(world: Vec2, output_origin: (i32, i32)) -> Vec2 {
    Vec2 {
        x: world.x - output_origin.0 as f32,
        y: world.y - output_origin.1 as f32,
    }
}

fn output_geometry_center(
    geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
) -> (f64, f64) {
    (
        f64::from(geometry.loc.x) + f64::from(geometry.size.w) * 0.5,
        f64::from(geometry.loc.y) + f64::from(geometry.size.h) * 0.5,
    )
}

/// Moves the compositor pointer to the center of a named output. Presentation
/// navigation uses this after activation so a cross-output jump never leaves
/// the pointer behind on a different monitor.
pub(crate) fn center_pointer_on_output<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
) -> bool {
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == output_name)
        .cloned()
    else {
        return false;
    };
    let Some(geometry) = session.wayland.space.output_geometry(&output) else {
        return false;
    };
    super::pointer::release_for_compositor_warp(session);
    session
        .pointer
        .set_position(output_geometry_center(geometry));
    session.cursor_policy.pointer_activity();
    super::pointer::update_client_state(session, session.start_time.elapsed().as_millis() as u32);
    session.request_output_redraw(&output);
    true
}

/// Selects a directional neighbour in the output's Field. Cluster members are
/// deliberately excluded: while a cluster is closed their Field geometry is
/// only storage, and while it is open cluster navigation owns the action.
pub(super) fn focus_directional_field<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
    direction: halley_config::Direction,
) -> bool {
    // Directional Field focus must not punch through another presentation's
    // camera ownership (fullscreen, maximize, or a cluster workspace).
    if session.cameras.get_mut(output_name).is_none() {
        return false;
    }
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
    let target = output_local_center(node.pos, (output_geometry.loc.x, output_geometry.loc.y));
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

    #[test]
    fn output_pointer_center_uses_global_layout_coordinates() {
        let secondary = smithay::utils::Rectangle::new((2560, -120).into(), (1920, 1200).into());

        assert_eq!(output_geometry_center(secondary), (3520.0, 480.0));
    }

    #[test]
    fn centering_converts_global_field_position_to_output_local_camera_position() {
        assert_eq!(
            output_local_center(
                Vec2 {
                    x: 2080.0,
                    y: 500.0
                },
                (1280, 0)
            ),
            Vec2 { x: 800.0, y: 500.0 }
        );
    }
}
