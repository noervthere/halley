use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::seat::WaylandFocus;

use super::{Session, SessionDriver};
use crate::wayland::WaylandState;

struct FocusSuccession {
    output: Option<String>,
    preferred: Option<WlSurface>,
    pan: halley_config::CloseRestorePan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseSuccessorAction {
    FocusWindow,
    FocusNode,
    RestoreNode,
}

fn close_successor_action(collapsed: bool, restore_nodes: bool) -> CloseSuccessorAction {
    match (collapsed, restore_nodes) {
        (false, _) => CloseSuccessorAction::FocusWindow,
        (true, false) => CloseSuccessorAction::FocusNode,
        (true, true) => CloseSuccessorAction::RestoreNode,
    }
}

pub(crate) struct WindowUnmapPreparation {
    surface: WlSurface,
    focus: Option<FocusSuccession>,
}

impl WindowUnmapPreparation {
    pub fn surface(&self) -> &WlSurface {
        &self.surface
    }
}

fn mapped_managed_window(wayland: &WaylandState, surface: &WlSurface) -> Option<Window> {
    if !wayland.managed_windows.contains(surface) {
        return None;
    }
    wayland
        .space
        .elements()
        .find(|window| {
            !crate::xwayland::is_override_redirect(window)
                && window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == surface)
        })
        .cloned()
}

fn select_focus_successor(
    wayland: &WaylandState,
    nodes: &crate::nodes::NodesState,
    closing: &WlSurface,
    closing_output: Option<&str>,
) -> Option<WlSurface> {
    select_ordered_successor(
        wayland.managed_windows.top_to_bottom().cloned(),
        closing,
        closing_output,
        |surface| {
            nodes
                .id_for_surface(surface)
                .and_then(|id| nodes.record(id))
                .filter(|record| record.attached)
                .map(|record| Some(record.output.clone()))
        },
        |surface| {
            nodes
                .id_for_surface(surface)
                .and_then(|id| nodes.last_focus_ms().get(&id))
                .copied()
                .unwrap_or(0)
        },
    )
}

fn select_ordered_successor<T>(
    candidates: impl IntoIterator<Item = T>,
    closing: &T,
    closing_output: Option<&str>,
    mut mapped_output: impl FnMut(&T) -> Option<Option<String>>,
    mut last_focus_ms: impl FnMut(&T) -> u64,
) -> Option<T>
where
    T: Clone + Eq,
{
    let mut global: Option<(T, u64)> = None;
    let mut local: Option<(T, u64)> = None;
    for candidate in candidates {
        if &candidate == closing {
            continue;
        }
        let Some(output) = mapped_output(&candidate) else {
            continue;
        };
        let recency = last_focus_ms(&candidate);
        if global
            .as_ref()
            .is_none_or(|(_, current)| recency > *current)
        {
            global = Some((candidate.clone(), recency));
        }
        if closing_output.is_some_and(|closing_output| output.as_deref() == Some(closing_output)) {
            if local.as_ref().is_none_or(|(_, current)| recency > *current) {
                local = Some((candidate, recency));
            }
        }
    }
    local.or(global).map(|(candidate, _)| candidate)
}

pub(crate) fn prepare_window_unmap<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
) -> WindowUnmapPreparation {
    super::touch::cancel_surface(session, surface);
    super::gesture::cancel_surface(session, surface);
    super::pointer::prepare_unmap(session, surface);
    let focus = (session.wayland.focused_window.as_ref() == Some(surface)).then(|| {
        let output = mapped_managed_window(&session.wayland, surface)
            .and_then(|window| crate::wayland::window_output_name(&window));
        let preferred = session
            .field_config
            .close_restore_focus
            .then(|| {
                select_focus_successor(&session.wayland, &session.nodes, surface, output.as_deref())
            })
            .flatten();
        FocusSuccession {
            output,
            preferred,
            pan: session.field_config.close_restore_pan,
        }
    });
    WindowUnmapPreparation {
        surface: surface.clone(),
        focus,
    }
}

pub(crate) fn finish_window_unmap<D: SessionDriver>(
    session: &mut Session<D>,
    preparation: WindowUnmapPreparation,
) {
    let WindowUnmapPreparation { surface, focus } = preparation;
    session.wayland.managed_windows.remove(&surface);
    session.opening_origins.forget(&surface);
    if session.pending_pointer_warp.as_ref() == Some(&surface) {
        session.pending_pointer_warp = None;
    }
    session.window_open_animations.remove(&surface);
    session.fullscreen.remove(&surface);
    if session.maximize.remove(&surface)
        && let Some(output) = focus.as_ref().and_then(|focus| focus.output.as_deref())
    {
        let _ = session.cameras.apply_field_maximize(output, None);
    }
    session.render.fullscreen_textures.remove(&surface);
    super::cancel_grab_for_surface(session, &surface);
    crate::input::grab::forget_resize_anchor(&mut session.resize_anchor, &surface);
    super::closing::start(session, &surface);

    let Some(focus) = focus else {
        return;
    };
    if session
        .wayland
        .focused_window
        .as_ref()
        .is_some_and(|focused| focused != &surface)
    {
        return;
    }

    if !session.field_config.close_restore_focus {
        crate::window::clear_focus(&mut session.wayland);
        session
            .nodes
            .focus(None, session.start_time.elapsed().as_millis() as u64);
        return;
    }

    let revalidated = select_focus_successor(
        &session.wayland,
        &session.nodes,
        &surface,
        focus.output.as_deref(),
    );
    if revalidated != focus.preferred {
        eventline::debug!("focus: successor changed while window teardown completed");
    }
    let successor = revalidated
        .as_ref()
        .and_then(|surface| session.nodes.id_for_surface(surface));
    if let Some(id) = successor {
        let collapsed = session
            .nodes
            .record(id)
            .is_some_and(|record| record.collapsed);
        let serial = SERIAL_COUNTER.next_serial();
        match close_successor_action(collapsed, session.field_config.close_restore_nodes) {
            CloseSuccessorAction::FocusWindow => {
                if let Some(window) = session.nodes.record(id).map(|record| record.window.clone()) {
                    super::focus_window(session, &window, serial);
                }
                crate::nodes::pan_after_close_restore(session, id, focus.pan);
            }
            CloseSuccessorAction::FocusNode => {
                crate::window::clear_focus(&mut session.wayland);
                session
                    .nodes
                    .focus(Some(id), session.start_time.elapsed().as_millis() as u64);
                super::sync_keyboard_focus(session, serial);
                session.request_redraw();
            }
            CloseSuccessorAction::RestoreNode => {
                let _ = crate::nodes::restore_for_close(session, id, serial);
                crate::nodes::pan_after_close_restore(session, id, focus.pan);
            }
        }
    } else {
        crate::window::clear_focus(&mut session.wayland);
        session
            .nodes
            .focus(None, session.start_time.elapsed().as_millis() as u64);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{CloseSuccessorAction, close_successor_action, select_ordered_successor};

    fn output_lookup(
        outputs: &HashMap<&'static str, Option<&'static str>>,
        candidate: &&'static str,
    ) -> Option<Option<String>> {
        outputs
            .get(candidate)
            .map(|output| output.map(str::to_owned))
    }

    #[test]
    fn focus_successor_prefers_managed_stack_entry_on_closing_output() {
        let outputs = HashMap::from([
            ("closing", Some("DP-1")),
            ("global-top", Some("DP-2")),
            ("same-output", Some("DP-1")),
        ]);

        assert_eq!(
            select_ordered_successor(
                ["closing", "global-top", "same-output"],
                &"closing",
                Some("DP-1"),
                |candidate| output_lookup(&outputs, candidate),
                |candidate| match *candidate {
                    "global-top" => 200,
                    "same-output" => 100,
                    _ => 0,
                },
            ),
            Some("same-output")
        );
    }

    #[test]
    fn focus_successor_falls_back_to_most_recent_managed_window() {
        let outputs = HashMap::from([
            ("closing", Some("DP-1")),
            ("global-top", Some("DP-2")),
            ("global-bottom", Some("DP-3")),
        ]);

        assert_eq!(
            select_ordered_successor(
                ["closing", "global-top", "global-bottom"],
                &"closing",
                Some("DP-1"),
                |candidate| output_lookup(&outputs, candidate),
                |candidate| match *candidate {
                    "global-bottom" => 200,
                    "global-top" => 100,
                    _ => 0,
                },
            ),
            Some("global-bottom")
        );
    }

    #[test]
    fn focus_successor_skips_entries_that_are_no_longer_mapped() {
        let outputs = HashMap::from([
            ("closing", Some("DP-1")),
            ("stale", None),
            ("remaining", Some("DP-1")),
        ]);

        assert_eq!(
            select_ordered_successor(
                ["closing", "stale", "remaining"],
                &"closing",
                Some("DP-1"),
                |candidate| {
                    outputs
                        .get(candidate)
                        .and_then(|output| output.map(|output| Some(output.to_owned())))
                },
                |_| 0,
            ),
            Some("remaining")
        );
    }

    #[test]
    fn focus_successor_is_none_after_the_last_managed_window_closes() {
        assert_eq!(
            select_ordered_successor(
                ["closing"],
                &"closing",
                Some("DP-1"),
                |_| Some(Some("DP-1".to_owned())),
                |_| 0
            ),
            None
        );
    }

    #[test]
    fn collapsed_successor_stays_a_node_by_default() {
        assert_eq!(
            close_successor_action(true, false),
            CloseSuccessorAction::FocusNode
        );
    }

    #[test]
    fn collapsed_successor_restores_when_enabled() {
        assert_eq!(
            close_successor_action(true, true),
            CloseSuccessorAction::RestoreNode
        );
    }

    #[test]
    fn active_successor_focus_is_independent_of_node_restore_policy() {
        assert_eq!(
            close_successor_action(false, false),
            CloseSuccessorAction::FocusWindow
        );
        assert_eq!(
            close_successor_action(false, true),
            CloseSuccessorAction::FocusWindow
        );
    }
}
