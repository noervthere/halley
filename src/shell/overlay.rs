use std::path::Path;
use std::time::Duration;

const TRANSITION_DURATION: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationKind {
    Success,
    Error,
}

#[derive(Clone, Debug)]
struct Notification {
    output: String,
    message: String,
    kind: NotificationKind,
    shown_at: Duration,
    enter_from: f32,
    expires_at: Duration,
    dismissed: Option<(Duration, f32)>,
    expiry_notified: bool,
}

impl Notification {
    fn mix(&self, now: Duration) -> f32 {
        if let Some((dismissed_at, from)) = self.dismissed {
            return from * (1.0 - transition_progress(now.saturating_sub(dismissed_at)));
        }
        if now >= self.expires_at {
            return 1.0 - transition_progress(now.saturating_sub(self.expires_at));
        }
        self.enter_from
            + (1.0 - self.enter_from) * transition_progress(now.saturating_sub(self.shown_at))
    }

    fn finished(&self, now: Duration) -> bool {
        let end = self.dismissed.map(|(at, _)| at).unwrap_or(self.expires_at) + TRANSITION_DURATION;
        now >= end
    }

    fn animating(&self, now: Duration) -> bool {
        if self.finished(now) {
            return false;
        }
        now < self.shown_at + TRANSITION_DURATION
            || self.dismissed.is_some()
            || now >= self.expires_at
    }
}

#[derive(Clone, Debug)]
struct ExitConfirmation {
    opened_at: Duration,
    closing: Option<(Duration, f32)>,
}

impl ExitConfirmation {
    fn mix(&self, now: Duration) -> f32 {
        self.closing.map_or_else(
            || transition_progress(now.saturating_sub(self.opened_at)),
            |(closed_at, from)| from * (1.0 - transition_progress(now.saturating_sub(closed_at))),
        )
    }

    fn finished(&self, now: Duration) -> bool {
        self.closing
            .is_some_and(|(at, _)| now >= at + TRANSITION_DURATION)
    }

    fn animating(&self, now: Duration) -> bool {
        !self.finished(now)
            && (now < self.opened_at + TRANSITION_DURATION || self.closing.is_some())
    }
}

#[derive(Clone, Debug, Default)]
pub struct OverlayManager {
    exit: Option<ExitConfirmation>,
    notification: Option<Notification>,
}

#[derive(Clone, Debug)]
pub struct NotificationSnapshot {
    pub message: String,
    pub kind: NotificationKind,
    pub mix: f32,
}

#[derive(Clone, Debug, Default)]
pub struct OverlaySnapshot {
    pub exit_mix: Option<f32>,
    pub notification: Option<NotificationSnapshot>,
}

impl OverlayManager {
    pub fn exit_modal_active(&self) -> bool {
        self.exit
            .as_ref()
            .is_some_and(|confirmation| confirmation.closing.is_none())
    }

    pub fn show_exit(&mut self, now: Duration) -> bool {
        if self.exit_modal_active() {
            return false;
        }
        self.exit = Some(ExitConfirmation {
            opened_at: now,
            closing: None,
        });
        true
    }

    pub fn cancel_exit(&mut self, now: Duration) -> bool {
        let Some(exit) = self.exit.as_mut() else {
            return false;
        };
        if exit.closing.is_some() {
            return false;
        }
        exit.closing = Some((now, exit.mix(now)));
        true
    }

    pub fn show_config_success(
        &mut self,
        output: String,
        path: &Path,
        duration_ms: u64,
        now: Duration,
    ) {
        self.show_notification(
            output,
            format!("Configuration successfully loaded from {}", path.display()),
            NotificationKind::Success,
            duration_ms,
            now,
        );
    }

    pub fn show_config_error(&mut self, output: String, duration_ms: u64, now: Duration) {
        self.show_notification(
            output,
            "Current configuration was unable to load properly. Run `halleyctl config verify` to see why."
                .to_string(),
            NotificationKind::Error,
            duration_ms,
            now,
        );
    }

    pub fn show_screenshot_saved(
        &mut self,
        output: String,
        directory: &Path,
        duration_ms: u64,
        now: Duration,
    ) {
        self.show_notification(
            output,
            format!("Screenshot saved to {}", directory.display()),
            NotificationKind::Success,
            duration_ms,
            now,
        );
    }

    pub fn show_error(
        &mut self,
        output: String,
        message: impl Into<String>,
        duration_ms: u64,
        now: Duration,
    ) {
        self.show_notification(
            output,
            message.into(),
            NotificationKind::Error,
            duration_ms,
            now,
        );
    }

    pub fn clear_config_error(&mut self, now: Duration) -> bool {
        let Some(notification) = self.notification.as_mut() else {
            return false;
        };
        if notification.kind != NotificationKind::Error || notification.dismissed.is_some() {
            return false;
        }
        notification.dismissed = Some((now, notification.mix(now)));
        true
    }

    fn show_notification(
        &mut self,
        output: String,
        message: String,
        kind: NotificationKind,
        duration_ms: u64,
        now: Duration,
    ) {
        let enter_from = self
            .notification
            .as_ref()
            .filter(|notification| !notification.finished(now))
            .map(|notification| notification.mix(now))
            .unwrap_or(0.0);
        self.notification = Some(Notification {
            output,
            message,
            kind,
            shown_at: now,
            enter_from,
            expires_at: now + Duration::from_millis(duration_ms.max(1)),
            dismissed: None,
            expiry_notified: false,
        });
    }

    pub fn snapshot(&self, output: &str, now: Duration) -> OverlaySnapshot {
        OverlaySnapshot {
            exit_mix: self
                .exit
                .as_ref()
                .map(|confirmation| confirmation.mix(now))
                .filter(|mix| *mix > 0.001),
            notification: self.notification.as_ref().and_then(|notification| {
                (notification.output == output && !notification.finished(now)).then(|| {
                    NotificationSnapshot {
                        message: notification.message.clone(),
                        kind: notification.kind,
                        mix: notification.mix(now),
                    }
                })
            }),
        }
    }

    pub fn animating(&self, now: Duration) -> bool {
        self.exit
            .as_ref()
            .is_some_and(|confirmation| confirmation.animating(now))
            || self
                .notification
                .as_ref()
                .is_some_and(|notification| notification.animating(now))
    }

    /// Timer-side lifecycle update. Returns true when a frame must be queued
    /// to begin an expiry fade or erase a completed overlay.
    pub fn wakeup(&mut self, now: Duration) -> bool {
        let mut redraw = false;
        if let Some(notification) = self.notification.as_mut()
            && notification.dismissed.is_none()
            && now >= notification.expires_at
            && !notification.expiry_notified
        {
            notification.expiry_notified = true;
            redraw = true;
        }
        if self
            .notification
            .as_ref()
            .is_some_and(|notification| notification.finished(now))
        {
            self.notification = None;
            redraw = true;
        }
        if self
            .exit
            .as_ref()
            .is_some_and(|confirmation| confirmation.finished(now))
        {
            self.exit = None;
            redraw = true;
        }
        redraw
    }
}

fn transition_progress(elapsed: Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / TRANSITION_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_error_replaces_message_lifetime_without_flashing_out() {
        let mut overlays = OverlayManager::default();
        overlays.show_config_error("DP-1".into(), 9_000, Duration::ZERO);
        let halfway = Duration::from_millis(100);
        let before = overlays.snapshot("DP-1", halfway).notification.unwrap().mix;

        overlays.show_config_error("DP-1".into(), 9_000, halfway);
        let after = overlays.snapshot("DP-1", halfway).notification.unwrap().mix;

        assert_eq!(before, after);
        assert!(after > 0.0);
    }

    #[test]
    fn valid_reload_only_dismisses_an_error() {
        let mut overlays = OverlayManager::default();
        overlays.show_config_success(
            "DP-1".into(),
            Path::new("/tmp/halley.rune"),
            4_000,
            Duration::ZERO,
        );
        assert!(!overlays.clear_config_error(Duration::from_millis(10)));

        overlays.show_config_error("DP-1".into(), 9_000, Duration::from_millis(20));
        assert!(overlays.clear_config_error(Duration::from_millis(30)));
    }

    #[test]
    fn screenshot_success_names_the_destination_directory() {
        let mut overlays = OverlayManager::default();
        overlays.show_screenshot_saved(
            "DP-1".into(),
            Path::new("/home/test/Pictures/Screenshots"),
            4_000,
            Duration::ZERO,
        );

        let notification = overlays
            .snapshot("DP-1", Duration::from_millis(180))
            .notification
            .unwrap();
        assert_eq!(
            notification.message,
            "Screenshot saved to /home/test/Pictures/Screenshots"
        );
        assert_eq!(notification.kind, NotificationKind::Success);
    }

    #[test]
    fn exit_confirmation_is_idempotent_and_modal_only_while_open() {
        let mut overlays = OverlayManager::default();
        assert!(overlays.show_exit(Duration::ZERO));
        assert!(!overlays.show_exit(Duration::from_millis(1)));
        assert!(overlays.exit_modal_active());
        assert!(overlays.cancel_exit(Duration::from_millis(20)));
        assert!(!overlays.exit_modal_active());
    }
}
