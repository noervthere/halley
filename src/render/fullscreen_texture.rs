use std::collections::HashMap;
use std::error::Error;
use std::hash::{DefaultHasher, Hash, Hasher};

use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, ffi,
};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{ContextId, Renderer};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};
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
uniform vec2 halley_corner_radii;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Signed distance to a rounded rectangle, negative inside. See the matching
// note in `window_decoration.rs`: the interior term is what stops a radius
// animating toward zero from fading the whole surface to half opacity.
float rounded_alpha(vec2 coords, vec2 size, vec2 radii) {
    float radius = coords.y < size.y * 0.5 ? radii.x : radii.y;
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
    float mask = rounded_alpha(v_coords * size, size, halley_corner_radii);
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
    capture_generation: CommitCounter,
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
    radii: super::window_decoration::CornerRadii,
    commit: CommitCounter,
}

/// One window's crossfade between its captured and live textures.
#[derive(Clone, Copy)]
pub struct BlendRequest<'a> {
    pub window: &'a Window,
    pub destination: Rectangle<i32, Physical>,
    pub progress: f64,
    /// X11 applies restored fullscreen-exit geometry before the matching
    /// `wl_surface` buffer necessarily arrives. Capturing the still-fullscreen
    /// buffer at the smaller target crops it, making the reverse animation look
    /// like a discontinuous shrink. Set this to keep presenting the intact
    /// captured texture until XWayland commits the restored client size;
    /// geometry continues to animate independently through `destination`.
    pub hold_previous_until_restored_buffer_matches: bool,
    pub alpha: f32,
    pub radii: super::window_decoration::CornerRadii,
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
                capture_generation: CommitCounter::default(),
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
        request: BlendRequest<'_>,
    ) -> Result<Option<FullscreenBlendElement>, Box<dyn Error>> {
        let BlendRequest {
            window,
            destination,
            progress,
            hold_previous_until_restored_buffer_matches,
            alpha,
            radii,
        } = request;
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

        let buffer_matches = transition_buffer_ready(
            hold_previous_until_restored_buffer_matches,
            with_renderer_surface_state(surface.as_ref(), |state| state.surface_size()).flatten(),
            window.geometry().size,
        );
        let (current, texture_progress) = if buffer_matches {
            let reusable = entry.current.take();
            let current = match super::window_texture::capture(renderer, window, reusable) {
                Ok(current) => current,
                Err(err) => {
                    self.windows.remove(surface.as_ref());
                    return Err(err);
                }
            };
            entry.capture_generation.increment();
            (current, progress)
        } else {
            (
                WindowTexture {
                    texture: entry.previous.texture.clone(),
                    context: entry.previous.context.clone(),
                },
                0.0,
            )
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
                        UniformName::new("halley_corner_radii", UniformType::_2f),
                    ],
                )?;
                self.program = Some((context.clone(), program.clone()));
                program
            }
        };
        let previous_element = entry.previous.render_element(id, destination, alpha);
        let commit = blend_commit(
            previous_element.current_commit(),
            entry.capture_generation,
            texture_progress as f32,
            destination,
            radii,
        );

        Ok(Some(FullscreenBlendElement {
            previous: previous_element,
            previous_texture: previous,
            next: current.texture,
            program,
            progress: texture_progress.clamp(0.0, 1.0) as f32,
            size: (destination.size.w as f32, destination.size.h as f32),
            radii: super::window_decoration::CornerRadii {
                top: radii
                    .top
                    .max(0.0)
                    .min(destination.size.w.min(destination.size.h).max(0) as f32 * 0.5),
                bottom: radii
                    .bottom
                    .max(0.0)
                    .min(destination.size.w.min(destination.size.h).max(0) as f32 * 0.5),
            },
            commit,
        }))
    }
}

fn transition_buffer_ready(
    hold_previous: bool,
    buffer_size: Option<Size<i32, Logical>>,
    configured_size: Size<i32, Logical>,
) -> bool {
    !hold_previous || buffer_size == Some(configured_size)
}

fn blend_commit(
    base: CommitCounter,
    generation: CommitCounter,
    progress: f32,
    destination: Rectangle<i32, Physical>,
    radii: super::window_decoration::CornerRadii,
) -> CommitCounter {
    let mut hasher = DefaultHasher::new();
    base.distance(Some(CommitCounter::default()))
        .unwrap_or(usize::MAX)
        .hash(&mut hasher);
    generation
        .distance(Some(CommitCounter::default()))
        .unwrap_or(usize::MAX)
        .hash(&mut hasher);
    progress.to_bits().hash(&mut hasher);
    destination.size.w.hash(&mut hasher);
    destination.size.h.hash(&mut hasher);
    radii.top.to_bits().hash(&mut hasher);
    radii.bottom.to_bits().hash(&mut hasher);
    CommitCounter::from(hasher.finish() as usize)
}

impl Element for FullscreenBlendElement {
    fn id(&self) -> &Id {
        self.previous.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
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
                Uniform::new("halley_corner_radii", (self.radii.top, self.radii.bottom)),
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

#[cfg(test)]
mod tests {
    use super::{blend_commit, transition_buffer_ready};
    use smithay::backend::renderer::utils::CommitCounter;
    use smithay::utils::{Physical, Rectangle};

    #[test]
    fn x11_fullscreen_exit_holds_previous_texture_until_restored_buffer_arrives() {
        let restored = (1200, 800).into();

        assert!(!transition_buffer_ready(
            true,
            Some((1920, 1080).into()),
            restored
        ));
        assert!(!transition_buffer_ready(true, None, restored));
        assert!(transition_buffer_ready(
            true,
            Some((1200, 800).into()),
            restored
        ));
        assert!(transition_buffer_ready(
            false,
            Some((1920, 1080).into()),
            restored
        ));
    }

    #[test]
    fn blend_commits_follow_capture_generation_and_animation_progress() {
        let destination = Rectangle::<i32, Physical>::new((0, 0).into(), (1920, 1080).into());
        let radii = crate::render::window_decoration::CornerRadii::all(8.0);
        let base = CommitCounter::from(7);
        let first = blend_commit(base, CommitCounter::from(1), 0.25, destination, radii);

        assert_eq!(
            first,
            blend_commit(base, CommitCounter::from(1), 0.25, destination, radii)
        );
        assert_ne!(
            first,
            blend_commit(base, CommitCounter::from(2), 0.25, destination, radii)
        );
        assert_ne!(
            first,
            blend_commit(base, CommitCounter::from(1), 0.5, destination, radii)
        );
    }
}
