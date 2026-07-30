use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;

use super::{Session, SessionDriver};
use crate::wayland::WaylandState;

struct FocusSuccession {
    output: Option<String>,
    preferred: Option<WlSurface>,
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
    closing: &WlSurface,
    closing_output: Option<&str>,
) -> Option<WlSurface> {
    select_ordered_successor(
        wayland.managed_windows.top_to_bottom().cloned(),
        closing,
        closing_output,
        |surface| {
            mapped_managed_window(wayland, surface)
                .map(|window| crate::wayland::window_output_name(&window))
        },
    )
}

fn select_ordered_successor<T>(
    candidates: impl IntoIterator<Item = T>,
    closing: &T,
    closing_output: Option<&str>,
    mut mapped_output: impl FnMut(&T) -> Option<Option<String>>,
) -> Option<T>
where
    T: Clone + Eq,
{
    let mut global = None;
    for candidate in candidates {
        if &candidate == closing {
            continue;
        }
        let Some(output) = mapped_output(&candidate) else {
            continue;
        };
        global.get_or_insert_with(|| candidate.clone());
        if closing_output.is_some_and(|closing_output| output.as_deref() == Some(closing_output)) {
            return Some(candidate);
        }
    }
    global
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
        let preferred = select_focus_successor(&session.wayland, surface, output.as_deref());
        FocusSuccession { output, preferred }
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
    session.maximize.remove(&surface);
    session.fullscreen_textures.remove(&surface);
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

    let revalidated = select_focus_successor(&session.wayland, &surface, focus.output.as_deref());
    if revalidated != focus.preferred {
        eventline::debug!("focus: successor changed while window teardown completed");
    }
    let successor = revalidated
        .as_ref()
        .and_then(|surface| mapped_managed_window(&session.wayland, surface));
    if let Some(window) = successor {
        crate::window::focus_and_raise(&mut session.wayland, &window);
        session.xwayland.raise_window(&window);
    } else {
        session.wayland.focused_window = None;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::select_ordered_successor;

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
            ),
            Some("same-output")
        );
    }

    #[test]
    fn focus_successor_falls_back_to_topmost_managed_window() {
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
            ),
            Some("global-top")
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
            ),
            Some("remaining")
        );
    }

    #[test]
    fn focus_successor_is_none_after_the_last_managed_window_closes() {
        assert_eq!(
            select_ordered_successor(["closing"], &"closing", Some("DP-1"), |_| Some(Some(
                "DP-1".to_owned()
            )),),
            None
        );
    }
}
