use std::error::Error;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, Color32F, ContextId, Frame, Offscreen, Renderer, Texture};
use smithay::desktop::Window;
use smithay::utils::{Physical, Rectangle, Transform};
use smithay::wayland::seat::WaylandFocus;

#[derive(Debug)]
pub struct WindowTexture {
    pub(crate) texture: GlesTexture,
    pub(crate) context: ContextId<GlesTexture>,
}

impl WindowTexture {
    pub fn render_element(
        &self,
        id: Id,
        destination: Rectangle<i32, Physical>,
        alpha: f32,
    ) -> TextureRenderElement<GlesTexture> {
        let source = Rectangle::from_size(
            self.texture
                .size()
                .to_logical(1, Transform::Normal)
                .to_f64(),
        );
        TextureRenderElement::from_static_texture(
            id,
            self.context.clone(),
            destination.loc.to_f64(),
            self.texture.clone(),
            1,
            Transform::Normal,
            Some(alpha.clamp(0.0, 1.0)),
            Some(source),
            Some(destination.size.to_logical(1)),
            None,
            Kind::Unspecified,
        )
    }
}

pub fn capture(
    renderer: &mut GlesRenderer,
    window: &Window,
    reusable: Option<GlesTexture>,
) -> Result<WindowTexture, Box<dyn Error>> {
    let surface = window
        .wl_surface()
        .ok_or("window snapshot has no surface")?;
    let geometry = window.geometry();
    if geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Err("window snapshot has empty geometry".into());
    }

    let size = geometry.size.to_physical(1);
    let location = smithay::utils::Point::from((-geometry.loc.x, -geometry.loc.y)).to_physical(1);
    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            renderer,
            surface.as_ref(),
            location,
            1.0,
            1.0,
            Kind::Unspecified,
        );
    if elements.is_empty() {
        return Err("window snapshot surface tree is empty".into());
    }

    let context = renderer.context_id();
    let buffer_size = geometry.size.to_buffer(1, Transform::Normal);
    let mut reusable = reusable;
    let can_reuse = reusable
        .as_mut()
        .is_some_and(|texture| texture.size() == buffer_size && texture.is_unique_reference());
    let mut texture = if can_reuse {
        reusable.expect("reusable texture checked above")
    } else {
        <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            buffer_size,
        )?
    };
    let damage = Rectangle::<i32, Physical>::from_size(size);
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
        frame.clear(Color32F::TRANSPARENT, &[damage])?;
        draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
        let _ = frame.finish()?;
    }

    Ok(WindowTexture { texture, context })
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Buffer, Logical};

    #[test]
    fn texture_mapping_preserves_the_full_source() {
        let size = smithay::utils::Size::<i32, Buffer>::from((800, 600));
        let source = Rectangle::from_size(size.to_logical(1, Transform::Normal).to_f64());

        assert_eq!(
            source,
            Rectangle::<f64, Logical>::from_size((800.0, 600.0).into())
        );
    }
}
