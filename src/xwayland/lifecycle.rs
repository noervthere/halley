use std::collections::HashMap;

use smithay::utils::{Logical, Rectangle, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MapAdmission {
    Wait,
    Admit,
    Ignore,
}

/// Fullscreen changes received during the opening animation are client intent,
/// not separate presentation steps. Keeping only the latest value prevents
/// startup state churn from replaying as multiple visible transitions.
#[derive(Default)]
pub(super) struct OpeningFullscreenIntents {
    desired_by_window: HashMap<u32, bool>,
}

impl OpeningFullscreenIntents {
    pub(super) fn update(&mut self, xid: u32, fullscreen: bool) {
        self.desired_by_window.insert(xid, fullscreen);
    }

    pub(super) fn take(&mut self, xid: u32) -> Option<bool> {
        self.desired_by_window.remove(&xid)
    }

    pub(super) fn get(&self, xid: u32) -> Option<bool> {
        self.desired_by_window.get(&xid).copied()
    }

    pub(super) fn remove(&mut self, xid: u32) {
        self.desired_by_window.remove(&xid);
    }

    pub(super) fn clear(&mut self) {
        self.desired_by_window.clear();
    }
}

pub(super) fn map_admission(
    pending: bool,
    surface_associated: bool,
    has_buffer: bool,
) -> MapAdmission {
    if !pending {
        MapAdmission::Ignore
    } else if surface_associated && has_buffer {
        MapAdmission::Admit
    } else {
        MapAdmission::Wait
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OpeningPlacement {
    center_x_twice: i32,
    center_y_twice: i32,
}

impl OpeningPlacement {
    pub(super) fn preferred_size(
        initial_size: Size<i32, Logical>,
        fallback: Size<i32, Logical>,
    ) -> Size<i32, Logical> {
        Self::valid_initial_size(initial_size).unwrap_or(fallback)
    }

    pub(super) fn new(geometry: Rectangle<i32, Logical>) -> Self {
        Self {
            center_x_twice: geometry.loc.x * 2 + geometry.size.w,
            center_y_twice: geometry.loc.y * 2 + geometry.size.h,
        }
    }

    pub(super) fn centered(self, size: Size<i32, Logical>) -> Rectangle<i32, Logical> {
        Rectangle::new(
            (
                (self.center_x_twice - size.w).div_euclid(2),
                (self.center_y_twice - size.h).div_euclid(2),
            )
                .into(),
            size,
        )
    }

    fn valid_initial_size(size: Size<i32, Logical>) -> Option<Size<i32, Logical>> {
        (size.w > 0 && size.h > 0).then_some(size)
    }
}

#[cfg(test)]
mod tests {
    use smithay::utils::Rectangle;

    use super::{MapAdmission, OpeningFullscreenIntents, OpeningPlacement, map_admission};

    #[test]
    fn pending_window_waits_for_surface_and_buffer() {
        assert_eq!(map_admission(true, false, false), MapAdmission::Wait);
        assert_eq!(map_admission(true, true, false), MapAdmission::Wait);
        assert_eq!(map_admission(true, false, true), MapAdmission::Wait);
        assert_eq!(map_admission(true, true, true), MapAdmission::Admit);
    }

    #[test]
    fn admitted_or_unknown_window_cannot_be_admitted_again() {
        assert_eq!(map_admission(false, true, true), MapAdmission::Ignore);
    }

    #[test]
    fn opening_size_changes_preserve_the_original_center() {
        let preliminary = Rectangle::new((1264, 704).into(), (32, 32).into());
        let placement = OpeningPlacement::new(preliminary);

        assert_eq!(
            placement.centered((1920, 1080).into()),
            Rectangle::new((320, 180).into(), (1920, 1080).into())
        );
        assert_eq!(
            placement.centered((1280, 720).into()),
            Rectangle::new((640, 360).into(), (1280, 720).into())
        );
    }

    #[test]
    fn client_size_wins_over_a_later_output_sized_buffer() {
        assert_eq!(
            OpeningPlacement::preferred_size((640, 480).into(), (2560, 1440).into()),
            (640, 480).into()
        );
        assert_eq!(
            OpeningPlacement::preferred_size((0, 0).into(), (2560, 1440).into()),
            (2560, 1440).into()
        );
    }

    #[test]
    fn opening_fullscreen_churn_collapses_to_the_latest_intent() {
        let mut intents = OpeningFullscreenIntents::default();

        intents.update(42, true);
        intents.update(42, false);
        intents.update(42, true);

        assert_eq!(intents.get(42), Some(true));
        assert_eq!(intents.take(42), Some(true));
        assert_eq!(intents.take(42), None);
    }

    #[test]
    fn opening_fullscreen_intents_are_independent_per_window() {
        let mut intents = OpeningFullscreenIntents::default();

        intents.update(10, true);
        intents.update(20, false);
        intents.remove(10);

        assert_eq!(intents.take(10), None);
        assert_eq!(intents.take(20), Some(false));
    }
}
