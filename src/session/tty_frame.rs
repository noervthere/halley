use std::time::Duration;

use calloop::RegistrationToken;

use crate::frame_clock::FrameClock;

/// The redraw work an output owes and the kernel/timer event that currently
/// gates it. Keeping these transitions together prevents input, DRM, and
/// timer handlers from independently interpreting the same state.
#[derive(Debug, Default)]
enum RedrawState {
    #[default]
    Idle,
    Queued,
    WaitingForVBlank {
        redraw_needed: bool,
    },
    WaitingForEstimatedVBlank(RegistrationToken),
    WaitingForEstimatedVBlankAndQueued(RegistrationToken),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EstimatedVblankTimer {
    AlreadyArmed,
    ArmAfter(Duration),
}

pub(super) struct OutputFrameState {
    clock: FrameClock,
    redraw: RedrawState,
    last_camera_sample: Duration,
    unfinished_animations: bool,
}

impl OutputFrameState {
    pub fn new(refresh_interval: Duration) -> Self {
        Self {
            clock: FrameClock::new(Some(refresh_interval)),
            redraw: RedrawState::default(),
            last_camera_sample: crate::frame_clock::monotonic_now(),
            unfinished_animations: false,
        }
    }

    pub fn queue_redraw(&mut self) {
        self.redraw = match std::mem::take(&mut self.redraw) {
            RedrawState::Idle => RedrawState::Queued,
            RedrawState::WaitingForEstimatedVBlank(token) => {
                RedrawState::WaitingForEstimatedVBlankAndQueued(token)
            }
            value @ (RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued(_)) => {
                value
            }
            RedrawState::WaitingForVBlank { .. } => RedrawState::WaitingForVBlank {
                redraw_needed: true,
            },
        };
    }

    pub fn is_redraw_queued(&self) -> bool {
        matches!(
            self.redraw,
            RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued(_)
        )
    }

    pub fn replace_clock(&mut self, refresh_interval: Duration) {
        self.clock = FrameClock::new(Some(refresh_interval));
        self.last_camera_sample = crate::frame_clock::monotonic_now();
        self.unfinished_animations = false;
    }

    pub fn next_frame_sample(&mut self, now: Duration) -> (Duration, Duration) {
        let target = self.clock.next_presentation_time(now);
        let dt = target.saturating_sub(self.last_camera_sample);
        self.last_camera_sample = target;
        (target, dt)
    }

    pub fn on_vblank(&mut self, presented: Option<Duration>) -> Option<String> {
        self.clock.presented(presented);
        let (redraw_needed, unexpected_state) = match std::mem::take(&mut self.redraw) {
            RedrawState::WaitingForVBlank { redraw_needed } => {
                (redraw_needed || self.unfinished_animations, None)
            }
            other => (true, Some(format!("{other:?}"))),
        };
        self.redraw = if redraw_needed {
            RedrawState::Queued
        } else {
            RedrawState::Idle
        };

        unexpected_state
    }

    /// Records a real page flip and returns an estimated-VBlank timer that
    /// became obsolete, if one was armed.
    pub fn frame_submitted(&mut self, animating: bool) -> Option<RegistrationToken> {
        let timer = match std::mem::take(&mut self.redraw) {
            RedrawState::WaitingForEstimatedVBlank(token)
            | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => Some(token),
            _ => None,
        };
        self.unfinished_animations = animating;
        self.redraw = RedrawState::WaitingForVBlank {
            redraw_needed: animating,
        };
        timer
    }

    /// Records a render with no page flip. A previously armed fallback is
    /// reused; otherwise the caller receives the delay for one new timer.
    pub fn frame_skipped(&mut self, animating: bool, now: Duration) -> EstimatedVblankTimer {
        self.unfinished_animations = animating;
        match std::mem::take(&mut self.redraw) {
            RedrawState::Queued => {}
            RedrawState::WaitingForEstimatedVBlank(token)
            | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
                self.redraw = RedrawState::WaitingForEstimatedVBlank(token);
                return EstimatedVblankTimer::AlreadyArmed;
            }
            RedrawState::Idle | RedrawState::WaitingForVBlank { .. } => {
                unreachable!("frame_skipped called from an unexpected redraw state")
            }
        }

        let due = self.clock.next_presentation_time(now);
        EstimatedVblankTimer::ArmAfter(due.saturating_sub(now).max(Duration::from_millis(1)))
    }

    pub fn timer_armed(&mut self, token: RegistrationToken) {
        self.redraw = RedrawState::WaitingForEstimatedVBlank(token);
    }

    pub fn estimated_vblank_fired(&mut self) -> Option<String> {
        match std::mem::take(&mut self.redraw) {
            RedrawState::WaitingForEstimatedVBlank(_) if self.unfinished_animations => {
                self.redraw = RedrawState::Queued;
                None
            }
            RedrawState::WaitingForEstimatedVBlank(_) => None,
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => {
                self.redraw = RedrawState::Queued;
                None
            }
            other => Some(format!("{other:?}")),
        }
    }

    /// Resets presentation history after a VT resume and returns any timer
    /// registration that the event loop must remove.
    pub fn reset(&mut self, now: Duration) -> Option<RegistrationToken> {
        let timer = match std::mem::take(&mut self.redraw) {
            RedrawState::WaitingForEstimatedVBlank(token)
            | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => Some(token),
            _ => None,
        };
        self.clock.reset();
        self.last_camera_sample = now;
        self.unfinished_animations = false;
        self.redraw = RedrawState::Queued;
        timer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> OutputFrameState {
        OutputFrameState::new(Duration::from_millis(10))
    }

    #[test]
    fn queued_requests_are_idempotent() {
        let mut state = state();
        state.queue_redraw();
        state.queue_redraw();

        assert!(state.is_redraw_queued());
    }

    #[test]
    fn request_while_waiting_for_vblank_is_not_lost() {
        let mut state = state();
        state.queue_redraw();
        assert_eq!(state.frame_submitted(false), None);

        state.queue_redraw();
        let unexpected = state.on_vblank(None);

        assert_eq!(unexpected, None);
        assert!(state.is_redraw_queued());
    }

    #[test]
    fn unfinished_animation_requests_the_next_real_frame() {
        let mut state = state();
        state.queue_redraw();
        state.frame_submitted(true);

        assert_eq!(state.on_vblank(None), None);
        assert!(state.is_redraw_queued());
    }

    #[test]
    fn settled_frame_returns_to_idle_after_vblank() {
        let mut state = state();
        state.queue_redraw();
        state.frame_submitted(false);

        assert_eq!(state.on_vblank(None), None);
        assert!(!state.is_redraw_queued());
    }

    #[test]
    fn skipped_frame_arms_a_nonzero_fallback_delay() {
        let mut state = state();
        state.queue_redraw();

        assert_eq!(
            state.frame_skipped(false, Duration::from_secs(5)),
            EstimatedVblankTimer::ArmAfter(Duration::from_millis(1))
        );
    }
}
