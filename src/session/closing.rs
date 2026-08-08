use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;

use super::{Session, SessionDriver};

pub(crate) fn capture_surface<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
) -> bool {
    if session.render.window_close_animations.has_pending(surface) {
        return true;
    }
    let Some(window) = session
        .wayland
        .space
        .elements()
        .find(|window| {
            window
                .wl_surface()
                .is_some_and(|candidate| candidate.as_ref() == surface)
        })
        .cloned()
    else {
        return false;
    };
    capture_window(session, &window)
}

/// Captures a native toplevel immediately before its buffer-removal commit.
///
/// A close request is advisory: clients such as Firefox may keep the toplevel
/// mapped while they ask the user for confirmation.  Waiting for this
/// authoritative unmap boundary keeps the live client visible and interactive
/// if it rejects the request, while still preserving the last attached frame
/// for the close animation.
pub(crate) fn capture_native_toplevel_before_unmap<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &WlSurface,
) -> bool {
    let Some(window) = session
        .wayland
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface)
        })
        .cloned()
    else {
        return false;
    };
    capture_window(session, &window)
}

pub(crate) fn capture_window<D: SessionDriver>(session: &mut Session<D>, window: &Window) -> bool {
    capture_window_inner(session, window, false)
}

fn captures_before_client_decision(is_x11: bool) -> bool {
    is_x11
}

pub(crate) fn capture_before_close_request<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) -> bool {
    captures_before_client_decision(crate::xwayland::is_x11(window))
        && capture_window(session, window)
}

/// Captures a close-button target before pointer focus activates its X11
/// client. The snapshot remains provisional until the button is released and
/// is discarded if the pointer or touch leaves the control.
pub(crate) fn capture_close_control<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) -> bool {
    if !captures_before_client_decision(crate::xwayland::is_x11(window)) {
        return false;
    }
    capture_window_inner(session, window, true)
}

fn capture_window_inner<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    provisional: bool,
) -> bool {
    if crate::xwayland::is_override_redirect(window) {
        return false;
    }
    let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
        return false;
    };
    if session.render.window_close_animations.has_pending(&surface) {
        return true;
    }
    let Some(output) = output_for_window(&session.wayland, session.driver.primary_output(), window)
    else {
        return false;
    };
    let now = crate::frame_clock::monotonic_now();
    let Some(visual) = crate::presentation::window::window_visual_state(
        &session.wayland.space,
        &session.cameras,
        Some(&session.clusters),
        Some(&session.nodes),
        window,
        &output,
        &session.window_open_animations,
        &session.fullscreen,
        &session.maximize,
        &session.settings.decorations,
        &session.settings.font,
        now,
    ) else {
        return false;
    };
    let Some(stack_index) = session
        .wayland
        .space
        .elements()
        .position(|candidate| candidate == window)
    else {
        return false;
    };
    let focused = session.wayland.focused_window.as_ref() == Some(&surface);
    let chrome_visible =
        !session.fullscreen.suppresses_chrome(&surface) && !crate::xwayland::is_fullscreen(window);
    let maximized = session.maximize.contains(&surface);
    let initial_destination = closing_destination(
        window,
        visual,
        chrome_visible,
        &session.settings.decorations,
        &session.settings.font,
    );
    let anchor = if visual.presentation_space
        == crate::presentation::window::PresentationSpace::OutputLocal
    {
        crate::render::close::CloseAnchor::OutputLocal
    } else {
        crate::render::close::CloseAnchor::Windowed {
            world_geometry: visual.source_geometry,
            captured_camera_rect: visual.camera_rect,
        }
    };
    let retract_origin = session
        .opening_origins
        .active(&surface)
        .or_else(|| super::opening::fallback_origin(session, &output))
        .and_then(|origin| {
            let output_geometry = session.wayland.space.output_geometry(&output)?;
            Some(
                smithay::utils::Point::<f64, smithay::utils::Physical>::from((
                    origin.x - f64::from(output_geometry.loc.x),
                    origin.y - f64::from(output_geometry.loc.y),
                )),
            )
        });
    let metadata = crate::render::close::CloseSnapshotMetadata {
        output_name: output.name(),
        initial_destination,
        anchor,
        stack_index,
        start_alpha: visual.opening_alpha * session.window_rules.opacity(&surface),
        retract_origin,
        // The snapshot includes the complete compositor decoration, so the
        // closing scene must not synthesize a second border or clip pass.
        border: None,
        content_radius: 0.0,
        collapse_target: None,
    };
    let decorations = session.settings.decorations;
    let font = session.settings.font.clone();
    let render = &mut session.render;
    let capture = session.driver.with_renderer(|renderer| {
        let texture = crate::render::window_texture::capture_decorated(
            renderer,
            window,
            None,
            &decorations,
            &font,
            focused,
            chrome_visible,
            maximized,
            &mut render.titlebar_renderer,
            &mut render.window_decoration_renderer,
            &mut render.node_renderer,
            &mut render.ui_text,
        )?;
        render
            .window_close_animations
            .capture(window, texture, metadata)
    });
    match capture {
        Ok(captured) => {
            if captured && provisional {
                session
                    .render
                    .window_close_animations
                    .mark_provisional(surface);
            }
            captured
        }
        Err(err) => {
            eventline::warn!("window close: failed to capture window texture: {err}");
            false
        }
    }
}

fn closing_destination(
    window: &Window,
    visual: crate::presentation::window::WindowVisualState,
    chrome_visible: bool,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
) -> smithay::utils::Rectangle<i32, smithay::utils::Physical> {
    if !chrome_visible {
        return visual.animated_rect;
    }
    let opening_scale_y = if visual.presentation_rect.size.h > 0 {
        visual.animated_rect.size.h as f32 / visual.presentation_rect.size.h as f32
    } else {
        1.0
    };
    let decoration_scale = visual.zoom_scale * opening_scale_y.max(0.0);
    let chrome = crate::titlebar::WindowChrome::for_window(window, decorations, font);
    let border_width =
        crate::render::window_decoration::scaled_metric(chrome.border_width, decoration_scale);
    if chrome.has_server_titlebar() {
        let titlebar_height =
            crate::titlebar::rendered_metrics(&decorations.titlebars, font.size, decoration_scale)
                .height;
        crate::titlebar::DecorationLayout::new(
            visual.animated_rect,
            border_width,
            titlebar_height,
            &decorations.titlebars,
        )
        .outer
    } else {
        smithay::utils::Rectangle::new(
            (
                visual.animated_rect.loc.x - border_width,
                visual.animated_rect.loc.y - border_width,
            )
                .into(),
            (
                visual.animated_rect.size.w + border_width * 2,
                visual.animated_rect.size.h + border_width * 2,
            )
                .into(),
        )
    }
}

pub(crate) fn discard_close_control<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) -> bool {
    window.wl_surface().is_some_and(|surface| {
        session
            .render
            .window_close_animations
            .discard_provisional(surface.as_ref())
    })
}

pub(crate) fn start<D: SessionDriver>(session: &mut Session<D>, surface: &WlSurface) -> bool {
    session
        .render
        .window_close_animations
        .start(surface, crate::frame_clock::monotonic_now())
}

/// Captures and activates the close texture while the window is still mapped.
///
/// X11 client-owned controls do not expose close intent to the compositor, so
/// their first authoritative close boundary is `UnmapNotify`. Smithay invokes
/// Halley's XWM handler before clearing the associated Wayland surface; doing
/// both operations here makes the replacement texture active before the live
/// window leaves `Space`, matching Hyprland's unmap ordering.
pub(crate) fn capture_and_activate_before_unmap<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) -> bool {
    let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
        return false;
    };
    if session.render.window_close_animations.is_active(&surface) {
        return true;
    }
    if !capture_window(session, window) {
        return false;
    }
    let activated = start(session, &surface);
    debug_assert!(
        !activated || session.render.window_close_animations.is_active(&surface),
        "a successful close handoff must be active before the live window is unmapped"
    );
    if activated {
        session.request_redraw();
    }
    activated
}

pub(crate) fn mapped<D: SessionDriver>(session: &mut Session<D>, surface: &WlSurface) {
    session.render.window_close_animations.cancel(surface);
}

fn output_for_window(
    wayland: &crate::wayland::WaylandState,
    primary: &Output,
    window: &Window,
) -> Option<Output> {
    wayland
        .space
        .outputs()
        .find(|output| crate::wayland::window_is_on_output(window, output, primary))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::captures_before_client_decision;

    #[test]
    fn only_x11_captures_before_an_advisory_close_can_be_rejected() {
        assert!(captures_before_client_decision(true));
        assert!(!captures_before_client_decision(false));
    }
}
