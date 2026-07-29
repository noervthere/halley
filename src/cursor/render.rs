use std::error::Error;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};

use super::CursorFrame;

smithay::backend::renderer::element::render_elements! {
    pub CursorRenderElement<=GlesRenderer>;
    Named=MemoryRenderBufferRenderElement<GlesRenderer>,
}

pub fn named_element(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    pointer_position: (f64, f64),
    frame: &CursorFrame,
) -> Result<Option<CursorRenderElement>, Box<dyn Error>> {
    let Some(position) = cursor_origin(
        output,
        output_geometry,
        pointer_position,
        frame.hotspot_x,
        frame.hotspot_y,
        frame.scale,
    ) else {
        return Ok(None);
    };
    Ok(Some(
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            position,
            &frame.buffer,
            None,
            None,
            None,
            Kind::Cursor,
        )?
        .into(),
    ))
}

fn cursor_origin(
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    pointer_position: (f64, f64),
    hotspot_x: i32,
    hotspot_y: i32,
    cursor_scale: i32,
) -> Option<Point<f64, Physical>> {
    let pointer = Point::<f64, Logical>::from(pointer_position);
    if !output_geometry.to_f64().contains(pointer) {
        return None;
    }
    let local = pointer - output_geometry.loc.to_f64();
    let hotspot = Point::<f64, Logical>::from((
        f64::from(hotspot_x) / f64::from(cursor_scale.max(1)),
        f64::from(hotspot_y) / f64::from(cursor_scale.max(1)),
    ));
    Some((local - hotspot).to_physical(Scale::from(output.current_scale().fractional_scale())))
}

#[cfg(test)]
mod tests {
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    use smithay::utils::{Physical, Size, Transform};

    use super::*;

    fn output(scale: i32) -> Output {
        let output = Output::new(
            "test".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
                serial_number: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: Size::<i32, Physical>::from((1920, 1080)),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            Some(smithay::output::Scale::Integer(scale)),
            Some((0, 0).into()),
        );
        output
    }

    #[test]
    fn hotspot_is_removed_in_logical_then_output_physical_space() {
        let output = output(2);
        let geometry = Rectangle::new((100, 50).into(), (960, 540).into());

        assert_eq!(
            cursor_origin(&output, geometry, (120.0, 70.0), 4, 6, 2),
            Some(Point::from((36.0, 34.0)))
        );
    }

    #[test]
    fn pointer_outside_output_produces_no_cursor() {
        let output = output(1);
        let geometry = Rectangle::new((0, 0).into(), (1920, 1080).into());

        assert_eq!(
            cursor_origin(&output, geometry, (1920.0, 100.0), 0, 0, 1),
            None
        );
    }
}
