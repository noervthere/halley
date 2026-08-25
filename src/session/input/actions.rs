use std::ffi::OsStr;

use smithay::output::Output;
use smithay::utils::{Rectangle, SERIAL_COUNTER};
use smithay::wayland::seat::WaylandFocus;

use super::{
    Session, SessionDriver, cluster_owns_focus, focus_output_target, navigate_cluster,
    sync_cluster_activation_focus, toggle_cluster_or_focused_node, work_area_for_output,
};

pub(super) fn window_action_output(
    focus_mode: halley_config::FocusMode,
    pointer_output: Option<&str>,
    selected_output: Option<&str>,
) -> Option<String> {
    match focus_mode {
        halley_config::FocusMode::Hover => pointer_output.or(selected_output).map(str::to_owned),
        halley_config::FocusMode::Click => selected_output.map(str::to_owned),
    }
}

pub(super) fn cluster_blocks_zoom(action: &halley_config::Action, active_cluster: bool) -> bool {
    active_cluster
        && matches!(
            action,
            halley_config::Action::ZoomIn
                | halley_config::Action::ZoomOut
                | halley_config::Action::ZoomReset
        )
}

fn toggle_focused_cluster_float<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: Option<&str>,
) {
    let Some(output_name) = output_name else {
        return;
    };
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == output_name)
        .cloned()
    else {
        return;
    };
    let Some(record) = super::super::focused_window_record(session, Some(output_name)) else {
        return;
    };
    let Some(cluster) = session.clusters.cluster_for_member(record.id) else {
        return;
    };
    let Some(cluster_output_name) = session
        .clusters
        .metadata(cluster)
        .map(|metadata| metadata.output.clone())
    else {
        return;
    };
    if session.clusters.active_on(&cluster_output_name) != Some(cluster)
        || session.fullscreen.is_fullscreen_or_pending(&record.surface)
        || session.maximize.contains(&record.surface)
        || crate::input::grab::belongs_to_surface(&session.interactions.grab, &record.surface)
    {
        return;
    }
    let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
        return;
    };
    let Some(cluster_output) = session
        .wayland
        .space
        .outputs()
        .find(|candidate| candidate.name() == cluster_output_name)
        .cloned()
    else {
        return;
    };
    let Some(cluster_output_geometry) = session.wayland.space.output_geometry(&cluster_output)
    else {
        return;
    };
    let now = crate::frame_clock::monotonic_now();
    let current = super::super::presented_window_rect(session, &record.window, &output, now)
        .map(|rect| rect.to_logical(1))
        .or_else(|| {
            session
                .wayland
                .space
                .element_geometry(&record.window)
                .map(|rect| Rectangle::new(rect.loc - output_geometry.loc, rect.size))
        });
    let Some(current) = current else {
        return;
    };
    let cluster_local_current = Rectangle::new(
        output_geometry.loc + current.loc - cluster_output_geometry.loc,
        current.size,
    );
    let work_area = smithay::desktop::layer_map_for_output(&cluster_output).non_exclusive_zone();
    let Some(floating) = session.clusters.toggle_member_floating(
        &cluster_output_name,
        record.id,
        work_area,
        cluster_local_current,
        now,
    ) else {
        return;
    };
    let member_output_name = session
        .clusters
        .member_floating_output(record.id)
        .map(str::to_owned)
        .unwrap_or_else(|| cluster_output_name.clone());
    let member_output = session
        .wayland
        .space
        .outputs()
        .find(|candidate| candidate.name() == member_output_name)
        .cloned()
        .unwrap_or_else(|| cluster_output.clone());
    crate::nodes::set_collapsed_output(session, record.id, &member_output);
    super::super::reconcile_cluster_surfaces(session, &cluster_output_name);
    if member_output_name != cluster_output_name {
        super::super::reconcile_cluster_surfaces(session, &member_output_name);
    }
    if floating {
        crate::window::raise_managed(&mut session.wayland, &record.window);
        session.xwayland.raise_window(&record.window);
    }
    session.request_redraw();
}

pub(super) fn dispatch<D: SessionDriver>(
    session: &mut Session<D>,
    action: halley_config::Action,
    socket_name: &OsStr,
    output_name: Option<&str>,
    held_keycode: Option<u32>,
) {
    let zoom_action = matches!(
        &action,
        halley_config::Action::ZoomIn
            | halley_config::Action::ZoomOut
            | halley_config::Action::ZoomReset
    );
    let selected_output =
        crate::wayland::focus::selected_output(&session.wayland).map(Output::name);
    let action_output = window_action_output(
        session.settings.input.focus_mode,
        output_name,
        selected_output.as_deref(),
    );
    let x11_display = session.xwayland.display_name();
    let cluster_blocks_zoom = output_name.is_some_and(|name| {
        cluster_blocks_zoom(&action, session.clusters.active_on(name).is_some())
    });
    let camera = (!cluster_blocks_zoom)
        .then(|| output_name.and_then(|name| session.cameras.get_mut(name)))
        .flatten();
    match super::super::dispatch_action(
        action,
        session.keyboard.terminal_command(),
        super::super::SpawnContext {
            socket_name,
            x11_display: x11_display.as_deref(),
            cursor_size: session.cursor.size(),
            environment: &session.launch_environment,
        },
        camera,
        &session.settings.zoom,
    ) {
        super::super::SessionControl::Continue => {}
        super::super::SessionControl::Quit => session.show_exit_confirmation(),
        super::super::SessionControl::CloseFocusedWindow => {
            crate::nodes::close_focused_on_output(session, action_output.as_deref())
        }
        super::super::SessionControl::ToggleFullscreen => {
            super::super::toggle_focused_fullscreen(session, action_output.as_deref())
        }
        super::super::SessionControl::ToggleFieldMaximize => {
            super::super::toggle_focused_field_maximize(session, action_output.as_deref())
        }
        super::super::SessionControl::ToggleState => toggle_cluster_or_focused_node(
            session,
            action_output.as_deref(),
            SERIAL_COUNTER.next_serial(),
        ),
        super::super::SessionControl::ToggleFocusedPin => {
            super::super::toggle_focused_pin(session, action_output.as_deref());
        }
        super::super::SessionControl::Apogee => {
            crate::shell::apogee::toggle(session);
        }
        super::super::SessionControl::FocusCycle(direction) => {
            let cluster_cycle = action_output.as_deref().map_or(
                crate::clusters::StackCycleOutcome::NotActive,
                |output| {
                    let Some(work_area) = work_area_for_output(&session.wayland.space, output)
                    else {
                        return crate::clusters::StackCycleOutcome::NotActive;
                    };
                    session.clusters.cycle_stack(
                        output,
                        direction,
                        work_area,
                        crate::frame_clock::monotonic_now(),
                    )
                },
            );
            match cluster_cycle {
                crate::clusters::StackCycleOutcome::Cycled(member) => {
                    if let Some(window) = session
                        .nodes
                        .record(member)
                        .map(|record| record.window.clone())
                    {
                        super::super::focus_window(session, &window, SERIAL_COUNTER.next_serial());
                        session.request_redraw();
                    }
                }
                crate::clusters::StackCycleOutcome::Unchanged => {}
                crate::clusters::StackCycleOutcome::NotActive => {
                    crate::shell::focus_cycle::start_or_step(session, direction);
                }
            }
        }
        super::super::SessionControl::Trail(direction) => {
            if let Err(error) = crate::trail::navigate(session, direction, action_output.as_deref())
            {
                eventline::debug!("trail: {error}");
            }
        }
        super::super::SessionControl::FocusDirection(direction) => {
            if let Some(output) = action_output {
                if session.clusters.active_on(&output).is_some() {
                    navigate_cluster(session, &output, direction, false);
                } else {
                    super::super::navigation::focus_directional_field(session, &output, direction);
                }
            }
        }
        super::super::SessionControl::MoveNode(direction) => {
            if let Some(output) = action_output
                && session.clusters.active_on(&output).is_none()
            {
                crate::nodes::move_selected_direction(session, direction, Some(&output));
            }
        }
        super::super::SessionControl::ResizeWindow(direction) => {
            if let Some(output) = action_output
                && session.clusters.active_on(&output).is_none()
            {
                crate::nodes::resize_selected_direction(session, direction, Some(&output));
            }
        }
        super::super::SessionControl::CenterLastFocused => {
            if let Some(output) = action_output {
                super::super::navigation::center_last_focused(session, &output);
            }
        }
        super::super::SessionControl::ClusterMode => {
            if let Some(output) = action_output
                && session.clusters.begin_creation(output)
            {
                session.request_redraw();
            }
        }
        super::super::SessionControl::ClusterLayoutCycle => {
            if let Some(output) = action_output
                && let Some(work_area) = work_area_for_output(&session.wayland.space, &output)
                && session.clusters.cycle_active_layout(
                    &output,
                    work_area,
                    crate::frame_clock::monotonic_now(),
                )
            {
                session.request_redraw();
            }
        }
        super::super::SessionControl::ClusterToggleFloat => {
            toggle_focused_cluster_float(session, action_output.as_deref());
        }
        super::super::SessionControl::ClusterSlot(slot) => {
            if let Some(output_name) = action_output {
                let target = session
                    .clusters
                    .clusters_for_output(&output_name)
                    .find_map(|(candidate_slot, id, _)| (candidate_slot == slot).then_some(id));
                let owned_focus = target.is_some_and(|id| cluster_owns_focus(session, id));
                if session.clusters.activate_slot(
                    &output_name,
                    slot,
                    crate::frame_clock::monotonic_now(),
                ) {
                    if let Some(id) = target {
                        let output = session
                            .wayland
                            .space
                            .outputs()
                            .find(|candidate| candidate.name() == output_name)
                            .cloned();
                        if let Some(output) = output {
                            sync_cluster_activation_focus(
                                session,
                                &output,
                                id,
                                owned_focus,
                                SERIAL_COUNTER.next_serial(),
                            );
                        }
                    }
                    session.request_redraw();
                }
            }
        }
        super::super::SessionControl::ClusterTileFocus(direction) => {
            if let Some(output) = action_output {
                navigate_cluster(session, &output, direction, false);
            }
        }
        super::super::SessionControl::ClusterTileSwap(direction) => {
            if let Some(output) = action_output {
                navigate_cluster(session, &output, direction, true);
            }
        }
        super::super::SessionControl::MonitorFocus(target) => focus_output_target(session, target),
        super::super::SessionControl::Reload => {
            if let Some(watcher) = session.config_watcher.as_ref() {
                watcher.request_reload();
            } else {
                eventline::warn!("config: manual reload requested without a watched file");
            }
        }
        super::super::SessionControl::BearingsShow => {
            let changed = match held_keycode {
                Some(keycode) => session.shell.bearings.show_while_held(keycode),
                None => session.shell.bearings.set_visible(true),
            };
            if changed {
                session.request_redraw();
            }
        }
        super::super::SessionControl::BearingsToggle => {
            session.shell.bearings.toggle();
            session.request_redraw();
        }
        super::super::SessionControl::Screenshot => {
            let window_available = session.wayland.space.elements().any(|window| {
                window.wl_surface().is_some()
                    && crate::wayland::window_output_name(window)
                        .map(|name| Some(name.as_str()) == output_name)
                        .unwrap_or_else(|| {
                            output_name
                                .is_some_and(|name| name == session.driver.primary_output().name())
                        })
            });
            crate::capture::begin_local(session, output_name, window_available);
        }
    }
    if zoom_action
        && !cluster_blocks_zoom
        && let Some(output_name) = output_name
    {
        let scale = session
            .cameras
            .get(output_name)
            .map(crate::presentation::camera::target_scale);
        if let Some(scale) = scale {
            crate::nodes::reconcile_landmarks_at_scale(session, output_name, scale);
        }
    }
}
