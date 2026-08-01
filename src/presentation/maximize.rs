use std::collections::HashMap;
use std::time::Duration;

use halley_config::{Animations, Field};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Rectangle};
use smithay::wayland::seat::WaylandFocus;

use crate::animation::MotionTimeline;

#[derive(Clone, Copy, Debug)]
pub struct FieldMaximizePresentation {
    pub progress: f64,
    pub transition_completion: f64,
    pub windowed_rect: Rectangle<i32, Logical>,
    pub windowed_output_rect: Option<Rectangle<i32, Physical>>,
    pub target_rect: Rectangle<i32, Physical>,
}

impl FieldMaximizePresentation {
    pub fn client_rect(self, windowed: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        interpolate_rect(windowed, self.target_rect, self.progress)
    }
}

#[derive(Clone, Debug)]
pub struct FieldRestore {
    pub surface: WlSurface,
    pub geometry: Rectangle<i32, Logical>,
    pub output: String,
}

#[derive(Clone, Debug)]
pub struct FieldMaximizeChange {
    pub geometry: Rectangle<i32, Logical>,
    pub output: String,
    pub displaced: Option<FieldRestore>,
}

#[derive(Debug)]
struct Entry {
    surface: WlSurface,
    restore_geometry: Rectangle<i32, Logical>,
    restore_output: String,
    target_rect: Rectangle<i32, Logical>,
    /// Rect the window animation eases from, when it differs from
    /// `restore_geometry`. Set by the fullscreen handoff so the grow starts at
    /// the on-screen fullscreen rect instead of the small windowed rect the
    /// window will eventually restore to. Mirrors
    /// `FullscreenManager::presentation_windowed`.
    presentation_windowed: Option<Rectangle<i32, Logical>>,
    /// Exact output-local rectangle occupied by a retiring fullscreen
    /// presentation. Unlike `presentation_windowed`, this has already passed
    /// through the old camera and must not be projected through the maximize
    /// camera again.
    presentation_output: Option<Rectangle<i32, Physical>>,
    active: bool,
    window_timeline: Option<MotionTimeline>,
    camera_timeline: Option<MotionTimeline>,
}

pub struct FieldMaximizeManager {
    field: Field,
    animations: Animations,
    outputs: HashMap<String, Entry>,
}

impl FieldMaximizeManager {
    pub fn new(field: Field, animations: Animations) -> Self {
        Self {
            field,
            animations,
            outputs: HashMap::new(),
        }
    }

    pub fn reload(&mut self, field: Field, animations: Animations) -> bool {
        self.field = field;
        self.animations = animations;
        if !animations_enabled(animations) {
            for entry in self.outputs.values_mut() {
                entry.window_timeline = None;
                entry.camera_timeline = None;
            }
            return true;
        }
        false
    }

    pub fn gap(&self) -> f32 {
        self.field.gap
    }

    pub fn animations_enabled(&self) -> bool {
        animations_enabled(self.animations)
    }

    pub fn toggle(
        &mut self,
        output: &Output,
        restore: FieldRestore,
        target_rect: Rectangle<i32, Logical>,
        presentation_output: Option<Rectangle<i32, Physical>>,
        now: Duration,
    ) -> FieldMaximizeChange {
        let FieldRestore {
            surface,
            geometry: restore_geometry,
            output: restore_output,
        } = restore;
        let output_name = output.name();
        let (restore_geometry, restore_output) = authoritative_restore(
            self.outputs
                .get(&output_name)
                .filter(|entry| entry.surface == surface)
                .map(|entry| (entry.restore_geometry, entry.restore_output.clone())),
            (restore_geometry, restore_output),
        );
        match self.outputs.get_mut(&output_name) {
            Some(entry) if entry.surface == surface && entry.active => {
                let (window_progress, window_velocity) =
                    motion_state(entry.window_timeline, entry.active, now);
                let (camera_progress, camera_velocity) =
                    motion_state(entry.camera_timeline, entry.active, now);
                entry.active = false;
                entry.restore_geometry = restore_geometry;
                entry.restore_output = restore_output;
                entry.presentation_windowed = None;
                entry.presentation_output = presentation_output;
                entry.window_timeline =
                    timeline(self.animations, now, window_progress, 0.0, window_velocity);
                entry.camera_timeline =
                    timeline(self.animations, now, camera_progress, 0.0, camera_velocity);
                FieldMaximizeChange {
                    geometry: entry.restore_geometry,
                    output: entry.restore_output.clone(),
                    displaced: None,
                }
            }
            Some(entry) if entry.surface == surface => {
                let (window_progress, window_velocity) =
                    motion_state(entry.window_timeline, entry.active, now);
                let (camera_progress, camera_velocity) =
                    motion_state(entry.camera_timeline, entry.active, now);
                entry.active = true;
                entry.restore_geometry = restore_geometry;
                entry.restore_output = restore_output;
                entry.target_rect = target_rect;
                entry.presentation_windowed = None;
                entry.presentation_output = presentation_output;
                entry.window_timeline =
                    timeline(self.animations, now, window_progress, 1.0, window_velocity);
                entry.camera_timeline =
                    timeline(self.animations, now, camera_progress, 1.0, camera_velocity);
                FieldMaximizeChange {
                    geometry: target_rect,
                    output: output_name,
                    displaced: None,
                }
            }
            Some(entry) => {
                let (camera_progress, camera_velocity) =
                    motion_state(entry.camera_timeline, entry.active, now);
                let displaced = FieldRestore {
                    surface: entry.surface.clone(),
                    geometry: entry.restore_geometry,
                    output: entry.restore_output.clone(),
                };
                *entry = Entry {
                    surface,
                    restore_geometry,
                    restore_output,
                    target_rect,
                    presentation_windowed: None,
                    presentation_output,
                    active: true,
                    window_timeline: timeline(self.animations, now, 0.0, 1.0, 0.0),
                    camera_timeline: timeline(
                        self.animations,
                        now,
                        camera_progress,
                        1.0,
                        camera_velocity,
                    ),
                };
                FieldMaximizeChange {
                    geometry: target_rect,
                    output: output_name,
                    displaced: Some(displaced),
                }
            }
            None => {
                self.outputs.insert(
                    output_name.clone(),
                    Entry {
                        surface,
                        restore_geometry,
                        restore_output,
                        target_rect,
                        presentation_windowed: None,
                        presentation_output,
                        active: true,
                        window_timeline: timeline(self.animations, now, 0.0, 1.0, 0.0),
                        camera_timeline: timeline(self.animations, now, 0.0, 1.0, 0.0),
                    },
                );
                FieldMaximizeChange {
                    geometry: target_rect,
                    output: output_name,
                    displaced: None,
                }
            }
        }
    }

    pub fn presentation(
        &self,
        surface: &WlSurface,
        output: &Output,
        output_geometry: Rectangle<i32, Logical>,
        now: Duration,
    ) -> Option<FieldMaximizePresentation> {
        let entry = self.outputs.get(&output.name())?;
        if &entry.surface != surface {
            return None;
        }
        let progress = progress(entry.window_timeline, entry.active, now).clamp(0.0, 1.0);
        let transition_completion = entry
            .window_timeline
            .map(|timeline| timeline.completion_at(now))
            .unwrap_or(1.0);
        presentation_is_visible(progress, entry.window_timeline.is_some()).then(|| {
            FieldMaximizePresentation {
                progress,
                transition_completion,
                windowed_rect: entry
                    .presentation_windowed
                    .unwrap_or(entry.restore_geometry),
                windowed_output_rect: entry.presentation_output,
                target_rect: Rectangle::new(
                    (entry.target_rect.loc - output_geometry.loc).to_physical(1),
                    entry.target_rect.size.to_physical(1),
                ),
            }
        })
    }

    /// Eases the window animation from the rect a retiring fullscreen
    /// presentation last occupied, rather than from the windowed rect the
    /// window restores to. The mirror of
    /// `FullscreenManager::override_restore_from_field`, which is what already
    /// makes the maximize -> fullscreen direction start from the maximized
    /// rect.
    pub(crate) fn override_windowed_from_fullscreen(
        &mut self,
        surface: &WlSurface,
        fullscreen_geometry: Rectangle<i32, Logical>,
        fullscreen_output_rect: Option<Rectangle<i32, Physical>>,
    ) {
        if let Some(entry) = self
            .outputs
            .values_mut()
            .find(|entry| &entry.surface == surface && entry.active)
        {
            entry.presentation_windowed = Some(fullscreen_geometry);
            entry.presentation_output = fullscreen_output_rect;
        }
    }

    pub fn camera_progress(&self, output: &Output, now: Duration) -> Option<f32> {
        let entry = self.outputs.get(&output.name())?;
        Some(progress(entry.camera_timeline, entry.active, now).clamp(0.0, 1.0) as f32)
    }

    pub fn owns_output(&self, output: &str) -> bool {
        self.outputs.contains_key(output)
    }

    pub fn contains(&self, surface: &WlSurface) -> bool {
        self.outputs
            .values()
            .any(|entry| &entry.surface == surface && entry.active)
    }

    pub fn output_for_surface(&self, surface: &WlSurface) -> Option<&str> {
        self.outputs.iter().find_map(|(output, entry)| {
            (&entry.surface == surface && entry.active).then_some(output.as_str())
        })
    }

    pub fn remove(&mut self, surface: &WlSurface) -> bool {
        self.take_restore(surface).is_some()
    }

    pub fn take_restore(&mut self, surface: &WlSurface) -> Option<FieldRestore> {
        let output = self
            .outputs
            .iter()
            .find_map(|(output, entry)| (&entry.surface == surface).then(|| output.clone()))?;
        self.take_output_restore(&output)
    }

    pub fn take_output_restore(&mut self, output: &str) -> Option<FieldRestore> {
        let entry = self.outputs.remove(output)?;
        Some(FieldRestore {
            surface: entry.surface,
            geometry: entry.restore_geometry,
            output: entry.restore_output,
        })
    }

    pub fn restore(&self, surface: &WlSurface) -> Option<FieldRestore> {
        self.outputs.values().find_map(|entry| {
            (&entry.surface == surface).then(|| FieldRestore {
                surface: entry.surface.clone(),
                geometry: entry.restore_geometry,
                output: entry.restore_output.clone(),
            })
        })
    }

    pub fn remove_output(&mut self, output: &str) -> bool {
        self.outputs.remove(output).is_some()
    }

    pub fn cleanup(&mut self, now: Duration) -> FieldMaximizeCleanup {
        let mut changed = false;
        let mut finished_surfaces = Vec::new();
        self.outputs.retain(|_, entry| {
            if entry
                .window_timeline
                .is_some_and(|timeline| timeline.is_finished_at(now))
            {
                entry.window_timeline = None;
                finished_surfaces.push(entry.surface.clone());
                changed = true;
            }
            if entry
                .camera_timeline
                .is_some_and(|timeline| timeline.is_finished_at(now))
            {
                entry.camera_timeline = None;
                changed = true;
            }
            entry.active || entry.window_timeline.is_some() || entry.camera_timeline.is_some()
        });
        FieldMaximizeCleanup {
            visual_finished: changed,
            finished_surfaces,
        }
    }

    pub fn is_animating(&self, now: Duration) -> bool {
        self.outputs.values().any(|entry| {
            entry
                .window_timeline
                .is_some_and(|timeline| !timeline.is_finished_at(now))
                || entry
                    .camera_timeline
                    .is_some_and(|timeline| !timeline.is_finished_at(now))
        })
    }

    pub fn handle_commit(
        &self,
        wayland: &mut crate::wayland::WaylandState,
        surface: &WlSurface,
    ) -> bool {
        let Some(entry) = self
            .outputs
            .values()
            .find(|entry| &entry.surface == surface)
        else {
            return false;
        };
        let Some(window) = wayland
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
        let (geometry, location) = if entry.active {
            (entry.target_rect, entry.target_rect.loc)
        } else {
            (entry.restore_geometry, entry.restore_geometry.loc)
        };
        if window.geometry().size != geometry.size {
            return false;
        }
        let output = if entry.active {
            wayland
                .space
                .outputs()
                .find(|output| output.name() == self.output_for_surface_including_exit(surface))
        } else {
            wayland
                .space
                .outputs()
                .find(|output| output.name() == entry.restore_output)
        }
        .cloned();
        if let Some(output) = output {
            crate::wayland::set_window_output(&window, &output);
        }
        if wayland.space.element_location(&window) != Some(location) {
            wayland.space.relocate_element(&window, location);
            return true;
        }
        false
    }

    fn output_for_surface_including_exit(&self, surface: &WlSurface) -> &str {
        self.outputs
            .iter()
            .find_map(|(output, entry)| (&entry.surface == surface).then_some(output.as_str()))
            .unwrap_or("")
    }
}

pub struct FieldMaximizeCleanup {
    pub visual_finished: bool,
    pub finished_surfaces: Vec<WlSurface>,
}

fn animations_enabled(animations: Animations) -> bool {
    animations.enabled && animations.maximize.enabled
}

fn presentation_is_visible(progress: f64, timeline_active: bool) -> bool {
    // A running timeline owns the handoff even at its exact zero endpoint.
    // Dropping presentation there exposes the plain camera path for one frame -
    // the window has already been relocated to the maximized rect while still
    // carrying its old buffer - which reads as a flash. Same guard as
    // `fullscreen_presentation_is_visible`.
    timeline_active || progress > 0.0
}

fn timeline(
    animations: Animations,
    now: Duration,
    from: f64,
    to: f64,
    velocity: f64,
) -> Option<MotionTimeline> {
    animations_enabled(animations)
        .then(|| MotionTimeline::between(animations.maximize.motion, now, from, to, velocity))
}

fn progress(timeline: Option<MotionTimeline>, active: bool, now: Duration) -> f64 {
    timeline
        .map(|timeline| timeline.value_at(now))
        .unwrap_or_else(|| if active { 1.0 } else { 0.0 })
}

fn motion_state(timeline: Option<MotionTimeline>, active: bool, now: Duration) -> (f64, f64) {
    timeline
        .map(|timeline| (timeline.value_at(now), timeline.velocity_at(now)))
        .unwrap_or_else(|| (if active { 1.0 } else { 0.0 }, 0.0))
}

fn authoritative_restore(
    existing: Option<(Rectangle<i32, Logical>, String)>,
    requested: (Rectangle<i32, Logical>, String),
) -> (Rectangle<i32, Logical>, String) {
    existing.unwrap_or(requested)
}

fn interpolate_rect(
    from: Rectangle<i32, Physical>,
    to: Rectangle<i32, Physical>,
    progress: f64,
) -> Rectangle<i32, Physical> {
    let lerp =
        |from: i32, to: i32| (f64::from(from) + f64::from(to - from) * progress).round() as i32;
    Rectangle::new(
        (lerp(from.loc.x, to.loc.x), lerp(from.loc.y, to.loc.y)).into(),
        (
            lerp(from.size.w, to.size.w).max(1),
            lerp(from.size.h, to.size.h).max(1),
        )
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use halley_config::{AnimationCurve, AnimationMotion, EasingMotion};

    #[test]
    fn maximize_motion_retargets_without_discontinuity() {
        let mut animations = Animations::default();
        animations.maximize.motion = AnimationMotion::Easing(EasingMotion {
            duration_ms: 400,
            curve: AnimationCurve::Linear,
        });
        let started = Duration::from_secs(1);
        let forward = timeline(animations, started, 0.0, 1.0, 0.0).unwrap();
        let reversed_at = started + Duration::from_millis(100);
        let value = forward.value_at(reversed_at);
        let velocity = forward.velocity_at(reversed_at);
        let reverse = timeline(animations, reversed_at, value, 0.0, velocity).unwrap();

        assert!((reverse.value_at(reversed_at) - value).abs() < f64::EPSILON);
        assert_eq!(
            reverse.value_at(reversed_at + Duration::from_millis(400)),
            0.0
        );
    }

    #[test]
    fn maximized_rect_lands_exactly_on_target() {
        let from = Rectangle::new((100, 80).into(), (800, 600).into());
        let to = Rectangle::new((20, 20).into(), (1880, 1040).into());
        assert_eq!(interpolate_rect(from, to, 1.0), to);
    }

    #[test]
    fn existing_maximize_cycle_keeps_its_original_windowed_restore() {
        let original = Rectangle::new((120, 90).into(), (900, 650).into());
        let maximized = Rectangle::new((20, 20).into(), (1880, 1040).into());

        assert_eq!(
            authoritative_restore(
                Some((original, "DP-1".to_string())),
                (maximized, "DP-2".to_string()),
            ),
            (original, "DP-1".to_string())
        );
        assert_eq!(
            authoritative_restore(None, (maximized, "DP-2".to_string())),
            (maximized, "DP-2".to_string())
        );
    }

    #[test]
    fn presentation_survives_its_own_zero_endpoint() {
        assert!(
            presentation_is_visible(0.0, true),
            "the handoff frame must not fall back to the plain camera path"
        );
        assert!(presentation_is_visible(0.5, false));
        assert!(!presentation_is_visible(0.0, false));
    }

    #[test]
    fn fullscreen_handoff_starts_the_grow_at_the_fullscreen_rect() {
        let fullscreen = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let target = Rectangle::new((20, 20).into(), (1880, 1040).into());
        let presentation = |progress| FieldMaximizePresentation {
            progress,
            transition_completion: progress,
            windowed_rect: Rectangle::new((100, 80).into(), (800, 600).into()),
            windowed_output_rect: Some(fullscreen),
            target_rect: target,
        };

        // The handoff source replaces the windowed rect the window restores to,
        // so the first frame is exactly where fullscreen left off rather than a
        // jump down to the small rect.
        assert_eq!(presentation(0.0).client_rect(fullscreen), fullscreen);
        assert_eq!(presentation(1.0).client_rect(fullscreen), target);
    }
}
