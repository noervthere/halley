use std::borrow::Cow;
use std::ffi::OsStr;
use std::time::Duration;

use calloop::LoopHandle;
use calloop::timer::{TimeoutAction, Timer};
use halley_config::Action;
use halley_core::camera::Camera;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::seat::WaylandFocus;

use crate::wayland;

mod autostart;
pub(crate) mod closing;
mod cursor;
mod focus;
pub(crate) mod gesture;
mod input;
pub(crate) use input::{cluster_owns_focus, sync_cluster_activation_focus};
mod interaction;
mod lifecycle;
pub(crate) mod opening;
pub(crate) mod output;
pub(crate) mod pointer;
mod protocol;
mod settings;
mod spawn;
mod state;
pub(crate) mod touch;
pub(crate) mod trace;

pub mod environment;
pub mod tty;
pub mod winit;

pub(crate) use focus::focus_window;
pub use interaction::InteractionState;
pub use settings::RuntimeSettings;
pub use state::{OutputDriver, RenderDriver, Session, SessionDriver};

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
    ClusterTileFocus(halley_config::Direction),
    ClusterTileSwap(halley_config::Direction),
    MonitorFocus(halley_config::Direction),
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
        Action::MonitorFocus(direction) => return SessionControl::MonitorFocus(direction),
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
    if crate::input::grab::belongs_to_surface(&session.interactions.grab, surface) {
        cancel_compositor_grab(session);
        crate::input::grab::forget_resize_anchor(&mut session.interactions.resize_anchor, surface);
    }
}

/// Cancels compositor-owned pointer state and its cluster-side presentation
/// authority as one transaction. Modal overlays, session locking, and surface
/// destruction all use this path so a held workspace window cannot remain
/// floating or assigned to the wrong output.
pub(crate) fn cancel_compositor_grab<D: SessionDriver>(session: &mut Session<D>) {
    let provisional_cluster = match &session.interactions.grab {
        crate::input::grab::Grab::MoveWindow {
            id: Some(id),
            cluster_drag: Some(drag),
            ..
        } => Some((*id, drag.output.clone())),
        _ => None,
    };
    if matches!(
        &session.interactions.grab,
        crate::input::grab::Grab::ResizeWindow(_)
    ) {
        crate::input::grab::release_resize_anchor(&mut session.interactions.resize_anchor);
    }
    session.clusters.cancel_join_candidate();
    session.clusters.cancel_window_drag();
    if let Some((id, output_name)) = provisional_cluster {
        let output = session
            .wayland
            .space
            .outputs()
            .find(|output| output.name() == output_name)
            .cloned();
        if let Some(output) = output {
            crate::nodes::set_collapsed_output(session, id, &output);
            reconcile_cluster_surfaces(session, &output_name);
            session.request_redraw();
        }
    }
    session.interactions.grab = crate::input::grab::Grab::None;
    session.cursor.set_override(None);
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
                    session.settings.apogee.preview_max_fps,
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
            Timer::from_duration(Duration::from_millis(8)),
            |_, _, session| {
                let now = crate::frame_clock::monotonic_now();
                let overlays_changed = session.overlays.wakeup(now);
                let bloom_changed = session.clusters.bloom_wakeup(now);
                if bloom_changed {
                    for core in session.clusters.bloom_pinned_core_nodes() {
                        session.nodes.clear_direct_motion(core);
                    }
                }
                let interactions_changed = input::wakeup_cluster_interactions(session, now);
                if overlays_changed || bloom_changed || interactions_changed {
                    session.request_redraw();
                }
                TimeoutAction::ToDuration(Duration::from_millis(8))
            },
        )
        .map(|_| ())
        .map_err(Into::into)
}

pub(crate) fn reconcile_pointer_constraints<D: SessionDriver>(session: &mut Session<D>) {
    pointer::reconcile_state(session);
}

/// Applies cluster workspace geometry at the client protocol boundary.
///
/// ClusterSystem owns the layout and deduplication; Session owns Smithay
/// surfaces and X11 configure calls. Field remains unaware of workspace sizes.
pub(crate) fn reconcile_cluster_surfaces<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
) {
    let now = crate::frame_clock::monotonic_now();
    let Some(output) = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == output_name)
        .cloned()
    else {
        return;
    };
    let Some(output_geometry) = session.wayland.space.output_geometry(&output) else {
        return;
    };
    let work_area = smithay::desktop::layer_map_for_output(&output).non_exclusive_zone();
    let targets =
        session
            .clusters
            .workspace_surface_targets(output_name, work_area, output_geometry);
    for target in targets {
        if session
            .clusters
            .surface_layout_is_deferred(target.node_id, now)
        {
            continue;
        }
        let Some((window, surface, current)) = session.nodes.record(target.node_id).map(|record| {
            (
                record.window.clone(),
                record.surface.clone(),
                session
                    .wayland
                    .space
                    .element_geometry(&record.window)
                    .unwrap_or(record.geometry),
            )
        }) else {
            continue;
        };
        if session.fullscreen.is_fullscreen_or_pending(&surface)
            || session.maximize.contains(&surface)
        {
            continue;
        }
        if window.x11_surface().is_some_and(|x11| {
            session
                .xwayland
                .client_geometry_guarded(x11.window_id(), now)
        }) {
            continue;
        }
        if !session
            .clusters
            .prepare_surface_target(target.node_id, current, target.geometry)
        {
            continue;
        }
        crate::wayland::set_window_output(&window, &output);
        session
            .wayland
            .space
            .relocate_element(&window, target.geometry.loc);
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|pending| {
                pending.size = Some(target.geometry.size);
                pending.bounds = Some(work_area.size);
                crate::wayland::decoration::apply_tiled_hint(pending);
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
        } else {
            crate::xwayland::configure_window(&window, target.geometry);
        }
    }
}

pub(crate) fn sync_cluster_camera<D: SessionDriver>(
    session: &mut Session<D>,
    output_name: &str,
) -> bool {
    session.cameras.set_cluster_active(
        output_name,
        session.clusters.active_on(output_name).is_some(),
    )
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
    let Some(record) = focused_window_record(session, output) else {
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
    let cluster_restore = cluster_presentation_restore(session, &focused, now, entering);
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
    if !entering && let Some(restore) = cluster_restore.as_ref() {
        session.fullscreen.override_restore_from_cluster(
            &focused,
            restore.geometry,
            restore.output.clone(),
            restore.presentation_output,
        );
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
    if entering && let Some(restore) = cluster_restore {
        session.fullscreen.override_restore_from_cluster(
            &focused,
            restore.geometry,
            restore.output,
            restore.presentation_output,
        );
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

pub(crate) fn forget_destroyed_cluster_member<D: SessionDriver>(
    session: &mut Session<D>,
    id: halley_core::field::NodeId,
) {
    let work_area = session
        .nodes
        .record(id)
        .and_then(|record| {
            session
                .wayland
                .space
                .outputs()
                .find(|output| output.name() == record.output)
        })
        .map(|output| smithay::desktop::layer_map_for_output(output).non_exclusive_zone());
    if let Some(work_area) = work_area {
        session.clusters.forget_destroyed_member_animated(
            &mut session.nodes.field,
            id,
            work_area,
            crate::frame_clock::monotonic_now(),
        );
    } else {
        session
            .clusters
            .forget_destroyed_member(&mut session.nodes.field, id);
    }
}

fn toggle_focused_field_maximize<D: SessionDriver>(session: &mut Session<D>, output: Option<&str>) {
    let Some(record) = focused_window_record(session, output) else {
        return;
    };
    let _ = toggle_field_maximize(session, record);
}

/// Window actions follow the compositor's live client focus first. The node
/// focus is persistent by design and can lag briefly during hover transitions;
/// using it first made maximize/fullscreen target an older cluster member.
fn focused_window_record<D: SessionDriver>(
    session: &Session<D>,
    output: Option<&str>,
) -> Option<crate::nodes::NodeRecord> {
    let belongs_to_output = |record: &&crate::nodes::NodeRecord| {
        output.is_none_or(|output| {
            crate::wayland::window_output_name(&record.window)
                .as_deref()
                .unwrap_or(&record.output)
                == output
        })
    };
    session
        .wayland
        .focused_window
        .as_ref()
        .and_then(|surface| session.nodes.id_for_surface(surface))
        .and_then(|id| session.nodes.record(id))
        .filter(|record| !record.collapsed)
        .filter(belongs_to_output)
        .cloned()
        .or_else(|| {
            let id = match output {
                Some(output) => session.nodes.focused_on_output(output),
                None => session.nodes.focused(),
            }?;
            session
                .nodes
                .record(id)
                .filter(|record| !record.collapsed)
                .filter(belongs_to_output)
                .cloned()
        })
}

pub(crate) fn set_surface_field_maximized<D: SessionDriver>(
    session: &mut Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    maximized: bool,
) -> bool {
    // Client-side title bars can issue an interactive move on press before a
    // double-click state request. Retire that pending move first so maximize
    // and unmaximize use the same transaction as the title-bar button.
    cancel_grab_for_surface(session, surface);
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
    let inset = (session.settings.field.gap.ceil() as i32)
        .saturating_add(session.settings.decorations.border_width_px);
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
    let now = crate::frame_clock::monotonic_now();
    let entering = !session.maximize.contains(&record.surface);
    // A maximize entry owns the original windowed placement for its entire
    // lifetime, including an in-flight unmaximize/re-maximize reversal. The
    // live Space geometry is already the maximized configure by the time a
    // client requests unmaximize and must never replace this snapshot.
    let tracked_restore = session.maximize.restore(&record.surface);
    let cluster_restore = cluster_presentation_restore(session, &record.surface, now, entering);
    let inherited_restore = session.fullscreen.restore_placement(&record.surface);
    let Some(restore_geometry) = tracked_restore
        .as_ref()
        .map(|restore| restore.geometry)
        .or_else(|| inherited_restore.as_ref().map(|(geometry, _)| *geometry))
        .or_else(|| cluster_restore.as_ref().map(|restore| restore.geometry))
        .or_else(|| session.wayland.space.element_geometry(&record.window))
    else {
        return false;
    };
    let restore_output = tracked_restore
        .as_ref()
        .map(|restore| restore.output.clone())
        .or_else(|| inherited_restore.and_then(|(_, output)| output))
        .or_else(|| {
            cluster_restore
                .as_ref()
                .map(|restore| restore.output.clone())
        })
        .unwrap_or_else(|| output_name.clone());

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
        crate::presentation::maximize::FieldRestore {
            surface: record.surface.clone(),
            geometry: restore_geometry,
            output: restore_output,
        },
        target,
        cluster_restore.and_then(|restore| restore.presentation_output),
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

#[derive(Clone, Debug)]
pub(crate) struct ClusterPresentationRestore {
    pub(crate) geometry: Rectangle<i32, Logical>,
    pub(crate) output: String,
    pub(crate) presentation_output: Option<Rectangle<i32, smithay::utils::Physical>>,
}

pub(crate) fn cluster_presentation_restore<D: SessionDriver>(
    session: &Session<D>,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    now: std::time::Duration,
    entering: bool,
) -> Option<ClusterPresentationRestore> {
    let id = session.nodes.id_for_surface(surface)?;
    let cluster = session.clusters.cluster_for_member(id)?;
    let metadata = session.clusters.metadata(cluster)?;
    (session.clusters.active_on(&metadata.output) == Some(cluster)).then_some(())?;
    let output = session
        .wayland
        .space
        .outputs()
        .find(|output| output.name() == metadata.output)?;
    let output_geometry = session.wayland.space.output_geometry(output)?;
    let work_area = smithay::desktop::layer_map_for_output(output).non_exclusive_zone();
    let target = session.clusters.workspace_surface_target_for(
        id,
        &metadata.output,
        work_area,
        output_geometry,
    )?;
    let window = session.nodes.record(id)?.window.clone();
    let tiled = Rectangle::new(
        (target.geometry.loc - output_geometry.loc).to_physical(1),
        target.geometry.size.to_physical(1),
    );
    // Entry must start at the member's live, possibly animated tile. Exit
    // must finish at the latest layout target rather than reusing the current
    // fullscreen/maximized rectangle as its windowed endpoint.
    let presented = Some(
        entering
            .then(|| presented_window_rect(session, &window, output, now))
            .flatten()
            .unwrap_or(tiled),
    );
    Some(ClusterPresentationRestore {
        geometry: target.geometry,
        output: metadata.output.clone(),
        presentation_output: presented,
    })
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
        if let Some(surface) = window.x11_surface()
            && let Err(err) = surface.set_maximized(session.maximize.contains(&request.surface))
        {
            eventline::warn!("xwayland: failed to synchronize field-maximized state: {err}");
        }
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
        session.xwayland.sync_active_window(None);
        return;
    }
    wayland::focus::refresh_selected_layer(&mut session.wayland);
    if let Some(surface) = session.wayland.focused_window.clone() {
        session
            .nodes
            .focus_surface(&surface, session.start_time.elapsed().as_millis() as u64);
    } else if session.nodes.focused().is_some_and(|id| {
        session
            .nodes
            .record(id)
            .is_some_and(|record| !record.collapsed)
            && session.clusters.cluster_for_member(id).is_none()
    }) {
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
    let active_x11_window = focused
        .as_ref()
        .and_then(crate::xwayland::KeyboardFocusTarget::x11_window_id);
    if let Some(focused) = focused.as_ref() {
        focused.acknowledge_attention();
    }
    pointer::prepare_keyboard_focus_change(session, next_constraint_root.as_ref());
    keyboard.set_focus(session, focused, serial);
    session.xwayland.sync_active_window(active_x11_window);
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
    let surface = window.wl_surface();
    if surface
        .as_ref()
        .is_some_and(|surface| session.maximize.contains(surface.as_ref()))
        || surface
            .as_ref()
            .and_then(|surface| session.nodes.id_for_surface(surface.as_ref()))
            .is_some_and(|id| session.clusters.active_layout_for_member(id).is_some())
    {
        return false;
    }
    let Some(start_rect) = session.wayland.space.element_geometry(window) else {
        return false;
    };
    focus::focus_window_from_pointer(session, window, serial);
    session.interactions.grab =
        crate::input::grab::Grab::ResizeWindow(crate::input::grab::ResizeState {
            window: window.clone(),
            handle,
            button,
            start_rect,
            start_cursor: cursor,
        });
    session.interactions.resize_anchor =
        window.toplevel().map(|_| crate::input::grab::ResizeAnchor {
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

/// Arms a client-requested move without committing any move side effects.
/// Toolkits commonly send this request from their title-bar press handler,
/// before they know whether the gesture will become a click or a drag.
pub(crate) fn begin_client_pointer_move<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    serial: smithay::utils::Serial,
    button: u32,
) -> bool {
    if !matches!(session.interactions.grab, crate::input::grab::Grab::None)
        || !crate::window::accepts_compositor_grab(window)
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
    let visual_geometry = route.visual_geometry.unwrap_or_else(|| {
        session
            .wayland
            .space
            .element_geometry(window)
            .unwrap_or_else(|| window.geometry())
    });
    let maximized = window
        .wl_surface()
        .is_some_and(|surface| session.maximize.contains(surface.as_ref()));
    session.interactions.grab =
        crate::input::grab::Grab::PendingWindowMove(crate::input::grab::PendingWindowMove {
            window: window.clone(),
            serial,
            button,
            press_screen: smithay::utils::Point::from(session.pointer.position()),
            output: route.output.name(),
            visual_geometry,
            maximized,
        });
    true
}

/// Starts a compositor-owned move immediately. Client title bars use
/// `begin_client_pointer_move` and promote only after real pointer motion.
pub(crate) fn begin_pointer_move<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    serial: smithay::utils::Serial,
    button: u32,
) -> bool {
    begin_pointer_move_active(session, window, serial, button, false, None)
}

pub(crate) fn activate_client_pointer_move<D: SessionDriver>(
    session: &mut Session<D>,
    pending: crate::input::grab::PendingWindowMove,
) -> bool {
    let window = pending.window.clone();
    begin_pointer_move_active(
        session,
        &window,
        pending.serial,
        pending.button,
        true,
        Some(pending),
    )
}

fn begin_pointer_move_active<D: SessionDriver>(
    session: &mut Session<D>,
    window: &smithay::desktop::Window,
    serial: smithay::utils::Serial,
    button: u32,
    client_owned: bool,
    pending: Option<crate::input::grab::PendingWindowMove>,
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
    let (output_name, visual_geometry, press_screen, was_maximized) =
        if let Some(pending) = pending.as_ref() {
            (
                pending.output.clone(),
                pending.visual_geometry,
                pending.press_screen,
                pending.maximized,
            )
        } else {
            let Some(route) = pointer::route_client(session) else {
                return false;
            };
            if !matches!(
                route.target,
                crate::input::pointer::PointerTarget::Window(ref routed) if routed == window
            ) {
                return false;
            }
            let visual = route.visual_geometry.unwrap_or_else(|| {
                session
                    .wayland
                    .space
                    .element_geometry(window)
                    .unwrap_or_else(|| window.geometry())
            });
            let maximized = window
                .wl_surface()
                .is_some_and(|surface| session.maximize.contains(surface.as_ref()));
            (
                route.output.name(),
                visual,
                smithay::utils::Point::from(session.pointer.position()),
                maximized,
            )
        };
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
    let Some(camera) = session.cameras.get(&output_name) else {
        return false;
    };
    let scale = crate::input::zoom::scale(camera);
    let pointer_position = session.pointer.position();
    let pointer_world =
        crate::input::grab::screen_to_world_on_output(pointer_position, camera, output_geometry);
    let press_world = crate::input::grab::screen_to_world_on_output(
        (press_screen.x, press_screen.y),
        camera,
        output_geometry,
    );
    let world = halley_core::field::Vec2 {
        x: pointer_world.x,
        y: pointer_world.y,
    };
    let Some(window_location) = session.wayland.space.element_location(window) else {
        return false;
    };
    let restore = was_maximized
        .then(|| {
            window
                .wl_surface()
                .and_then(|surface| session.maximize.restore(surface.as_ref()))
        })
        .flatten();

    let id = window
        .wl_surface()
        .and_then(|surface| session.nodes.id_for_surface(surface.as_ref()));
    let mut cluster_drag = id.and_then(|id| {
        let cluster_id = session.clusters.cluster_for_member(id)?;
        let metadata = session.clusters.metadata(cluster_id)?;
        (metadata.output == output_name
            && session.clusters.active_on(&output_name) == Some(cluster_id))
        .then(|| crate::input::grab::ClusterWindowDrag {
            cluster_id,
            output: output_name.clone(),
            layout: metadata.layout,
            on_origin_output: true,
        })
    });
    if cluster_drag.as_ref().is_some_and(|drag| {
        drag.layout == halley_core::cluster::layout::ClusterWorkspaceLayoutKind::Stacking
            && id != session.clusters.first_member(drag.cluster_id)
    }) {
        return false;
    }
    focus::focus_window_from_pointer(session, window, serial);

    let mut cluster_grab_location = None;
    let mut cluster_drag_rect = None;
    let screen_offset = if cluster_drag.is_some() {
        let offset = crate::input::grab::screen_grip_offset(
            (press_screen.x, press_screen.y),
            visual_geometry.loc,
        );
        cluster_drag_rect = Some(Rectangle::new(
            visual_geometry.loc - output_geometry.loc,
            visual_geometry.size,
        ));
        let camera = session
            .cameras
            .get(&output_name)
            .expect("the pointer output camera was validated above");
        cluster_grab_location = Some(crate::input::grab::world_location_from_screen_grip(
            pointer_position,
            offset,
            camera,
            output_geometry,
        ));
        offset
    } else if was_maximized {
        let source = restore
            .as_ref()
            .map(|restore| restore.geometry)
            .or_else(|| session.wayland.space.element_geometry(window))
            .unwrap_or_else(|| window.geometry());
        let ratio_x = ((press_screen.x - f64::from(visual_geometry.loc.x))
            / f64::from(visual_geometry.size.w.max(1)))
        .clamp(0.0, 1.0);
        let ratio_y = ((press_screen.y - f64::from(visual_geometry.loc.y))
            / f64::from(visual_geometry.size.h.max(1)))
        .clamp(0.0, 1.0);
        halley_core::field::Vec2 {
            x: -(source.size.w as f32 * ratio_x as f32) * scale,
            y: -(source.size.h as f32 * ratio_y as f32) * scale,
        }
    } else {
        halley_core::field::Vec2 {
            x: (window_location.x as f32 - press_world.x) * scale,
            y: (window_location.y as f32 - press_world.y) * scale,
        }
    };

    if let Some(expected) = restore.as_ref() {
        let maximize_output = session
            .maximize
            .output_for_surface(&expected.surface)
            .map(str::to_owned);
        if let Some(restore) = session.maximize.take_restore(&expected.surface) {
            session.render.fullscreen_textures.remove(&restore.surface);
            configure_field_geometry(session, &restore);
            if let Some(output) = maximize_output {
                let _ = session.cameras.apply_field_maximize(&output, None);
            }
        }
    }

    if let (Some(id), Some(location), Some(drag)) =
        (id, cluster_grab_location, cluster_drag.as_mut())
    {
        let Some(rect) = cluster_drag_rect else {
            return false;
        };
        if !session
            .clusters
            .begin_workspace_drag(&drag.output, id, rect)
        {
            return false;
        }
        reconcile_cluster_surfaces(session, &output_name);
        session.wayland.space.relocate_element(window, location);
        crate::window::raise_managed(&mut session.wayland, window);
        session.xwayland.raise_window(window);
    } else if let Some(id) = id {
        let _ = session.clusters.begin_field_drag(id);
        crate::window::raise_managed(&mut session.wayland, window);
        session.xwayland.raise_window(window);
    }

    if let Some(id) = id {
        session.nodes.clear_direct_motion(id);
    }
    let drag_size = restore.as_ref().map(|restore| restore.geometry.size);
    let center = drag_size
        .and_then(|size| {
            let camera = session.cameras.get(&output_name)?;
            let location = crate::input::grab::world_location_from_screen_grip(
                pointer_position,
                screen_offset,
                camera,
                output_geometry,
            );
            Some(halley_core::field::Vec2 {
                x: location.x as f32 + size.w as f32 * 0.5,
                y: location.y as f32 + size.h as f32 * 0.5,
            })
        })
        .or_else(|| {
            session
                .wayland
                .space
                .element_geometry(window)
                .map(|geometry| halley_core::field::Vec2 {
                    x: geometry.loc.x as f32 + geometry.size.w as f32 * 0.5,
                    y: geometry.loc.y as f32 + geometry.size.h as f32 * 0.5,
                })
        })
        .unwrap_or(world);
    session.interactions.grab = crate::input::grab::Grab::MoveWindow {
        id,
        window: window.clone(),
        cluster_drag,
        drag_size,
        button,
        client_owned,
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
