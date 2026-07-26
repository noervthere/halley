use std::collections::HashMap;
use std::error::Error;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, ffi,
};
use smithay::backend::renderer::utils::{
    CommitCounter, DamageSet, OpaqueRegions, draw_render_elements,
};
use smithay::backend::renderer::{Bind, Color32F, ContextId, Frame, Offscreen, Renderer, Texture};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};
use smithay::wayland::seat::WaylandFocus;

const FULLSCREEN_BLEND_SHADER: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
varying vec2 v_coords;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform sampler2D halley_next_tex;
uniform float halley_progress;
uniform float alpha;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec4 previous = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    previous.a = 1.0;
#endif
    vec4 next = texture2D(halley_next_tex, v_coords);
    vec4 color = mix(previous, next, halley_progress) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

#[derive(Debug)]
struct WindowTexture {
    texture: GlesTexture,
    context: ContextId<GlesTexture>,
}

#[derive(Debug)]
struct TransitionTextures {
    id: Id,
    previous: WindowTexture,
    current: Option<GlesTexture>,
}

#[derive(Default)]
pub struct FullscreenTextureTransitions {
    windows: HashMap<WlSurface, TransitionTextures>,
    program: Option<(ContextId<GlesTexture>, GlesTexProgram)>,
}

#[derive(Debug)]
pub struct FullscreenBlendElement {
    previous: TextureRenderElement<GlesTexture>,
    previous_texture: GlesTexture,
    next: GlesTexture,
    program: GlesTexProgram,
    progress: f32,
}

impl FullscreenTextureTransitions {
    pub fn capture_previous(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
    ) -> Result<(), Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("fullscreen snapshot window has no Wayland surface")?
            .into_owned();
        self.windows.remove(&surface);
        let previous = render_window(renderer, window, None)?;
        self.windows.insert(
            surface,
            TransitionTextures {
                id: Id::new(),
                previous,
                current: None,
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
    }

    pub fn contains(&self, surface: &WlSurface) -> bool {
        self.windows.contains_key(surface)
    }

    pub fn clear(&mut self) {
        self.windows.clear();
    }

    pub fn blend_element(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        destination: Rectangle<i32, Physical>,
        progress: f64,
    ) -> Result<Option<FullscreenBlendElement>, Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("fullscreen blend window has no Wayland surface")?;
        let context = renderer.context_id();
        let Some(entry) = self.windows.get_mut(surface.as_ref()) else {
            return Ok(None);
        };
        if entry.previous.context != context {
            self.windows.remove(surface.as_ref());
            return Ok(None);
        }

        let reusable = entry.current.take();
        let current = match render_window(renderer, window, reusable) {
            Ok(current) => current,
            Err(err) => {
                self.windows.remove(surface.as_ref());
                return Err(err);
            }
        };
        let previous = entry.previous.texture.clone();
        let id = entry.id.clone();
        entry.current = Some(current.texture.clone());

        let program = match self.program.as_ref() {
            Some((program_context, program)) if program_context == &context => program.clone(),
            _ => {
                let program = renderer.compile_custom_texture_shader(
                    FULLSCREEN_BLEND_SHADER,
                    &[
                        UniformName::new("halley_next_tex", UniformType::_1i),
                        UniformName::new("halley_progress", UniformType::_1f),
                    ],
                )?;
                self.program = Some((context.clone(), program.clone()));
                program
            }
        };
        let (source, destination_size) = texture_mapping(previous.size(), destination.size);
        let previous_element = TextureRenderElement::from_static_texture(
            id,
            context,
            destination.loc.to_f64(),
            previous.clone(),
            1,
            Transform::Normal,
            Some(1.0),
            Some(source),
            Some(destination_size),
            None,
            Kind::Unspecified,
        );

        Ok(Some(FullscreenBlendElement {
            previous: previous_element,
            previous_texture: previous,
            next: current.texture,
            program,
            progress: progress.clamp(0.0, 1.0) as f32,
        }))
    }
}

fn texture_mapping(
    texture_size: Size<i32, Buffer>,
    destination_size: Size<i32, Physical>,
) -> (Rectangle<f64, Logical>, Size<i32, Logical>) {
    (
        Rectangle::from_size(texture_size.to_logical(1, Transform::Normal).to_f64()),
        destination_size.to_logical(1),
    )
}

impl Element for FullscreenBlendElement {
    fn id(&self) -> &Id {
        self.previous.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.previous.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.previous.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.previous.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.previous.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.previous.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        1.0
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for FullscreenBlendElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, self.next.tex_id());
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;

        let result = frame.render_texture_from_to(
            &self.previous_texture,
            src,
            dst,
            damage,
            opaque_regions,
            self.transform(),
            self.alpha(),
            Some(&self.program),
            &[
                Uniform::new("halley_next_tex", 1_i32),
                Uniform::new("halley_progress", self.progress),
            ],
        );

        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

fn render_window(
    renderer: &mut GlesRenderer,
    window: &Window,
    reusable: Option<GlesTexture>,
) -> Result<WindowTexture, Box<dyn Error>> {
    let surface = window
        .wl_surface()
        .ok_or("fullscreen snapshot window has no Wayland surface")?;
    let geometry = window.geometry();
    if geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Err("fullscreen snapshot window has empty geometry".into());
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
        return Err("fullscreen snapshot surface tree is empty".into());
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

    #[test]
    fn growing_destination_does_not_expand_texture_source() {
        let (source, destination) = texture_mapping((800, 600).into(), (1920, 1080).into());

        assert_eq!(
            source,
            Rectangle::<f64, Logical>::from_size((800.0, 600.0).into())
        );
        assert_eq!(destination, Size::<i32, Logical>::from((1920, 1080)));
        assert_eq!(source.size.w / 800.0, 1.0);
        assert_eq!(source.size.h / 600.0, 1.0);
    }
}
