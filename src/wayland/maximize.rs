use std::collections::HashMap;
use std::time::Duration;

use halley_config::{Animations, Field};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Rectangle};

#[derive(Clone, Copy, Debug)]
pub struct FieldMaximizePresentation {
    pub progress: f64,
    pub target_rect: Rectangle<i32, Physical>,
}

impl FieldMaximizePresentation {
    pub fn client_rect(self, windowed: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        interpolate_rect(windowed, self.target_rect, self.progress)
    }
}

#[derive(Debug)]
struct Entry {
    surface: WlSurface,
    target_rect: Rectangle<i32, Logical>,
    active: bool,
    window_timeline: Option<FieldTimeline>,
    camera_timeline: Option<FieldTimeline>,
}

#[derive(Clone, Copy, Debug)]
struct FieldTimeline {
    started: Duration,
    duration: Duration,
    from: f64,
    to: f64,
}

impl FieldTimeline {
    fn value_at(self, now: Duration) -> f64 {
        if self.duration.is_zero() {
            return self.to;
        }
        let linear = (now.saturating_sub(self.started).as_secs_f64() / self.duration.as_secs_f64())
            .clamp(0.0, 1.0);
        self.from + (self.to - self.from) * ease_in_out_cubic(linear)
    }

    fn is_finished_at(self, now: Duration) -> bool {
        now.saturating_sub(self.started) >= self.duration
    }
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

    pub fn reload(&mut self, field: Field, animations: Animations) {
        self.field = field;
        self.animations = animations;
        if !animations_enabled(animations) {
            for entry in self.outputs.values_mut() {
                entry.window_timeline = None;
                entry.camera_timeline = None;
            }
        }
    }

    pub fn gap(&self) -> f32 {
        self.field.gap
    }

    pub fn toggle(
        &mut self,
        output: &Output,
        surface: WlSurface,
        target_rect: Rectangle<i32, Logical>,
        now: Duration,
    ) {
        let output_name = output.name();
        match self.outputs.get_mut(&output_name) {
            Some(entry) if entry.surface == surface && entry.active => {
                let window_progress = progress(entry.window_timeline, entry.active, now);
                let camera_progress = progress(entry.camera_timeline, entry.active, now);
                entry.active = false;
                entry.window_timeline = timeline(self.animations, now, window_progress, 0.0);
                entry.camera_timeline = timeline(self.animations, now, camera_progress, 0.0);
            }
            Some(entry) => {
                let camera_progress = progress(entry.camera_timeline, entry.active, now);
                *entry = Entry {
                    surface,
                    target_rect,
                    active: true,
                    window_timeline: timeline(self.animations, now, 0.0, 1.0),
                    camera_timeline: timeline(self.animations, now, camera_progress, 1.0),
                };
            }
            None => {
                self.outputs.insert(
                    output_name,
                    Entry {
                        surface,
                        target_rect,
                        active: true,
                        window_timeline: timeline(self.animations, now, 0.0, 1.0),
                        camera_timeline: timeline(self.animations, now, 0.0, 1.0),
                    },
                );
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
        (progress > 0.0).then(|| FieldMaximizePresentation {
            progress,
            target_rect: Rectangle::new(
                (entry.target_rect.loc - output_geometry.loc).to_physical(1),
                entry.target_rect.size.to_physical(1),
            ),
        })
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
        let before = self.outputs.len();
        self.outputs.retain(|_, entry| &entry.surface != surface);
        self.outputs.len() != before
    }

    pub fn remove_output(&mut self, output: &str) -> bool {
        self.outputs.remove(output).is_some()
    }

    pub fn cleanup(&mut self, now: Duration) -> bool {
        let mut changed = false;
        self.outputs.retain(|_, entry| {
            if entry
                .window_timeline
                .is_some_and(|timeline| timeline.is_finished_at(now))
            {
                entry.window_timeline = None;
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
        changed
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
}

fn animations_enabled(animations: Animations) -> bool {
    animations.enabled && animations.maximize.enabled && animations.maximize.duration_ms > 0
}

fn timeline(animations: Animations, now: Duration, from: f64, to: f64) -> Option<FieldTimeline> {
    animations_enabled(animations).then(|| FieldTimeline {
        started: now,
        duration: Duration::from_millis(u64::from(animations.maximize.duration_ms)),
        from,
        to,
    })
}

fn progress(timeline: Option<FieldTimeline>, active: bool, now: Duration) -> f64 {
    timeline
        .map(|timeline| timeline.value_at(now))
        .unwrap_or_else(|| if active { 1.0 } else { 0.0 })
}

fn ease_in_out_cubic(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if value < 0.5 {
        4.0 * value * value * value
    } else {
        1.0 - (-2.0 * value + 2.0).powi(3) / 2.0
    }
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

    #[test]
    fn maximize_easing_preserves_endpoints() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert_eq!(ease_in_out_cubic(0.5), 0.5);
    }

    #[test]
    fn maximized_rect_lands_exactly_on_target() {
        let from = Rectangle::new((100, 80).into(), (800, 600).into());
        let to = Rectangle::new((20, 20).into(), (1880, 1040).into());
        assert_eq!(interpolate_rect(from, to, 1.0), to);
    }
}
