use smithay::utils::{Logical, Rectangle, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MapAdmission {
    Wait,
    Admit,
    Ignore,
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
    pub(super) fn from_geometry(geometry: Rectangle<i32, Logical>) -> Self {
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
}

#[cfg(test)]
mod tests {
    use smithay::utils::Rectangle;

    use super::{MapAdmission, OpeningPlacement, map_admission};

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
        let placement = OpeningPlacement::from_geometry(preliminary);

        assert_eq!(
            placement.centered((1920, 1080).into()),
            Rectangle::new((320, 180).into(), (1920, 1080).into())
        );
        assert_eq!(
            placement.centered((1280, 720).into()),
            Rectangle::new((640, 360).into(), (1280, 720).into())
        );
    }
}
