use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};

use super::{Session, SessionDriver};
use crate::cursor::{TimerDirective, Visibility};

/// Event-loop adapter for the pure cursor visibility policy.
///
/// Timer ownership stays here so neither raw input dispatch nor the cursor
/// theme manager needs to know about calloop registration tokens.
pub(super) struct Policy<D: SessionDriver> {
    visibility: Visibility,
    loop_handle: LoopHandle<'static, Session<D>>,
    timer: Option<RegistrationToken>,
}

impl<D: SessionDriver> Policy<D> {
    pub fn new(
        config: &halley_config::Cursor,
        loop_handle: LoopHandle<'static, Session<D>>,
    ) -> Self {
        let mut policy = Self {
            visibility: Visibility::new(config),
            loop_handle,
            timer: None,
        };
        let directive = policy.visibility.initial_timer();
        policy.apply_timer(directive);
        policy
    }

    pub fn visible(&self) -> bool {
        self.visibility.visible()
    }

    pub fn pointer_activity(&mut self) -> bool {
        let (redraw, directive) = self.visibility.pointer_activity();
        self.apply_timer(directive);
        redraw
    }

    pub fn keyboard_press(&mut self) -> bool {
        let (redraw, directive) = self.visibility.keyboard_press();
        self.apply_timer(directive);
        redraw
    }

    pub fn reload(&mut self, config: &halley_config::Cursor) {
        let directive = self.visibility.reload(config);
        self.apply_timer(directive);
    }

    fn apply_timer(&mut self, directive: TimerDirective) {
        if directive == TimerDirective::Keep {
            return;
        }
        if let Some(token) = self.timer.take() {
            self.loop_handle.remove(token);
        }
        let TimerDirective::Arm { generation, delay } = directive else {
            return;
        };
        match self
            .loop_handle
            .insert_source(Timer::from_duration(delay), move |_, _, session| {
                session.cursor_policy.timer = None;
                if session.cursor_policy.visibility.timer_expired(generation) {
                    session.request_redraw();
                }
                TimeoutAction::Drop
            }) {
            Ok(token) => self.timer = Some(token),
            Err(err) => eventline::warn!("cursor: failed to arm inactivity timer: {err}"),
        }
    }
}
