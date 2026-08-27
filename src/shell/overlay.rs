use std::collections::HashMap;
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

#[derive(Clone, Debug)]
struct ZoomIndicator {
    scale: f32,
    shown_at: Duration,
    enter_from: f32,
    expires_at: Duration,
    fade_duration: Duration,
    expiry_notified: bool,
}

impl ZoomIndicator {
    fn mix(&self, now: Duration) -> f32 {
        if now >= self.expires_at {
            return 1.0
                - transition_progress_for(now.saturating_sub(self.expires_at), self.fade_duration);
        }
        self.enter_from
            + (1.0 - self.enter_from)
                * transition_progress_for(now.saturating_sub(self.shown_at), self.fade_duration)
    }

    fn finished(&self, now: Duration) -> bool {
        now >= self.expires_at + self.fade_duration
    }

    fn animating(&self, now: Duration) -> bool {
        !self.finished(now)
            && (self.enter_from < 0.999 && now < self.shown_at + self.fade_duration
                || now >= self.expires_at)
    }
}

#[derive(Clone, Debug, Default)]
pub struct OverlayManager {
    exit: Option<ExitConfirmation>,
    notification: Option<Notification>,
    zoom_indicators: HashMap<String, ZoomIndicator>,
}

#[derive(Clone, Debug)]
pub struct NotificationSnapshot {
    pub message: String,
    pub kind: NotificationKind,
    pub mix: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct ZoomIndicatorSnapshot {
    pub scale: f32,
    pub mix: f32,
}

#[derive(Clone, Debug, Default)]
pub struct OverlaySnapshot {
    pub exit_mix: Option<f32>,
    pub notification: Option<NotificationSnapshot>,
    pub zoom_indicator: Option<ZoomIndicatorSnapshot>,
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

    pub fn show_zoom_indicator(
        &mut self,
        output: &str,
        scale: f32,
        config: &halley_config::ZoomIndicator,
        now: Duration,
    ) -> bool {
        if !config.enabled {
            return self.zoom_indicators.remove(output).is_some();
        }

        let (shown_at, enter_from) = self
            .zoom_indicators
            .get(output)
            .filter(|indicator| !indicator.finished(now))
            .map(|indicator| {
                if now >= indicator.expires_at {
                    (now, indicator.mix(now))
                } else {
                    (indicator.shown_at, indicator.enter_from)
                }
            })
            .unwrap_or((now, 1.0));
        self.zoom_indicators.insert(
            output.to_string(),
            ZoomIndicator {
                scale,
                shown_at,
                enter_from,
                expires_at: now + Duration::from_millis(config.hold_duration_ms),
                fade_duration: Duration::from_millis(config.fade_duration_ms),
                expiry_notified: false,
            },
        );
        true
    }

    pub fn reload_zoom_indicator(&mut self, config: &halley_config::ZoomIndicator) -> bool {
        if config.enabled || self.zoom_indicators.is_empty() {
            return false;
        }
        self.zoom_indicators.clear();
        true
    }

    pub fn remove_output(&mut self, output: &str) {
        self.zoom_indicators.remove(output);
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
            zoom_indicator: self.zoom_indicators.get(output).and_then(|indicator| {
                (!indicator.finished(now)).then(|| ZoomIndicatorSnapshot {
                    scale: indicator.scale,
                    mix: indicator.mix(now),
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
            || self
                .zoom_indicators
                .values()
                .any(|indicator| indicator.animating(now))
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
        for indicator in self.zoom_indicators.values_mut() {
            if now >= indicator.expires_at && !indicator.expiry_notified {
                indicator.expiry_notified = true;
                redraw = true;
            }
        }
        let before = self.zoom_indicators.len();
        self.zoom_indicators
            .retain(|_, indicator| !indicator.finished(now));
        redraw |= self.zoom_indicators.len() != before;
        redraw
    }
}

fn transition_progress(elapsed: Duration) -> f32 {
    transition_progress_for(elapsed, TRANSITION_DURATION)
}

fn transition_progress_for(elapsed: Duration, duration: Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
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

    #[test]
    fn zoom_indicator_holds_after_activity_then_fades() {
        let mut overlays = OverlayManager::default();
        let config = halley_config::ZoomIndicator::default();
        overlays.show_zoom_indicator("DP-1", 0.75, &config, Duration::ZERO);

        let initial = overlays
            .snapshot("DP-1", Duration::ZERO)
            .zoom_indicator
            .unwrap();
        assert_eq!(initial.scale, 0.75);
        assert_eq!(initial.mix, 1.0);
        assert_eq!(
            overlays
                .snapshot("DP-1", Duration::from_millis(749))
                .zoom_indicator
                .unwrap()
                .mix,
            1.0
        );
        assert!(
            overlays
                .snapshot("DP-1", Duration::from_millis(840))
                .zoom_indicator
                .unwrap()
                .mix
                < 1.0
        );
        assert!(
            overlays
                .snapshot("DP-1", Duration::from_millis(930))
                .zoom_indicator
                .is_none()
        );

        let mut overlays = OverlayManager::default();
        overlays.show_zoom_indicator("DP-1", 0.75, &config, Duration::ZERO);
        assert!(!overlays.animating(Duration::from_millis(749)));
        assert!(overlays.wakeup(Duration::from_millis(750)));
        assert!(overlays.animating(Duration::from_millis(750)));
        assert!(overlays.wakeup(Duration::from_millis(930)));
    }

    #[test]
    fn repeated_zoom_activity_extends_the_hold_and_updates_the_live_scale() {
        let mut overlays = OverlayManager::default();
        let config = halley_config::ZoomIndicator::default();
        overlays.show_zoom_indicator("DP-1", 0.90, &config, Duration::ZERO);
        overlays.show_zoom_indicator("DP-1", 0.70, &config, Duration::from_millis(700));

        let snapshot = overlays
            .snapshot("DP-1", Duration::from_millis(1_000))
            .zoom_indicator
            .unwrap();
        assert_eq!(snapshot.scale, 0.70);
        assert_eq!(snapshot.mix, 1.0);
    }

    #[test]
    fn activity_during_fade_reverses_without_flashing_or_affecting_other_outputs() {
        let mut overlays = OverlayManager::default();
        let config = halley_config::ZoomIndicator::default();
        overlays.show_zoom_indicator("DP-1", 0.80, &config, Duration::ZERO);
        overlays.show_zoom_indicator("DP-2", 0.60, &config, Duration::ZERO);
        let reactivated_at = Duration::from_millis(840);
        let before = overlays
            .snapshot("DP-1", reactivated_at)
            .zoom_indicator
            .unwrap()
            .mix;

        overlays.show_zoom_indicator("DP-1", 0.75, &config, reactivated_at);
        let after = overlays
            .snapshot("DP-1", reactivated_at)
            .zoom_indicator
            .unwrap();

        assert_eq!(after.mix, before);
        assert_eq!(after.scale, 0.75);
        assert_eq!(
            overlays
                .snapshot("DP-2", reactivated_at)
                .zoom_indicator
                .unwrap()
                .scale,
            0.60
        );
        assert!(
            overlays
                .snapshot("DP-1", Duration::from_millis(900))
                .zoom_indicator
                .unwrap()
                .mix
                > before
        );
    }

    #[test]
    fn disabling_or_removing_an_output_clears_zoom_indicator_state() {
        let mut overlays = OverlayManager::default();
        let mut config = halley_config::ZoomIndicator::default();
        overlays.show_zoom_indicator("DP-1", 0.75, &config, Duration::ZERO);
        overlays.remove_output("DP-1");
        assert!(
            overlays
                .snapshot("DP-1", Duration::ZERO)
                .zoom_indicator
                .is_none()
        );

        overlays.show_zoom_indicator("DP-1", 0.75, &config, Duration::ZERO);
        config.enabled = false;
        assert!(overlays.reload_zoom_indicator(&config));
        assert!(
            overlays
                .snapshot("DP-1", Duration::ZERO)
                .zoom_indicator
                .is_none()
        );

        assert!(!overlays.show_zoom_indicator("DP-1", 0.75, &config, Duration::ZERO));
        assert!(
            overlays
                .snapshot("DP-1", Duration::ZERO)
                .zoom_indicator
                .is_none()
        );
    }
}
