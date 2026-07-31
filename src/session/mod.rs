use std::borrow::Cow;
use std::ffi::OsStr;
use std::time::Duration;

use calloop::LoopHandle;
use calloop::timer::{TimeoutAction, Timer};
use halley_config::Action;
use halley_core::camera::Camera;
use smithay::utils::Rectangle;
use smithay::wayland::seat::WaylandFocus;

use crate::wayland;

mod autostart;
pub(crate) mod closing;
mod cursor;
mod focus;
pub(crate) mod gesture;
mod input;
mod lifecycle;
pub(crate) mod opening;
pub(crate) mod output;
pub(crate) mod pointer;
mod protocol;
mod spawn;
mod state;
pub(crate) mod touch;

pub mod environment;
pub mod tty;
pub mod winit;

pub(crate) use focus::focus_window;
pub use state::{Session, SessionDriver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionControl {
    Continue,
    Quit,
    CloseFocusedWindow,
    Screenshot,
    ToggleFullscreen,
    ToggleFieldMaximize,
    ToggleState,
    Apogee,
    FocusCycle(halley_config::FocusCycleDirection),
    BearingsShow,
    BearingsToggle,
    ClusterMode,
    ClusterLayoutCycle,
    ClusterSlot(u8),
    ClusterTileFocus(halley_config::ClusterDirection),
    ClusterTileSwap(halley_config::ClusterDirection),
}

#[derive(Clone, Copy)]
struct SpawnContext<'a> {
    socket_name: &'a OsStr,
    x11_display: Option<&'a OsStr>,
    cursor_theme: &'a str,
    cursor_size: u8,
    environment: &'a environment::LaunchEnvironment,
}

/// Interprets every configured action once for both session backends.
/// Backends provide the camera selected by their own output routing and
/// translate the returned quit request into their loop's native mechanism.
fn dispatch_action(
    action: Action,
    terminal_command: Option<&str>,
    spawn_context: SpawnContext<'_>,
    camera: Option<&mut Camera>,
    zoom: &halley_config::Zoom,
) -> SessionControl {
    match action {
        Action::Quit => return SessionControl::Quit,
        Action::CloseFocusedWindow => return SessionControl::CloseFocusedWindow,
        Action::ToggleFullscreen => return SessionControl::ToggleFullscreen,
        Action::ToggleFieldMaximize => return SessionControl::ToggleFieldMaximize,
        Action::ToggleState => return SessionControl::ToggleState,
        Action::Apogee => return SessionControl::Apogee,
        Action::FocusCycle(direction) => return SessionControl::FocusCycle(direction),
        Action::ClusterMode => return SessionControl::ClusterMode,
        Action::ClusterLayoutCycle => return SessionControl::ClusterLayoutCycle,
        Action::ClusterSlot(slot) => return SessionControl::ClusterSlot(slot),
        Action::ClusterTileFocus(direction) => return SessionControl::ClusterTileFocus(direction),
        Action::ClusterTileSwap(direction) => return SessionControl::ClusterTileSwap(direction),
        Action::BearingsShow => return SessionControl::BearingsShow,
        Action::BearingsToggle => return SessionControl::BearingsToggle,
        Action::OpenTerminal => match terminal_command {
            Some(command) => spawn::spawn_detached(
                command,
                spawn_context.socket_name,
                spawn_context.x11_display,
                spawn_context.cursor_theme,
                spawn_context.cursor_size,
                spawn_context.environment,
            ),
            None => eventline::warn!("keybinds: no terminal configured or found on PATH"),
        },
        Action::ZoomOut => {
            if let Some(camera) = camera {
                crate::input::zoom::zoom_out(camera, zoom);
            }
        }
        Action::ZoomIn => {
            if let Some(camera) = camera {
                crate::input::zoom::zoom_in(camera, zoom);
            }
        }
        Action::ZoomReset => {
            if let Some(camera) = camera {
                camera.reset_zoom_target();
            }
        }
        Action::Screenshot => return SessionControl::Screenshot,
        Action::Spawn(command) => spawn::spawn_detached(
            &command,
            spawn_context.socket_name,
            spawn_context.x11_display,
            spawn_context.cursor_theme,
            spawn_context.cursor_size,
            spawn_context.environment,
        ),
    }
    SessionControl::Continue
}

pub(crate) fn cancel_grab_for_surface<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    if crate::input::grab::belongs_to_surface(&session.grab, surface) {
        session.grab = crate::input::grab::Grab::None;
        session.cursor.set_override(None);
        crate::input::grab::forget_resize_anchor(&mut session.resize_anchor, surface);
    }
}

pub(crate) use lifecycle::{finish_window_unmap, prepare_window_unmap};

fn install_node_decay_timer<D: SessionDriver>(
    handle: &LoopHandle<'_, Session<D>>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            |_, _, session| {
                crate::nodes::tick_decay(session);
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )
        .map(|_| ())
        .map_err(Into::into)
}

fn install_apogee_preview_timer<D: SessionDriver>(
    handle: &LoopHandle<'_, Session<D>>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(8)),
            |_, _, session| {
                if session.apogee.take_live_redraw_due(
                    crate::frame_clock::monotonic_now(),
                    session.apogee_config.preview_max_fps,
                ) {
                    session.request_redraw();
                }
                TimeoutAction::ToDuration(Duration::from_millis(8))
            },
        )
        .map(|_| ())
        .map_err(Into::into)
}

fn install_overlay_timer<D: SessionDriver>(
    handle: &LoopHandle<'_, Session<D>>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(50)),
            |_, _, session| {
                if session.overlays.wakeup(crate::frame_clock::monotonic_now()) {
                    session.request_redraw();
                }
                TimeoutAction::ToDuration(Duration::from_millis(50))
            },
        )
        .map(|_| ())
        .map_err(Into::into)
}

pub(crate) fn reconcile_pointer_constraints<D: SessionDriver>(session: &mut Session<D>) {
    pointer::reconcile_state(session);
}

pub(crate) fn has_active_pointer_confinement<D: SessionDriver>(session: &Session<D>) -> bool {
    pointer::has_active_confinement(session)
}

pub(crate) fn cursor_visible<D: SessionDriver>(session: &Session<D>) -> bool {
    pointer::cursor_visible(session)
}

pub(crate) fn note_pointer_activity<D: SessionDriver>(session: &mut Session<D>) {
    session.cursor_policy.pointer_activity();
}

pub(crate) fn warp_pointer_to_window_center<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
) -> bool {
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| {
            crate::wayland::window_is_on_output(window, output, session.driver.primary_output())
        })
        .cloned()
    else {
        return false;
    };
    let Some(presentation) = crate::presentation::window::WindowPresentation::for_window(
        &session.wayland.space,
        &session.cameras,
        Some(&session.clusters),
        Some(&session.nodes),
        &session.window_open_animations,
        &session.fullscreen,
        &session.maximize,
        window,
        &output,
        crate::frame_clock::monotonic_now(),
    ) else {
        return false;
    };
    let geometry = presentation.visual_geometry();
    let center = (
        f64::from(geometry.loc.x) + f64::from(geometry.size.w) * 0.5,
        f64::from(geometry.loc.y) + f64::from(geometry.size.h) * 0.5,
    );
    pointer::release_for_compositor_warp(session);
    session.pointer.set_position(center);
    session.cursor_policy.pointer_activity();
    pointer::update_client_state(session, session.start_time.elapsed().as_millis() as u32);
    session.request_output_redraw(&output);
    true
}

fn toggle_focused_fullscreen<D: SessionDriver>(session: &mut Session<D>, output: Option<&str>) {
    let id = match output {
        Some(output) => session.nodes.focused_on_output(output),
        None => session.nodes.focused(),
    };
    let Some(record) = id
        .and_then(|id| session.nodes.record(id))
        .filter(|record| !record.collapsed)
        .cloned()
    else {
        return;
    };
    let focused = record.surface;
    let window = record.window;
    let output_name =
        crate::wayland::window_output_name(&window).unwrap_or_else(|| record.output.clone());
    let now = crate::frame_clock::monotonic_now();
    let target_output = session
        .wayland
        .space
        .outputs()
        .find(|candidate| candidate.name() == output_name)
        .cloned();
    cancel_grab_for_surface(session, &focused);
    let entering = !session.fullscreen.is_fullscreen_or_pending(&focused);
    if entering {
        displace_fullscreen_on_output(session, &output_name, &focused);
    }
    let field_output_rect = (entering && session.maximize.contains(&focused))
        .then(|| {
            target_output
                .as_ref()
                .and_then(|output| presented_window_rect(session, &window, output, now))
        })
        .flatten();
    let field_restore = entering
        .then(|| session.maximize.take_output_restore(&output_name))
        .flatten();
    let field_geometry = field_restore
        .as_ref()
        .filter(|restore| restore.surface == focused)
        .and_then(|_| session.wayland.space.element_geometry(&window));
    if let Some(restore) = field_restore.as_ref() {
        session.render.fullscreen_textures.remove(&restore.surface);
        let camera_handoff = restore.surface == focused
            && session
                .cameras
                .handoff_field_maximize_to_fullscreen(&output_name);
        if !camera_handoff {
            let _ = session.cameras.apply_field_maximize(&output_name, None);
        }
        if restore.surface == focused {
            session
                .wayland
                .space
                .relocate_element(&window, restore.geometry.loc);
        } else {
            configure_field_geometry(session, restore);
        }
    }
    if let Some(toplevel) = window.toplevel() {
        if entering {
            session
                .fullscreen
                .request_compositor(&mut session.wayland, toplevel);
        } else {
            session.fullscreen.unrequest(&session.wayland, toplevel);
        }
    } else {
        crate::xwayland::set_window_fullscreen(session, &window, entering);
    }
    if let (Some(restore), Some(field_geometry)) = (field_restore, field_geometry) {
        session.fullscreen.override_restore_from_field(
            &focused,
            restore.geometry,
            restore.output,
            field_geometry,
            field_output_rect,
        );
    }
    pointer::reconcile_state(session);
    session.request_redraw();
}

fn displace_fullscreen_on_output<D: SessionDriver>(
    session: &mut Session<D>,
    output: &str,
    except: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) {
    let occupants = session.fullscreen.occupants_on_output(output, except);
    for (surface, origin) in occupants {
        let restore = session.fullscreen.restore_location(&surface);
        let window = session
            .nodes
            .id_for_surface(&surface)
            .and_then(|id| session.nodes.record(id))
            .map(|record| record.window.clone())
            .or_else(|| {
                session
                    .wayland
                    .space
                    .elements()
                    .find(|window| {
                        window
                            .wl_surface()
                            .is_some_and(|candidate| candidate.as_ref() == &surface)
                    })
                    .cloned()
            });
        if let Some(window) = window {
            if let Some(toplevel) = window.toplevel() {
                session.fullscreen.unrequest(&session.wayland, toplevel);
            } else if origin == crate::wayland::fullscreen::FullscreenOrigin::Maximize {
                crate::xwayland::restore_maximized_window(session, &window);
            } else {
                crate::xwayland::set_window_fullscreen(session, &window, false);
            }
            if let Some((location, restore_output)) = restore {
                if let Some(restore_output) = restore_output.as_deref().and_then(|name| {
                    session
                        .wayland
                        .space
                        .outputs()
                        .find(|candidate| candidate.name() == name)
                        .cloned()
                }) {
                    crate::wayland::set_window_output(&window, &restore_output);
                }
                session.wayland.space.relocate_element(&window, location);
            }
        }
        session.fullscreen.remove(&surface);
        session.render.fullscreen_textures.remove(&surface);
    }
}

fn toggle_focused_field_maximize<D: SessionDriver>(session: &mut Session<D>, output: Option<&str>) {
    let id = match output {
        Some(output) => session.nodes.focused_on_output(output),
        None => session.nodes.focused(),
    };
    let Some(record) = id
        .and_then(|id| session.nodes.record(id))
        .filter(|record| !record.collapsed)
        .cloned()
    else {
        return;
    };
    let _ = toggle_field_maximize(session, record);
}

pub(crate) fn set_surface_field_maximized<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    maximized: bool,
) -> bool {
    if session.maximize.contains(surface) == maximized {
        return false;
    }
    let Some(record) = session
        .nodes
        .id_for_surface(surface)
        .and_then(|id| session.nodes.record(id))
        .filter(|record| !record.collapsed)
        .cloned()
    else {
        return false;
    };
    toggle_field_maximize(session, record)
}

fn toggle_field_maximize<D: SessionDriver>(
    session: &mut Session<D>,
    record: crate::nodes::NodeRecord,
) -> bool {
    let output_name =
        crate::wayland::window_output_name(&record.window).unwrap_or_else(|| record.output.clone());
    let Some(target_output) = session
        .wayland
        .space
        .outputs()
        .find(|candidate| candidate.name() == output_name)
        .cloned()
    else {
        return false;
    };
    let Some(output_geometry) = session.wayland.space.output_geometry(&target_output) else {
        return false;
    };
    let usable = smithay::desktop::layer_map_for_output(&target_output).non_exclusive_zone();
    let inset = (session.field_config.gap.ceil() as i32)
        .saturating_add(session.decorations.border_width_px);
    let target = Rectangle::new(
        output_geometry.loc
            + usable.loc
            + smithay::utils::Point::<i32, smithay::utils::Logical>::from((inset, inset)),
        (
            usable.size.w.saturating_sub(inset.saturating_mul(2)).max(1),
            usable.size.h.saturating_sub(inset.saturating_mul(2)).max(1),
        )
            .into(),
    );
    let inherited_restore = session.fullscreen.restore_placement(&record.surface);
    let Some(restore_geometry) = inherited_restore
        .as_ref()
        .map(|(geometry, _)| *geometry)
        .or_else(|| session.wayland.space.element_geometry(&record.window))
    else {
        return false;
    };
    let restore_output = inherited_restore
        .and_then(|(_, output)| output)
        .unwrap_or_else(|| output_name.clone());

    let now = crate::frame_clock::monotonic_now();
    let handoff_output_rect = session
        .fullscreen
        .is_fullscreen_or_pending(&record.surface)
        .then(|| presented_window_rect(session, &record.window, &target_output, now))
        .flatten();
    if session.maximize.animations_enabled() {
        let textures = &mut session.render.fullscreen_textures;
        let capture = session.driver.with_renderer(|renderer| {
            textures.capture_previous(
                renderer,
                &record.window,
                crate::render::fullscreen_texture::TextureTransitionOwner::Maximize,
            )
        });
        if let Err(err) = capture {
            eventline::warn!("maximize: failed to capture previous window texture: {err}");
        }
    }
    cancel_grab_for_surface(session, &record.surface);
    let entering = !session.maximize.contains(&record.surface);
    if entering {
        displace_fullscreen_on_output(session, &output_name, &record.surface);
    }
    // Maximizing straight out of fullscreen hands the whole travel to the
    // maximize animation: it eases from the rect the window occupies right now
    // down to the maximized rect. Letting fullscreen arm its own exit
    // transition instead would run two timelines at once, and fullscreen wins
    // in `window_visual_state`, so the shrink toward the small windowed rect is
    // what you would see until it retired and the maximize track took over
    // mid-flight.
    let handoff_geometry = session
        .fullscreen
        .is_fullscreen_or_pending(&record.surface)
        .then(|| {
            let geometry = session.wayland.space.element_geometry(&record.window);
            session
                .cameras
                .handoff_fullscreen_to_field_maximize(&output_name);
            if let Some(toplevel) = record.window.toplevel() {
                session.fullscreen.retire_for_handoff(toplevel);
            } else {
                crate::xwayland::set_window_fullscreen(session, &record.window, false);
                session.fullscreen.remove(&record.surface);
            }
            geometry
        })
        .flatten();
    if handoff_geometry.is_none() {
        let camera_progress = session.maximize.camera_progress(&target_output, now);
        session
            .cameras
            .clear_field_maximize_handoff(&output_name, camera_progress);
    }
    let change = session.maximize.toggle(
        &target_output,
        record.surface.clone(),
        restore_geometry,
        restore_output,
        target,
        now,
    );
    if let Some(handoff_geometry) = handoff_geometry {
        session.maximize.override_windowed_from_fullscreen(
            &record.surface,
            handoff_geometry,
            handoff_output_rect,
        );
    }
    if let Some(displaced) = change.displaced.as_ref() {
        session
            .render
            .fullscreen_textures
            .remove(&displaced.surface);
        configure_field_geometry(session, displaced);
    }
    configure_field_geometry(
        session,
        &crate::presentation::maximize::FieldRestore {
            surface: record.surface,
            geometry: change.geometry,
            output: change.output,
        },
    );
    pointer::reconcile_state(session);
    session.request_redraw();
    true
}

fn presented_window_rect<D: SessionDriver>(
    session: &Session<D>,
    window: &smithay::desktop::Window,
    output: &smithay::output::Output,
    now: std::time::Duration,
) -> Option<Rectangle<i32, smithay::utils::Physical>> {
    crate::presentation::window::window_visual_state(
        &session.wayland.space,
        &session.cameras,
        Some(&session.clusters),
        Some(&session.nodes),
        window,
        output,
        &session.window_open_animations,
        &session.fullscreen,
        &session.maximize,
        now,
    )
    .map(|visual| visual.animated_rect)
}

pub(crate) fn configure_field_geometry<D: SessionDriver>(
    session: &mut Session<D>,
    request: &crate::presentation::maximize::FieldRestore,
) {
    let Some(window) = session
        .nodes
        .id_for_surface(&request.surface)
        .and_then(|id| session.nodes.record(id))
        .map(|record| record.window.clone())
    else {
        return;
    };
    if let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == request.output)
        .cloned()
    {
        crate::wayland::set_window_output(&window, &output);
    }
    session
        .wayland
        .space
        .relocate_element(&window, request.geometry.loc);
    if let Some(toplevel) = window.toplevel() {
        let maximized = session.maximize.contains(&request.surface);
        toplevel.with_pending_state(|pending| {
            pending.size = Some(request.geometry.size);
            pending.bounds = Some(request.geometry.size);
            if maximized {
                pending.states.set(
                    smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized,
                );
            } else {
                pending.states.unset(
                    smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized,
                );
            }
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    } else {
        crate::xwayland::configure_window(&window, request.geometry);
    }
}

pub(crate) fn sync_keyboard_focus<D: SessionDriver>(
    session: &mut Session<D>,
    serial: smithay::utils::Serial,
) {
    if session.session_lock.active() {
        let focused = session
            .session_lock
            .focused_surface()
            .map(crate::xwayland::KeyboardFocusTarget::from);
        let keyboard = session
            .seat
            .get_keyboard()
            .expect("keyboard capability added at seat setup");
        pointer::prepare_keyboard_focus_change(session, None);
        keyboard.set_focus(session, focused, serial);
        return;
    }
    wayland::focus::refresh_selected_layer(&mut session.wayland);
    if let Some(surface) = session.wayland.focused_window.clone() {
        session
            .nodes
            .focus_surface(&surface, session.start_time.elapsed().as_millis() as u64);
    } else if session
        .nodes
        .focused()
        .and_then(|id| session.nodes.record(id))
        .is_some_and(|record| !record.collapsed)
    {
        session.nodes.focus(None, 0);
    }
    let focused = wayland::focus::current(
        &session.wayland,
        &session.fullscreen,
        crate::frame_clock::monotonic_now(),
    )
    .and_then(|focus| match focus {
        wayland::focus::KeyboardFocus::Window(surface) => session
            .wayland
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|candidate| candidate.as_ref() == &surface)
            })
            .and_then(crate::xwayland::KeyboardFocusTarget::for_window)
            .or_else(|| Some(surface.into())),
        wayland::focus::KeyboardFocus::ExclusiveLayer(surface)
        | wayland::focus::KeyboardFocus::OnDemandLayer(surface) => Some(surface.into()),
    });
    let keyboard = session
        .seat
        .get_keyboard()
        .expect("keyboard capability added at seat setup");
    let next_constraint_root = focused
        .as_ref()
        .and_then(|target| target.wl_surface().map(Cow::into_owned))
        .filter(|surface| session.wayland.focused_window.as_ref() == Some(surface));
    pointer::prepare_keyboard_focus_change(session, next_constraint_root.as_ref());
    keyboard.set_focus(session, focused, serial);
    pointer::reconcile_state(session);
}

pub(crate) fn begin_window_resize<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    handle: crate::input::grab::ResizeHandle,
    button: u32,
    cursor: halley_core::field::Vec2,
    serial: smithay::utils::Serial,
) -> bool {
    if window
        .wl_surface()
        .is_some_and(|surface| session.maximize.contains(surface.as_ref()))
    {
        return false;
    }
    let Some(start_rect) = session.wayland.space.element_geometry(window) else {
        return false;
    };
    focus::focus_window_from_pointer(session, window, serial);
    session.grab = crate::input::grab::Grab::ResizeWindow(crate::input::grab::ResizeState {
        window: window.clone(),
        handle,
        button,
        start_rect,
        start_cursor: cursor,
    });
    session.resize_anchor = window.toplevel().map(|_| crate::input::grab::ResizeAnchor {
        window: window.clone(),
        handle,
        phase: crate::input::grab::ResizePhase::Ongoing,
        last_configure: None,
        last_size: start_rect.size,
    });
    true
}

pub(crate) fn begin_pointer_resize<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    handle: crate::input::grab::ResizeHandle,
    button: u32,
) -> bool {
    let Some(route) = pointer::route_client(session) else {
        return false;
    };
    if !matches!(
        route.target,
        crate::input::pointer::PointerTarget::Window(ref routed) if routed == window
    ) {
        return false;
    }
    let cursor = halley_core::field::Vec2 {
        x: route.location.x as f32,
        y: route.location.y as f32,
    };
    begin_window_resize(
        session,
        window,
        handle,
        button,
        cursor,
        smithay::utils::SERIAL_COUNTER.next_serial(),
    )
}

/// Starts Halley's existing interactive window move from the current pointer
/// route. Callers must validate any protocol serial before reaching here.
///
/// Keeping grab construction here gives compositor key bindings, xdg-shell,
/// and XWayland one movement implementation without coupling protocol handlers
/// to the input dispatch loop.
pub(crate) fn begin_pointer_move<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    serial: smithay::utils::Serial,
) -> bool {
    if !crate::window::accepts_compositor_grab(window)
        || window.wl_surface().is_some_and(|surface| {
            session
                .fullscreen
                .is_fullscreen_or_pending(surface.as_ref())
        })
    {
        return false;
    }
    let Some(route) = pointer::route_client(session) else {
        return false;
    };
    if !matches!(
        route.target,
        crate::input::pointer::PointerTarget::Window(ref routed) if routed == window
    ) {
        return false;
    }
    let world = halley_core::field::Vec2 {
        x: route.location.x as f32,
        y: route.location.y as f32,
    };
    let Some(window_location) = session.wayland.space.element_location(window) else {
        return false;
    };
    let Some(camera) = session.cameras.get(&route.output.name()) else {
        return false;
    };
    let scale = crate::input::zoom::scale(camera);

    let restore = window
        .wl_surface()
        .and_then(|surface| session.maximize.take_restore(surface.as_ref()));
    let was_maximized = restore.is_some();
    if let Some(restore) = restore.as_ref() {
        session.render.fullscreen_textures.remove(&restore.surface);
        configure_field_geometry(session, restore);
        let _ = session
            .cameras
            .apply_field_maximize(&route.output.name(), None);
    }

    let screen_offset = if was_maximized {
        let visual = route.visual_geometry.unwrap_or_else(|| {
            session
                .wayland
                .space
                .element_geometry(window)
                .unwrap_or_else(|| window.geometry())
        });
        let source = restore
            .as_ref()
            .map(|restore| restore.geometry)
            .or_else(|| session.wayland.space.element_geometry(window))
            .unwrap_or_else(|| window.geometry());
        let ratio_x = ((session.pointer.position().0 - f64::from(visual.loc.x))
            / f64::from(visual.size.w.max(1)))
        .clamp(0.0, 1.0);
        let ratio_y = ((session.pointer.position().1 - f64::from(visual.loc.y))
            / f64::from(visual.size.h.max(1)))
        .clamp(0.0, 1.0);
        halley_core::field::Vec2 {
            x: -(source.size.w as f32 * ratio_x as f32) * scale,
            y: -(source.size.h as f32 * ratio_y as f32) * scale,
        }
    } else {
        halley_core::field::Vec2 {
            x: (window_location.x as f32 - world.x) * scale,
            y: (window_location.y as f32 - world.y) * scale,
        }
    };

    focus::focus_window_from_pointer(session, window, serial);
    let id = window
        .wl_surface()
        .and_then(|surface| session.nodes.id_for_surface(surface.as_ref()));
    if let Some(id) = id {
        session.nodes.clear_direct_motion(id);
    }
    let center = session
        .wayland
        .space
        .element_geometry(window)
        .map(|geometry| halley_core::field::Vec2 {
            x: geometry.loc.x as f32 + geometry.size.w as f32 * 0.5,
            y: geometry.loc.y as f32 + geometry.size.h as f32 * 0.5,
        })
        .unwrap_or(world);
    session.grab = crate::input::grab::Grab::MoveWindow {
        id,
        window: window.clone(),
        screen_offset,
        last_world: center,
        last_update: crate::frame_clock::monotonic_now(),
        velocity: halley_core::field::Vec2 { x: 0.0, y: 0.0 },
    };
    session
        .cursor
        .set_override(Some(smithay::input::pointer::CursorIcon::Grabbing));
    true
}
