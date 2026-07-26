pub mod picker;
mod screenshot;

use smithay::desktop::{Space, Window};
use smithay::utils::{Logical, Point, Rectangle};

use picker::RegionPicker;
pub use screenshot::save_region;

#[derive(Debug, Default)]
pub struct CaptureState {
    picker: RegionPicker,
}

impl CaptureState {
    pub fn is_active(&self) -> bool {
        self.picker.is_active()
    }

    pub fn region(&self) -> Option<Rectangle<i32, Logical>> {
        self.picker.region()
    }

    pub fn begin_region(&mut self, space: &Space<Window>, preferred_output: Option<&str>) -> bool {
        let bounds = space
            .outputs()
            .filter_map(|output| space.output_geometry(output))
            .reduce(Rectangle::merge);
        let active = preferred_output
            .and_then(|name| {
                space
                    .outputs()
                    .find(|output| output.name() == name)
                    .and_then(|output| space.output_geometry(output))
            })
            .or_else(|| {
                space
                    .outputs()
                    .next()
                    .and_then(|output| space.output_geometry(output))
            });
        let (Some(bounds), Some(active)) = (bounds, active) else {
            return false;
        };
        self.picker.begin(bounds, active);
        true
    }

    pub fn update_layout(&mut self, space: &Space<Window>) {
        if let Some(bounds) = space
            .outputs()
            .filter_map(|output| space.output_geometry(output))
            .reduce(Rectangle::merge)
        {
            self.picker.update_bounds(bounds);
        }
    }

    pub fn press(&mut self, position: (f64, f64)) -> bool {
        self.picker.press(Point::from(position))
    }

    pub fn motion(&mut self, position: (f64, f64)) -> bool {
        self.picker.motion(Point::from(position))
    }

    pub fn release(&mut self) -> bool {
        self.picker.release()
    }

    pub fn accept(&mut self) -> Option<Rectangle<i32, Logical>> {
        self.picker.accept()
    }

    pub fn cancel(&mut self) -> bool {
        self.picker.cancel()
    }

    pub fn remember_successful(&mut self, region: Rectangle<i32, Logical>) {
        self.picker.remember_successful(region);
    }
}
