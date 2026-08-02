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

pub(crate) fn capture_window<D: SessionDriver>(session: &mut Session<D>, window: &Window) -> bool {
    capture_window_inner(session, window, false)
}

/// Captures a close-button target before pointer focus activates its X11
/// client. The snapshot remains provisional until the button is released and
/// is discarded if the pointer or touch leaves the control.
pub(crate) fn capture_close_control<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) -> bool {
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
    let fullscreen_border_alpha = visual
        .fullscreen
        .map(|presentation| (1.0 - presentation.progress) as f32)
        .unwrap_or(1.0);
    let border = (fullscreen_border_alpha > 0.0).then(|| {
        let focused = session.wayland.focused_window.as_ref() == Some(&surface);
        crate::render::close::CloseBorder {
            width: ((session.settings.decorations.border_width_px as f64
                * f64::from(visual.zoom_scale))
            .round() as i32)
                .max(1),
            color: crate::render::window_border_color(&session.settings.decorations, focused)
                * fullscreen_border_alpha,
        }
    });
    let content_radius = crate::render::window_decoration::scaled_metric(
        session.settings.decorations.border_radius_px,
        visual.zoom_scale,
    ) as f32
        * fullscreen_border_alpha;
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
        initial_destination: visual.animated_rect,
        anchor,
        stack_index,
        start_alpha: visual.opening_alpha * session.window_rules.opacity(&surface),
        retract_origin,
        border,
        content_radius,
        collapse_target: None,
    };
    let animations = &mut session.render.window_close_animations;
    let capture = session
        .driver
        .with_renderer(|renderer| animations.capture(renderer, window, metadata));
    match capture {
        Ok(captured) => {
            if captured && provisional {
                animations.mark_provisional(surface);
            }
            captured
        }
        Err(err) => {
            eventline::warn!("window close: failed to capture window texture: {err}");
            false
        }
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
