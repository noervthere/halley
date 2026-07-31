use std::error::Error;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::pointer::CursorIcon;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};

use super::{CursorFrame, CursorManager, RenderCursor};

smithay::backend::renderer::element::render_elements! {
    pub CursorRenderElement<=GlesRenderer>;
    Named=MemoryRenderBufferRenderElement<GlesRenderer>,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
}

pub fn elements(
    renderer: &mut GlesRenderer,
    manager: &CursorManager,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    pointer_position: (f64, f64),
    time: std::time::Duration,
    presentation_override: Option<CursorIcon>,
) -> Result<Vec<CursorRenderElement>, Box<dyn Error>> {
    match manager.render_cursor_with_override(
        output.current_scale().integer_scale(),
        time,
        presentation_override,
    ) {
        RenderCursor::Hidden => Ok(Vec::new()),
        RenderCursor::Named(frame) => {
            let Some(position) =
                named_cursor_origin(output, output_geometry, pointer_position, &frame)
            else {
                return Ok(Vec::new());
            };
            Ok(vec![
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
            ])
        }
        RenderCursor::Surface { surface, snapshot } => {
            let Some(position) = surface_cursor_origin(
                output,
                output_geometry,
                pointer_position,
                super::surface::hotspot(&surface),
            ) else {
                return Ok(Vec::new());
            };
            if let Some(snapshot) = snapshot {
                return Ok(vec![
                    MemoryRenderBufferRenderElement::from_buffer(
                        renderer,
                        position.to_f64(),
                        &snapshot.buffer,
                        None,
                        None,
                        None,
                        Kind::Cursor,
                    )?
                    .into(),
                ]);
            }
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    position,
                    Scale::from(output.current_scale().fractional_scale()),
                    1.0,
                    Kind::Cursor,
                );
            // A cursor surface can remain alive while losing its committed
            // buffer across a lock, VT switch, suspend, or client teardown.
            // Treat that as stale client state and draw the themed arrow for
            // this frame instead of allowing an empty surface tree to make the
            // cursor disappear indefinitely.
            if elements.is_empty() {
                let frame = manager.default_frame(output.current_scale().integer_scale(), time);
                let Some(position) =
                    named_cursor_origin(output, output_geometry, pointer_position, &frame)
                else {
                    return Ok(Vec::new());
                };
                return Ok(vec![
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
                ]);
            }
            Ok(elements.into_iter().map(Into::into).collect())
        }
    }
}

fn named_cursor_origin(
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    pointer_position: (f64, f64),
    frame: &CursorFrame,
) -> Option<Point<f64, Physical>> {
    let pointer = Point::<f64, Logical>::from(pointer_position);
    if !output_geometry.to_f64().contains(pointer) {
        return None;
    }
    let local = pointer - output_geometry.loc.to_f64();
    let hotspot = Point::<f64, Logical>::from((
        f64::from(frame.hotspot_x) / f64::from(frame.scale.max(1)),
        f64::from(frame.hotspot_y) / f64::from(frame.scale.max(1)),
    ));
    Some((local - hotspot).to_physical(Scale::from(output.current_scale().fractional_scale())))
}

fn surface_cursor_origin(
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    pointer_position: (f64, f64),
    hotspot: Point<i32, Logical>,
) -> Option<Point<i32, Physical>> {
    let pointer = Point::<f64, Logical>::from(pointer_position);
    if !output_geometry.to_f64().contains(pointer) {
        return None;
    }
    Some(
        (pointer - output_geometry.loc.to_f64() - hotspot.to_f64())
            .to_physical_precise_round(Scale::from(output.current_scale().fractional_scale())),
    )
}

#[cfg(test)]
mod tests {
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
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
            named_cursor_origin(
                &output,
                geometry,
                (120.0, 70.0),
                &CursorFrame {
                    buffer: MemoryRenderBuffer::from_slice(
                        &[0, 0, 0, 0],
                        smithay::backend::allocator::Fourcc::Abgr8888,
                        (1, 1),
                        2,
                        Transform::Normal,
                        None,
                    ),
                    metadata_bgra: vec![0, 0, 0, 0].into(),
                    width: 1,
                    height: 1,
                    hotspot_x: 4,
                    hotspot_y: 6,
                    scale: 2,
                }
            ),
            Some(Point::from((36.0, 34.0)))
        );
    }

    #[test]
    fn pointer_outside_output_produces_no_cursor() {
        let output = output(1);
        let geometry = Rectangle::new((0, 0).into(), (1920, 1080).into());

        assert_eq!(
            named_cursor_origin(
                &output,
                geometry,
                (1920.0, 100.0),
                &CursorFrame {
                    buffer: MemoryRenderBuffer::from_slice(
                        &[0, 0, 0, 0],
                        smithay::backend::allocator::Fourcc::Abgr8888,
                        (1, 1),
                        1,
                        Transform::Normal,
                        None,
                    ),
                    metadata_bgra: vec![0, 0, 0, 0].into(),
                    width: 1,
                    height: 1,
                    hotspot_x: 0,
                    hotspot_y: 0,
                    scale: 1,
                }
            ),
            None
        );
    }

    #[test]
    fn secondary_output_localizes_global_pointer_coordinates() {
        let output = output(1);
        let geometry = Rectangle::new((2560, 0).into(), (1920, 1200).into());

        assert_eq!(
            named_cursor_origin(
                &output,
                geometry,
                (3000.0, 500.0),
                &CursorFrame {
                    buffer: MemoryRenderBuffer::from_slice(
                        &[0, 0, 0, 0],
                        smithay::backend::allocator::Fourcc::Abgr8888,
                        (1, 1),
                        1,
                        Transform::Normal,
                        None,
                    ),
                    metadata_bgra: vec![0, 0, 0, 0].into(),
                    width: 1,
                    height: 1,
                    hotspot_x: 0,
                    hotspot_y: 0,
                    scale: 1,
                }
            ),
            Some(Point::from((440.0, 500.0)))
        );
    }
}
