use smithay::desktop::{LayerSurface, Window};
use smithay::utils::Serial;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::KeyboardInteractivity;

use super::{Session, SessionDriver};

#[derive(Clone, Copy)]
enum FocusOrigin {
    Explicit,
    Pointer,
    Hover,
}

fn should_raise(origin: FocusOrigin, raise_on_click: bool) -> bool {
    match origin {
        FocusOrigin::Explicit => true,
        FocusOrigin::Pointer => raise_on_click,
        FocusOrigin::Hover => false,
    }
}

pub(crate) fn focus_layer<D: SessionDriver>(
    session: &mut Session<D>,
    layer: Option<LayerSurface>,
    serial: Serial,
) {
    crate::wayland::focus::select_layer(&mut session.wayland, layer);
    super::sync_keyboard_focus(session, serial);
}

/// Focus used by explicit activation and mapping policy. These paths retain
/// their established behavior and always raise.
pub(crate) fn focus_window<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    serial: Serial,
) {
    focus_window_with_raise(
        session,
        window,
        serial,
        should_raise(FocusOrigin::Explicit, session.input.raise_on_click),
    );
}

/// Pointer-initiated focus follows the configured click policy. Hover uses a
/// separate entry point below and never raises.
pub(crate) fn focus_window_from_pointer<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    serial: Serial,
) {
    focus_window_with_raise(
        session,
        window,
        serial,
        should_raise(FocusOrigin::Pointer, session.input.raise_on_click),
    );
}

fn focus_window_with_raise<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    serial: Serial,
    raise: bool,
) {
    crate::window::focus(&mut session.wayland, window, raise);
    if let Some(surface) = window.wl_surface() {
        session.nodes.focus_surface(
            surface.as_ref(),
            session.start_time.elapsed().as_millis() as u64,
        );
    }
    if raise {
        session.xwayland.raise_window(window);
    }
    super::sync_keyboard_focus(session, serial);
}

pub(super) fn focus_node_from_hover<D: SessionDriver>(
    session: &mut Session<D>,
    id: halley_core::field::NodeId,
    output: &smithay::output::Output,
    serial: Serial,
) {
    if session.input.focus_mode != halley_config::FocusMode::Hover
        || hover_is_blocked(session)
        || session.nodes.focused() == Some(id)
        || !session
            .nodes
            .record(id)
            .is_some_and(|record| record.attached && record.collapsed)
    {
        return;
    }
    crate::wayland::focus::select_output(&mut session.wayland, output);
    crate::window::clear_focus(&mut session.wayland);
    session
        .nodes
        .focus(Some(id), session.start_time.elapsed().as_millis() as u64);
    super::sync_keyboard_focus(session, serial);
    session.request_redraw();
}

pub(super) fn update_hover<D: SessionDriver>(
    session: &mut Session<D>,
    route: &crate::input::pointer::PointerRoute,
    serial: Serial,
) {
    if session.input.focus_mode != halley_config::FocusMode::Hover || hover_is_blocked(session) {
        return;
    }

    crate::wayland::focus::select_output(&mut session.wayland, &route.output);
    match &route.target {
        crate::input::pointer::PointerTarget::Window(window) => {
            let surface = window.wl_surface().map(std::borrow::Cow::into_owned);
            if surface.is_some() && session.wayland.focused_window != surface {
                focus_window_with_raise(
                    session,
                    window,
                    serial,
                    should_raise(FocusOrigin::Hover, session.input.raise_on_click),
                );
            }
        }
        crate::input::pointer::PointerTarget::Layer(layer)
            if layer.cached_state().keyboard_interactivity == KeyboardInteractivity::OnDemand =>
        {
            if session
                .wayland
                .focused_layer
                .as_ref()
                .is_none_or(|focused| focused != layer)
            {
                focus_layer(session, Some(layer.clone()), serial);
            }
        }
        crate::input::pointer::PointerTarget::Layer(_)
        | crate::input::pointer::PointerTarget::Background => {}
    }
}

fn hover_is_blocked<D: SessionDriver>(session: &Session<D>) -> bool {
    let pointer_grabbed = session
        .seat
        .get_pointer()
        .is_some_and(|pointer| pointer.is_grabbed());
    let exclusive_layer = matches!(
        crate::wayland::focus::current(
            &session.wayland,
            &session.fullscreen,
            crate::frame_clock::monotonic_now(),
        ),
        Some(crate::wayland::focus::KeyboardFocus::ExclusiveLayer(_))
    );

    pointer_grabbed
        || !matches!(session.grab, crate::input::grab::Grab::None)
        || session.capture.is_active()
        || super::pointer::has_active_constraint(session)
        || exclusive_layer
}

#[cfg(test)]
mod tests {
    use super::{FocusOrigin, should_raise};

    #[test]
    fn only_pointer_clicks_consult_raise_on_click() {
        assert!(should_raise(FocusOrigin::Explicit, false));
        assert!(should_raise(FocusOrigin::Explicit, true));
        assert!(!should_raise(FocusOrigin::Pointer, false));
        assert!(should_raise(FocusOrigin::Pointer, true));
        assert!(!should_raise(FocusOrigin::Hover, false));
        assert!(!should_raise(FocusOrigin::Hover, true));
    }
}
