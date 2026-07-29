use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerDirective {
    Keep,
    Cancel,
    Arm { generation: u64, delay: Duration },
}

/// Presentation-only cursor visibility.
///
/// This state has no access to pointer coordinates, focus, grabs, or
/// constraints, making it impossible for an auto-hide transition to mutate
/// input routing.
pub struct Visibility {
    visible: bool,
    hide_when_typing: bool,
    hide_after: Option<Duration>,
    timer_generation: u64,
}

impl Visibility {
    pub fn new(config: &halley_config::Cursor) -> Self {
        Self {
            visible: true,
            hide_when_typing: config.hide_when_typing,
            hide_after: timeout(config),
            timer_generation: 0,
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn initial_timer(&mut self) -> TimerDirective {
        self.next_timer()
    }

    pub fn pointer_activity(&mut self) -> (bool, TimerDirective) {
        let redraw = !self.visible;
        self.visible = true;
        (redraw, self.next_timer())
    }

    pub fn keyboard_press(&mut self) -> (bool, TimerDirective) {
        if !self.hide_when_typing || !self.visible {
            return (false, TimerDirective::Keep);
        }
        self.visible = false;
        self.timer_generation = self.timer_generation.wrapping_add(1);
        (true, TimerDirective::Cancel)
    }

    pub fn reload(&mut self, config: &halley_config::Cursor) -> TimerDirective {
        self.hide_when_typing = config.hide_when_typing;
        let hide_after = timeout(config);
        if self.hide_after == hide_after {
            return TimerDirective::Keep;
        }
        self.hide_after = hide_after;
        if self.visible {
            self.next_timer()
        } else {
            self.timer_generation = self.timer_generation.wrapping_add(1);
            TimerDirective::Cancel
        }
    }

    pub fn timer_expired(&mut self, generation: u64) -> bool {
        if generation != self.timer_generation || !self.visible || self.hide_after.is_none() {
            return false;
        }
        self.visible = false;
        true
    }

    fn next_timer(&mut self) -> TimerDirective {
        self.timer_generation = self.timer_generation.wrapping_add(1);
        match self.hide_after {
            Some(delay) => TimerDirective::Arm {
                generation: self.timer_generation,
                delay,
            },
            None => TimerDirective::Cancel,
        }
    }
}

fn timeout(config: &halley_config::Cursor) -> Option<Duration> {
    config
        .hide_after_ms
        .map(|milliseconds| Duration::from_millis(u64::from(milliseconds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(hide_when_typing: bool, hide_after_ms: Option<u32>) -> halley_config::Cursor {
        halley_config::Cursor {
            hide_when_typing,
            hide_after_ms,
            ..halley_config::Cursor::default()
        }
    }

    #[test]
    fn typing_policy_hides_on_press_and_pointer_activity_reveals() {
        let mut visibility = Visibility::new(&config(true, Some(2_000)));

        assert!(visibility.keyboard_press().0);
        assert!(!visibility.visible());
        assert!(visibility.pointer_activity().0);
        assert!(visibility.visible());
    }

    #[test]
    fn disabled_typing_policy_does_not_hide() {
        let mut visibility = Visibility::new(&config(false, Some(2_000)));

        assert!(!visibility.keyboard_press().0);
        assert!(visibility.visible());
    }

    #[test]
    fn stale_timeout_cannot_hide_after_new_activity() {
        let mut visibility = Visibility::new(&config(false, Some(2_000)));
        let TimerDirective::Arm {
            generation: stale, ..
        } = visibility.initial_timer()
        else {
            panic!("timeout should be armed");
        };
        let (_, TimerDirective::Arm { generation, .. }) = visibility.pointer_activity() else {
            panic!("activity should replace the timeout");
        };

        assert_ne!(stale, generation);
        assert!(!visibility.timer_expired(stale));
        assert!(visibility.visible());
        assert!(visibility.timer_expired(generation));
        assert!(!visibility.visible());
    }

    #[test]
    fn disabled_inactivity_never_arms_or_expires() {
        let mut visibility = Visibility::new(&config(false, None));

        assert_eq!(visibility.initial_timer(), TimerDirective::Cancel);
        assert!(!visibility.timer_expired(0));
        assert!(visibility.visible());
    }

    #[test]
    fn reload_invalidates_the_previous_timeout() {
        let mut visibility = Visibility::new(&config(false, Some(2_000)));
        let TimerDirective::Arm {
            generation: stale, ..
        } = visibility.initial_timer()
        else {
            panic!("timeout should be armed");
        };

        assert_eq!(
            visibility.reload(&config(false, None)),
            TimerDirective::Cancel
        );
        assert!(!visibility.timer_expired(stale));
    }
}
