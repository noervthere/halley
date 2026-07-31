use super::*;

pub(super) fn enter_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    origin: FullscreenRequestOrigin,
) {
    set_external_fullscreen(session, surface, true, origin);
}

pub(super) fn leave_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    origin: FullscreenRequestOrigin,
) {
    set_external_fullscreen(session, surface, false, origin);
}

pub(super) fn set_external_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    fullscreen: bool,
    origin: FullscreenRequestOrigin,
) {
    // Maximize uses the fullscreen presentation machinery internally, but it
    // must remain EWMH maximize to the X11 client. Advertising both
    // _NET_WM_STATE_MAXIMIZED_* and _NET_WM_STATE_FULLSCREEN confuses Steam's
    // client-side decoration state, so its second button press never asks to
    // unmaximize. Hyprland keeps this same client/internal-state distinction.
    let update_client_fullscreen = origin != FullscreenRequestOrigin::Maximize;
    if fullscreen && origin != FullscreenRequestOrigin::Maximize {
        let mut maximize = surface
            .user_data()
            .get_or_insert_threadsafe(MaximizeFullscreen::default)
            .0
            .lock()
            .expect("X11 maximize state lock poisoned");
        maximize.active = false;
        maximize.restore = None;
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to clear maximized state for fullscreen: {err}");
        }
    }
    if session
        .xwayland
        .pending_windows
        .contains_key(&surface.window_id())
    {
        if update_client_fullscreen && let Err(err) = surface.set_fullscreen(fullscreen) {
            eventline::warn!("xwayland: failed to update pending fullscreen state: {err}");
        }
        return;
    }
    let Some(window) = window_for_surface(session, surface) else {
        return;
    };
    let now = crate::frame_clock::monotonic_now();
    let cluster_restore = window.wl_surface().and_then(|wl_surface| {
        crate::session::cluster_presentation_restore(session, wl_surface.as_ref(), now, fullscreen)
    });
    if !fullscreen && let Some(restore) = cluster_restore.as_ref() {
        session.fullscreen.override_restore_from_cluster(
            window
                .wl_surface()
                .expect("managed X11 windows have a Wayland surface")
                .as_ref(),
            restore.geometry,
            restore.output.clone(),
            restore.presentation_output,
        );
    }
    if compositor_fullscreen_should_raise(fullscreen, origin) && cluster_restore.is_none() {
        crate::window::raise_managed(&mut session.wayland, &window);
        session.xwayland.raise_window(&window);
    }
    if update_client_fullscreen && let Err(err) = surface.set_fullscreen(fullscreen) {
        eventline::warn!("xwayland: failed to update fullscreen state: {err}");
    }
    if session
        .fullscreen
        .external_desired_matches(&window, fullscreen)
    {
        eventline::debug!(
            "xwayland: coalesced fullscreen request xid={} fullscreen={fullscreen} \
             origin={origin:?}",
            surface.window_id()
        );
        crate::session::reconcile_pointer_constraints(session);
        return;
    }
    let opening = window.wl_surface().is_some_and(|wl_surface| {
        session
            .window_open_animations
            .is_animating(wl_surface.as_ref(), now)
    });
    let policy = external_presentation_policy(
        origin,
        opening,
        crate::session::has_active_pointer_confinement(session),
    );
    if policy == ExternalPresentationPolicy::Opening {
        preserve_opening_center(session, surface, &window);
    }
    let animate = policy == ExternalPresentationPolicy::Animated
        && capture_fullscreen_snapshot(session, &window, fullscreen);
    if policy == ExternalPresentationPolicy::Opening {
        request_opening_fullscreen(session, surface, &window, fullscreen, origin);
    } else if animate {
        let request = if fullscreen {
            session.fullscreen.request_external_animated(
                &mut session.wayland,
                &window,
                origin.presentation_origin(),
            )
        } else {
            session.fullscreen.unrequest_external_animated(&window)
        };
        match request {
            Some(ExternalTransactionRequest::Configure(geometry)) => {
                if let Err(err) = surface.configure(geometry) {
                    eventline::warn!("xwayland: failed to configure animated fullscreen: {err}");
                    remove_fullscreen_snapshot(session, &window);
                    settle_external_immediately(session, surface, &window, fullscreen, origin);
                } else {
                    eventline::debug!(
                        "xwayland: fullscreen policy xid={} fullscreen={fullscreen} \
                         origin={origin:?} policy=animated",
                        surface.window_id(),
                    );
                }
            }
            Some(ExternalTransactionRequest::NoChange) => {
                remove_fullscreen_snapshot(session, &window);
            }
            None => {
                remove_fullscreen_snapshot(session, &window);
                settle_external_immediately(session, surface, &window, fullscreen, origin);
            }
        }
    } else {
        let reason = presentation_policy_name(policy);
        eventline::debug!(
            "xwayland: fullscreen policy xid={} fullscreen={fullscreen} \
             origin={origin:?} policy={reason}",
            surface.window_id(),
        );
        settle_external_immediately(session, surface, &window, fullscreen, origin);
    }
    if fullscreen
        && let Some(restore) = cluster_restore
        && let Some(wl_surface) = window.wl_surface()
    {
        session.fullscreen.override_restore_from_cluster(
            wl_surface.as_ref(),
            restore.geometry,
            restore.output,
            restore.presentation_output,
        );
    }
    crate::session::reconcile_pointer_constraints(session);
}

pub(super) fn compositor_fullscreen_should_raise(
    fullscreen: bool,
    origin: FullscreenRequestOrigin,
) -> bool {
    fullscreen && origin == FullscreenRequestOrigin::Compositor
}

pub(super) fn preserve_opening_center<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
) {
    let Some(placement) = session
        .xwayland
        .opening_placements
        .get(&surface.window_id())
        .copied()
    else {
        return;
    };
    let centered = placement.centered(window.geometry().size);
    session.wayland.space.relocate_element(window, centered.loc);
}

pub(super) fn request_opening_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
    fullscreen: bool,
    origin: FullscreenRequestOrigin,
) {
    let now = crate::frame_clock::monotonic_now();
    let Some((output, current_bounds)) = opening_presentation_bounds(session, window, now) else {
        settle_external_immediately(session, surface, window, fullscreen, origin);
        return;
    };
    let request = if fullscreen {
        let restore = session
            .xwayland
            .opening_placements
            .get(&surface.window_id())
            .and_then(|placement| placement.restore_geometry());
        session.fullscreen.request_external_opening(
            &mut session.wayland,
            window,
            restore,
            origin.presentation_origin(),
        )
    } else {
        session.fullscreen.unrequest_external_opening(window)
    };
    match request {
        Some(ExternalTransactionRequest::Configure(geometry)) => {
            let target_bounds = opening_target_bounds(session, &output, geometry, fullscreen)
                .unwrap_or_else(|| Rectangle::new((0, 0).into(), geometry.size.to_physical(1)));
            let Some(wl_surface) = window.wl_surface() else {
                settle_external_immediately(session, surface, window, fullscreen, origin);
                return;
            };
            session.window_open_animations.retarget(
                wl_surface.as_ref(),
                now,
                current_bounds,
                target_bounds,
            );
            if let Err(err) = surface.configure(geometry) {
                eventline::warn!("xwayland: failed to configure opening fullscreen: {err}");
                settle_external_immediately(session, surface, window, fullscreen, origin);
            } else {
                eventline::debug!(
                    "xwayland: fullscreen policy xid={} fullscreen={fullscreen} policy=opening",
                    surface.window_id()
                );
            }
        }
        Some(ExternalTransactionRequest::NoChange) => {}
        None => settle_external_immediately(session, surface, window, fullscreen, origin),
    }
}

pub(super) fn opening_presentation_bounds<D: SessionDriver>(
    session: &Session<D>,
    window: &Window,
    now: std::time::Duration,
) -> Option<(Output, Rectangle<i32, smithay::utils::Physical>)> {
    let output = crate::wayland::window_output_name(window)
        .and_then(|name| {
            session
                .wayland
                .space
                .outputs()
                .find(|output| output.name() == name)
        })
        .or_else(|| crate::wayland::focus::selected_output(&session.wayland))
        .cloned()?;
    let output_geometry = session.wayland.space.output_geometry(&output)?;
    let presentation = crate::presentation::window::WindowPresentation::for_window(
        &session.wayland.space,
        &session.cameras,
        Some(&session.clusters),
        Some(&session.nodes),
        &session.window_open_animations,
        &session.fullscreen,
        &session.maximize,
        window,
        &output,
        now,
    )?;
    let local = presentation.visual_geometry().loc - output_geometry.loc;
    Some((
        output,
        Rectangle::new(
            local.to_physical(1),
            presentation.visual_geometry().size.to_physical(1),
        ),
    ))
}

pub(super) fn opening_target_bounds<D: SessionDriver>(
    session: &Session<D>,
    output: &Output,
    geometry: Rectangle<i32, Logical>,
    fullscreen: bool,
) -> Option<Rectangle<i32, smithay::utils::Physical>> {
    let output_geometry = session.wayland.space.output_geometry(output)?;
    let output_size = output_geometry.size.to_physical(1);
    if fullscreen {
        return Some(Rectangle::new((0, 0).into(), output_size));
    }
    let view = session.cameras.view(&output.name())?;
    Some(crate::render::camera_rect(
        geometry.to_physical(1),
        crate::presentation::camera::global_center(view.center, output_geometry),
        output_size,
        view.scale,
    ))
}

pub(super) fn presentation_policy_name(policy: ExternalPresentationPolicy) -> &'static str {
    match policy {
        ExternalPresentationPolicy::Initial => "initial",
        ExternalPresentationPolicy::Opening => "opening",
        ExternalPresentationPolicy::Confined => "confined",
        ExternalPresentationPolicy::Animated => "animated",
    }
}

pub(super) fn settle_external_immediately<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &X11Surface,
    window: &Window,
    fullscreen: bool,
    origin: FullscreenRequestOrigin,
) {
    let geometry = if fullscreen {
        session.fullscreen.request_external(
            &mut session.wayland,
            window,
            origin.presentation_origin(),
        )
    } else {
        session
            .fullscreen
            .unrequest_external(&mut session.wayland, window)
    };
    if let Some(geometry) = geometry
        && let Err(err) = surface.configure(geometry)
    {
        eventline::warn!("xwayland: failed to configure fullscreen window: {err}");
    }
}

pub(super) fn capture_fullscreen_snapshot<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    fullscreen: bool,
) -> bool {
    let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
        return false;
    };
    if !session
        .fullscreen
        .should_capture_external_snapshot(&surface, fullscreen)
    {
        return false;
    }
    let textures = &mut session.render.fullscreen_textures;
    match session.driver.with_renderer(|renderer| {
        textures.capture_previous(
            renderer,
            window,
            crate::render::fullscreen_texture::TextureTransitionOwner::Fullscreen,
        )
    }) {
        Ok(()) => true,
        Err(err) => {
            eventline::warn!("fullscreen: failed to capture X11 window texture: {err}");
            false
        }
    }
}

pub(super) fn remove_fullscreen_snapshot<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) {
    if let Some(surface) = window.wl_surface() {
        session.render.fullscreen_textures.remove(surface.as_ref());
    }
}

pub(crate) fn set_window_fullscreen<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
    fullscreen: bool,
) {
    let Some(surface) = window.x11_surface().cloned() else {
        return;
    };
    if fullscreen {
        enter_fullscreen(session, &surface, FullscreenRequestOrigin::Compositor);
    } else {
        leave_fullscreen(session, &surface, FullscreenRequestOrigin::Compositor);
    }
}

pub(crate) fn restore_maximized_window<D: SessionDriver>(
    session: &mut Session<D>,
    window: &Window,
) {
    if let Some(surface) = window.x11_surface().cloned() {
        restore_window(session, &surface);
    }
}

pub(super) fn restore_window<D: SessionDriver>(session: &mut Session<D>, surface: &X11Surface) {
    if session
        .xwayland
        .pending_windows
        .contains_key(&surface.window_id())
    {
        if let Some(state) = surface.user_data().get::<MaximizeFullscreen>() {
            let mut state = state.0.lock().expect("X11 maximize state lock poisoned");
            state.active = false;
            state.restore = None;
        }
        if let Err(err) = surface.set_maximized(false) {
            eventline::warn!("xwayland: failed to clear pending maximized state: {err}");
        }
        if let Err(err) = surface.set_fullscreen(false) {
            eventline::warn!("xwayland: failed to clear pending fullscreen state: {err}");
        }
        return;
    }
    if let Err(err) = surface.set_maximized(false) {
        eventline::warn!("xwayland: failed to clear maximized state: {err}");
    }
    let restore = surface
        .user_data()
        .get::<MaximizeFullscreen>()
        .and_then(|state| {
            let mut state = state.0.lock().expect("X11 maximize state lock poisoned");
            let restore = state.active.then(|| state.restore.clone()).flatten();
            state.active = false;
            state.restore = None;
            restore
        });
    if let Some(restore) = restore {
        let window = window_for_surface(session, surface);
        let fullscreen_active = window
            .as_ref()
            .is_some_and(|window| session.fullscreen.external_desired_matches(window, true));
        leave_fullscreen(session, surface, FullscreenRequestOrigin::Maximize);
        if !fullscreen_active && let Some(window) = window {
            if let Some(output) = restore
                .output
                .as_deref()
                .and_then(|name| output_named(session, name))
            {
                crate::wayland::set_window_output(&window, &output);
            }
            session
                .wayland
                .space
                .relocate_element(&window, restore.geometry.loc);
            if let Err(err) = surface.configure(restore.geometry) {
                eventline::warn!("xwayland: failed to restore maximized geometry: {err}");
            }
        }
    }
}

pub(super) fn resize_handle(edge: ResizeEdge) -> crate::input::grab::ResizeHandle {
    use crate::input::grab::ResizeHandle;

    match edge {
        ResizeEdge::Top => ResizeHandle::Top,
        ResizeEdge::Bottom => ResizeHandle::Bottom,
        ResizeEdge::Left => ResizeHandle::Left,
        ResizeEdge::TopLeft => ResizeHandle::TopLeft,
        ResizeEdge::BottomLeft => ResizeHandle::BottomLeft,
        ResizeEdge::Right => ResizeHandle::Right,
        ResizeEdge::TopRight => ResizeHandle::TopRight,
        ResizeEdge::BottomRight => ResizeHandle::BottomRight,
    }
}

pub(super) fn evdev_button(x11_button: u32) -> Option<u32> {
    match x11_button {
        1 => Some(0x110),
        2 => Some(0x112),
        3 => Some(0x111),
        _ => None,
    }
}

pub(crate) fn reconfigure_fullscreen(windows: Vec<(Window, Rectangle<i32, Logical>)>) {
    for (window, geometry) in windows {
        let Some(surface) = window.x11_surface() else {
            continue;
        };
        if let Err(err) = surface.configure(geometry) {
            eventline::warn!("xwayland: failed to reconfigure fullscreen output: {err}");
        }
    }
}

pub(crate) fn configure_window(window: &Window, geometry: Rectangle<i32, Logical>) {
    let Some(surface) = window.x11_surface() else {
        return;
    };
    let geometry = Rectangle::new(
        geometry.loc,
        super::configure::constrain_surface_size(surface, geometry.size),
    );
    if let Err(err) = surface.configure(geometry) {
        eventline::warn!("xwayland: failed to configure window geometry: {err}");
    }
}
