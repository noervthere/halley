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
use smithay::utils::{Logical, Physical, Rectangle, Transform};
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
    capture_surface_elements(renderer, geometry, &elements, reusable)
}

fn capture_surface_elements(
    renderer: &mut GlesRenderer,
    geometry: Rectangle<i32, Logical>,
    elements: &[WaylandSurfaceRenderElement<GlesRenderer>],
    reusable: Option<GlesTexture>,
) -> Result<WindowTexture, Box<dyn Error>> {
    if elements.is_empty() {
        return Err("window snapshot surface tree is empty".into());
    }

    let size = geometry.size.to_physical(1);
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
        draw_render_elements(&mut frame, 1.0, elements, &[damage])?;
        let _ = frame.finish()?;
    }

    Ok(WindowTexture { texture, context })
}

#[allow(clippy::too_many_arguments)]
pub fn capture_decorated(
    renderer: &mut GlesRenderer,
    window: &Window,
    reusable: Option<GlesTexture>,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
    focused: bool,
    chrome_visible: bool,
    maximized: bool,
    titlebar_renderer: &mut crate::render::titlebar::TitlebarRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
) -> Result<WindowTexture, Box<dyn Error>> {
    capture_decorated_inner(
        renderer,
        window,
        reusable,
        decorations,
        font,
        focused,
        chrome_visible,
        maximized,
        titlebar_renderer,
        window_decoration_renderer,
        node_renderer,
        ui_text,
        |renderer, reusable| capture(renderer, window, reusable),
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_decorated_inner(
    renderer: &mut GlesRenderer,
    window: &Window,
    reusable: Option<GlesTexture>,
    decorations: &halley_config::Decorations,
    font: &halley_config::Font,
    focused: bool,
    chrome_visible: bool,
    maximized: bool,
    titlebar_renderer: &mut crate::render::titlebar::TitlebarRenderer,
    window_decoration_renderer: &mut crate::render::window_decoration::WindowDecorationRenderer,
    node_renderer: &mut crate::render::node::NodeRenderer,
    ui_text: &mut crate::render::text::UiTextRenderer,
    capture_client: impl FnOnce(
        &mut GlesRenderer,
        Option<GlesTexture>,
    ) -> Result<WindowTexture, Box<dyn Error>>,
) -> Result<WindowTexture, Box<dyn Error>> {
    if !chrome_visible || crate::xwayland::is_override_redirect(window) {
        return capture_client(renderer, reusable);
    }
    let client = capture_client(renderer, None)?;

    let client_size = client.texture.size().to_logical(1, Transform::Normal);
    let native_client = Rectangle::<i32, Logical>::from_size(client_size);
    let chrome = crate::titlebar::WindowChrome::for_window(window, decorations, font);
    let native_outer = chrome.outer_rect(native_client);
    let content = Rectangle::<i32, Physical>::new(
        (-native_outer.loc.x, -native_outer.loc.y).into(),
        client_size.to_physical(1),
    );
    let output_size = native_outer.size.to_physical(1);
    let damage = Rectangle::<i32, Physical>::from_size(output_size);
    let server_titlebar = chrome.has_server_titlebar();
    let border_width = chrome.border_width;
    let content_radius = decorations.border_radius_px.max(0) as f32;
    let rounded = content_radius > 0.0 && window_decoration_renderer.available(renderer);
    let source = Rectangle::<f64, Logical>::from_size(client_size.to_f64());
    let base = TextureRenderElement::from_static_texture(
        Id::new(),
        client.context.clone(),
        content.loc.to_f64(),
        client.texture.clone(),
        1,
        Transform::Normal,
        Some(1.0),
        Some(source),
        Some(content.size.to_logical(1)),
        None,
        Kind::Unspecified,
    );
    let mut elements = Vec::new();
    if server_titlebar {
        crate::render::scene::append_titlebar_elements(
            renderer,
            window,
            Some("snapshot"),
            content,
            crate::titlebar::effective_height(&decorations.titlebars, font.size),
            crate::titlebar::glyph_size(crate::titlebar::effective_height(
                &decorations.titlebars,
                font.size,
            )),
            1.0,
            maximized,
            border_width,
            decorations.titlebars.radius_px.max(0) as f32,
            focused,
            1.0,
            decorations,
            None,
            None,
            titlebar_renderer,
            window_decoration_renderer,
            node_renderer,
            ui_text,
            &mut elements,
        )?;
    }
    if rounded {
        let radii = if server_titlebar {
            crate::render::window_decoration::CornerRadii::bottom(content_radius)
        } else {
            crate::render::window_decoration::CornerRadii::all(content_radius)
        };
        let element = window_decoration_renderer
            .texture_element_with_radii(
                renderer,
                base,
                client.texture.clone(),
                content,
                radii,
                (1.0, 1.0, 1.0, 1.0),
            )
            .expect("rounded resources were checked above");
        elements.push(crate::render::scene::SceneElement::RoundedTexture(element));
    } else {
        elements.push(crate::render::scene::SceneElement::Closing(base));
    }
    let border_color = crate::render::window_border_color(decorations, focused);
    if border_width > 0 {
        if rounded
            && let Some(border) = if server_titlebar {
                window_decoration_renderer.body_border_element(
                    renderer,
                    Id::new(),
                    content,
                    border_width,
                    content_radius,
                    border_color,
                    1.0,
                )
            } else {
                window_decoration_renderer.border_element(
                    renderer,
                    Id::new(),
                    content,
                    border_width,
                    content_radius,
                    border_color,
                    1.0,
                )
            }
        {
            elements.push(crate::render::scene::SceneElement::WindowBorder(border));
        } else {
            let strips: Vec<_> = if server_titlebar {
                crate::render::body_border_strips(content, border_width, border_color)
                    .into_iter()
                    .collect()
            } else {
                crate::render::border_strips(content, border_width, border_color)
                    .into_iter()
                    .collect()
            };
            elements.extend(
                strips
                    .into_iter()
                    .map(crate::render::scene::SceneElement::Border),
            );
        }
    }

    let context = renderer.context_id();
    let buffer_size = output_size.to_logical(1).to_buffer(1, Transform::Normal);
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
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, output_size, Transform::Normal)?;
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
