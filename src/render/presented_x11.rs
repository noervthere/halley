use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::seat::WaylandFocus;

/// One XWayland surface tree whose buffers were part of a submitted frame.
///
/// Smithay's render elements retain both the imported renderer texture and a
/// [`Buffer`](smithay::backend::renderer::utils::Buffer) guard. Keeping the
/// elements alive therefore freezes this generation without copying its
/// pixels and without releasing the client buffer for reuse.
#[derive(Debug)]
pub struct PresentedX11Frame {
    pub(crate) surface: WlSurface,
    pub(crate) geometry: Rectangle<i32, Logical>,
    pub(crate) elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
}

const HISTORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const PRE_TEARDOWN_AGE: Duration = Duration::from_millis(200);
const MAX_HISTORY_SAMPLES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresentedX11FramePolicy {
    Latest,
    PreTeardown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresentedX11FrameSelectionKind {
    Latest,
    Guarded,
    OldestFallback,
}

pub(crate) struct PresentedX11FrameSelection<'a> {
    pub(crate) frame: &'a PresentedX11Frame,
    pub(crate) kind: PresentedX11FrameSelectionKind,
    pub(crate) age: Duration,
    pub(crate) sample_count: usize,
}

#[derive(Debug)]
struct TimedPresentedX11Frame {
    presented_at: Duration,
    frame: Arc<PresentedX11Frame>,
}

#[derive(Debug)]
struct PresentedX11History {
    latest: TimedPresentedX11Frame,
    samples: VecDeque<TimedPresentedX11Frame>,
}

impl PresentedX11History {
    fn new(frame: PresentedX11Frame, presented_at: Duration) -> Self {
        let frame = Arc::new(frame);
        let sample = TimedPresentedX11Frame {
            presented_at,
            frame: Arc::clone(&frame),
        };
        Self {
            latest: TimedPresentedX11Frame {
                presented_at,
                frame,
            },
            samples: VecDeque::from([sample]),
        }
    }

    fn promote(&mut self, frame: PresentedX11Frame, presented_at: Duration) {
        let frame = Arc::new(frame);
        self.latest = TimedPresentedX11Frame {
            presented_at,
            frame: Arc::clone(&frame),
        };
        if self.samples.back().is_some_and(|sample| {
            presented_at.saturating_sub(sample.presented_at) < HISTORY_SAMPLE_INTERVAL
        }) {
            return;
        }
        self.samples.push_back(TimedPresentedX11Frame {
            presented_at,
            frame,
        });
        while self.samples.len() > MAX_HISTORY_SAMPLES {
            self.samples.pop_front();
        }
    }

    fn select(
        &self,
        now: Duration,
        policy: PresentedX11FramePolicy,
    ) -> PresentedX11FrameSelection<'_> {
        if policy == PresentedX11FramePolicy::Latest {
            return selection(
                &self.latest,
                now,
                self.samples.len(),
                PresentedX11FrameSelectionKind::Latest,
            );
        }
        let (index, kind) =
            pre_teardown_sample_index(&self.samples, now, |sample| sample.presented_at)
                .expect("every presented X11 history retains its initial sample");
        selection(&self.samples[index], now, self.samples.len(), kind)
    }
}

fn selection<'a>(
    timed: &'a TimedPresentedX11Frame,
    now: Duration,
    sample_count: usize,
    kind: PresentedX11FrameSelectionKind,
) -> PresentedX11FrameSelection<'a> {
    PresentedX11FrameSelection {
        frame: &timed.frame,
        kind,
        age: now.saturating_sub(timed.presented_at),
        sample_count,
    }
}

fn pre_teardown_sample_index<T>(
    samples: &VecDeque<T>,
    now: Duration,
    presented_at: impl Fn(&T) -> Duration,
) -> Option<(usize, PresentedX11FrameSelectionKind)> {
    samples
        .iter()
        .rposition(|sample| now.saturating_sub(presented_at(sample)) >= PRE_TEARDOWN_AGE)
        .map(|index| (index, PresentedX11FrameSelectionKind::Guarded))
        .or_else(|| {
            (!samples.is_empty()).then_some((0, PresentedX11FrameSelectionKind::OldestFallback))
        })
}

/// Renderer-owned bounded history of presented content for managed X11 windows.
#[derive(Default)]
pub struct PresentedX11Frames {
    frames: HashMap<WlSurface, PresentedX11History>,
}

impl PresentedX11Frames {
    pub(crate) fn select(
        &self,
        surface: &WlSurface,
        now: Duration,
        policy: PresentedX11FramePolicy,
    ) -> Option<PresentedX11FrameSelection<'_>> {
        self.frames
            .get(surface)
            .map(|history| history.select(now, policy))
    }

    pub fn remove(&mut self, surface: &WlSurface) -> bool {
        self.frames.remove(surface).is_some()
    }

    /// Promotes frames only after their backend has confirmed presentation.
    /// Candidates for windows that disappeared while a page flip was pending
    /// are discarded instead of resurrecting stale buffer guards.
    pub fn promote(
        &mut self,
        candidates: Vec<PresentedX11Frame>,
        space: &Space<Window>,
        presented_at: Duration,
    ) {
        for candidate in candidates {
            if mapped_managed_x11(space, &candidate.surface) {
                match self.frames.entry(candidate.surface.clone()) {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        entry.get_mut().promote(candidate, presented_at);
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(PresentedX11History::new(candidate, presented_at));
                    }
                }
            }
        }
        self.frames
            .retain(|surface, _| mapped_managed_x11(space, surface));
    }
}

/// Retains local-coordinate surface elements for X11 windows that Smithay
/// reports as visible in the frame being submitted on `output`.
pub fn candidates_for_output(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    primary_output: &Output,
    states: &RenderElementStates,
) -> Vec<PresentedX11Frame> {
    space
        .elements()
        .filter(|window| candidate_window(window, output, primary_output, states))
        .filter_map(|window| {
            let surface = window.wl_surface()?.into_owned();
            let geometry = window.geometry();
            if geometry.size.w <= 0 || geometry.size.h <= 0 {
                return None;
            }
            let location =
                smithay::utils::Point::from((-geometry.loc.x, -geometry.loc.y)).to_physical(1);
            let elements = render_elements_from_surface_tree(
                renderer,
                &surface,
                location,
                1.0,
                1.0,
                smithay::backend::renderer::element::Kind::Unspecified,
            );
            (!elements.is_empty()).then_some(PresentedX11Frame {
                surface,
                geometry,
                elements,
            })
        })
        .collect()
}

fn candidate_window(
    window: &Window,
    output: &Output,
    primary_output: &Output,
    states: &RenderElementStates,
) -> bool {
    candidate_is_eligible(
        crate::xwayland::is_x11(window),
        crate::xwayland::is_override_redirect(window),
        crate::wayland::window_is_on_output(window, output, primary_output),
        window
            .wl_surface()
            .is_some_and(|surface| states.element_was_presented(surface.as_ref())),
    )
}

fn candidate_is_eligible(
    is_x11: bool,
    is_override_redirect: bool,
    is_on_output: bool,
    was_presented: bool,
) -> bool {
    is_x11 && !is_override_redirect && is_on_output && was_presented
}

fn mapped_managed_x11(space: &Space<Window>, surface: &WlSurface) -> bool {
    space.elements().any(|window| {
        crate::xwayland::is_x11(window)
            && !crate::xwayland::is_override_redirect(window)
            && window
                .wl_surface()
                .is_some_and(|candidate| candidate.as_ref() == surface)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_visible_managed_x11_content_becomes_a_candidate() {
        assert!(candidate_is_eligible(true, false, true, true));
        assert!(!candidate_is_eligible(false, false, true, true));
        assert!(!candidate_is_eligible(true, true, true, true));
        assert!(!candidate_is_eligible(true, false, false, true));
        assert!(!candidate_is_eligible(true, false, true, false));
    }

    #[test]
    fn history_samples_are_spaced_and_bounded() {
        let mut samples = VecDeque::new();
        for millis in [0, 50, 100, 200, 300] {
            if samples.back().is_none_or(|sample: &Duration| {
                Duration::from_millis(millis).saturating_sub(*sample) >= HISTORY_SAMPLE_INTERVAL
            }) {
                samples.push_back(Duration::from_millis(millis));
                while samples.len() > MAX_HISTORY_SAMPLES {
                    samples.pop_front();
                }
            }
        }
        assert_eq!(
            samples,
            VecDeque::from([
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(300),
            ])
        );
    }

    #[test]
    fn pre_teardown_selection_skips_recent_teardown_frames() {
        let samples = VecDeque::from([
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(300),
        ]);
        let (selected, kind) =
            pre_teardown_sample_index(&samples, Duration::from_millis(350), |time| *time).unwrap();
        assert_eq!(selected, 0);
        assert_eq!(kind, PresentedX11FrameSelectionKind::Guarded);
    }

    #[test]
    fn pre_teardown_selection_uses_the_newest_guarded_frame() {
        let samples = VecDeque::from([
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(300),
        ]);
        let (selected, kind) =
            pre_teardown_sample_index(&samples, Duration::from_millis(500), |time| *time).unwrap();
        assert_eq!(selected, 2);
        assert_eq!(kind, PresentedX11FrameSelectionKind::Guarded);
    }

    #[test]
    fn short_lived_history_falls_back_to_its_oldest_frame() {
        let samples = VecDeque::from([Duration::from_millis(100), Duration::from_millis(180)]);
        let (selected, kind) =
            pre_teardown_sample_index(&samples, Duration::from_millis(250), |time| *time).unwrap();
        assert_eq!(selected, 0);
        assert_eq!(kind, PresentedX11FrameSelectionKind::OldestFallback);
    }
}
