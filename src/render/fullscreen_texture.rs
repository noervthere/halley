use std::collections::HashMap;
use std::error::Error;

use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, ffi,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ContextId, Renderer};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};
use smithay::wayland::seat::WaylandFocus;

use super::window_texture::WindowTexture;

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
uniform vec2 halley_rect_size;
uniform float halley_corner_radius;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Signed distance to a rounded rectangle, negative inside. See the matching
// note in `window_decoration.rs`: the interior term is what stops a radius
// animating toward zero from fading the whole surface to half opacity.
float rounded_alpha(vec2 coords, vec2 size, float radius) {
    radius = clamp(radius, 0.0, min(size.x, size.y) * 0.5);
    vec2 half_size = size * 0.5;
    vec2 q = abs(coords - half_size) - (half_size - vec2(radius));
    float distance_to_edge =
        length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    return 1.0 - smoothstep(-0.75, 0.75, distance_to_edge);
}

void main() {
    vec4 previous = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    previous.a = 1.0;
#endif
    vec4 next = texture2D(halley_next_tex, v_coords);
    vec2 size = max(halley_rect_size, vec2(1.0));
    float mask = rounded_alpha(v_coords * size, size, halley_corner_radius);
    vec4 color = mix(previous, next, halley_progress) * (alpha * mask);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

#[derive(Debug)]
struct TransitionTextures {
    id: Id,
    previous: WindowTexture,
    current: Option<GlesTexture>,
    owner: TextureTransitionOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureTransitionOwner {
    Fullscreen,
    Maximize,
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
    size: (f32, f32),
    radius: f32,
}

impl FullscreenTextureTransitions {
    pub fn capture_previous(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        owner: TextureTransitionOwner,
    ) -> Result<(), Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("fullscreen snapshot window has no surface")?
            .into_owned();
        self.windows.remove(&surface);
        let previous = super::window_texture::capture(renderer, window, None)?;
        self.windows.insert(
            surface,
            TransitionTextures {
                id: Id::new(),
                previous,
                current: None,
                owner,
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, surface: &WlSurface) {
        self.windows.remove(surface);
    }

    pub fn remove_owner(&mut self, owner: TextureTransitionOwner) {
        self.windows.retain(|_, entry| entry.owner != owner);
    }

    pub fn blend_element(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        destination: Rectangle<i32, Physical>,
        progress: f64,
        alpha: f32,
        radius: f32,
    ) -> Result<Option<FullscreenBlendElement>, Box<dyn Error>> {
        let surface = window
            .wl_surface()
            .ok_or("fullscreen blend window has no surface")?;
        let context = renderer.context_id();
        let Some(entry) = self.windows.get_mut(surface.as_ref()) else {
            return Ok(None);
        };
        if entry.previous.context != context {
            self.windows.remove(surface.as_ref());
            return Ok(None);
        }

        let reusable = entry.current.take();
        let current = match super::window_texture::capture(renderer, window, reusable) {
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
                        UniformName::new("halley_rect_size", UniformType::_2f),
                        UniformName::new("halley_corner_radius", UniformType::_1f),
                    ],
                )?;
                self.program = Some((context.clone(), program.clone()));
                program
            }
        };
        let previous_element = entry.previous.render_element(id, destination, alpha);

        Ok(Some(FullscreenBlendElement {
            previous: previous_element,
            previous_texture: previous,
            next: current.texture,
            program,
            progress: progress.clamp(0.0, 1.0) as f32,
            size: (destination.size.w as f32, destination.size.h as f32),
            radius: radius
                .max(0.0)
                .min(destination.size.w.min(destination.size.h).max(0) as f32 * 0.5),
        }))
    }
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
        self.previous.alpha()
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
        let (previous_active_texture, previous_texture_1) = frame.with_context(|gl| unsafe {
            let mut active_texture = 0_i32;
            gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active_texture);
            gl.ActiveTexture(ffi::TEXTURE1);
            let mut texture_1 = 0_i32;
            gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut texture_1);
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
            gl.ActiveTexture(active_texture as u32);
            (active_texture, texture_1)
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
                Uniform::new("halley_rect_size", self.size),
                Uniform::new("halley_corner_radius", self.radius),
            ],
        );

        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, previous_texture_1 as u32);
            gl.ActiveTexture(previous_active_texture as u32);
        })?;
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
