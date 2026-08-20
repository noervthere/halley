use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, ffi, link_program,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{ContextId, Offscreen, Renderer, Texture};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};

const DOWN_SHADER: &str = include_str!("shaders/blur_down.frag");
const UP_SHADER: &str = include_str!("shaders/blur_up.frag");
const COMPOSITE_SHADER: &str = include_str!("shaders/blur_composite.frag");
const BLUR_VERTEX_SHADER: &str = r#"#version 100
attribute vec2 vert;
varying vec2 v_coords;

void main() {
    v_coords = vert;
    gl_Position = vec4(vert * 2.0 - 1.0, 1.0, 1.0);
}
"#;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlurPatch {
    pub rect: Rectangle<i32, Physical>,
    pub radius: f32,
    pub alpha: f32,
    pub clip: Option<(
        Rectangle<i32, Physical>,
        crate::render::window_decoration::CornerRadii,
    )>,
}

struct Programs {
    context: ContextId<GlesTexture>,
    down: RawPassProgram,
    up: RawPassProgram,
    composite: GlesTexProgram,
}

#[derive(Clone, Copy)]
struct RawPassProgram {
    program: ffi::types::GLuint,
    texture: ffi::types::GLint,
    alpha: ffi::types::GLint,
    halfpixel: ffi::types::GLint,
    offset: ffi::types::GLint,
    vertex: ffi::types::GLint,
}

struct BlurTextures {
    size: Size<i32, Physical>,
    levels: u32,
    accum: GlesTexture,
    chain: Vec<GlesTexture>,
    result: GlesTexture,
    dirty: bool,
    failed: bool,
}

struct OutputResources {
    textures: Rc<RefCell<BlurTextures>>,
    ids: HashMap<BlurIdentity, Id>,
    scene_identities: HashSet<BlurIdentity>,
}

/// Stable identity of one logical framebuffer-effect stack position. A
/// Wayland render-element `Id` is used where possible because protocol object
/// numbers are only unique within one client and formatted IDs can alias.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BlurIdentity {
    Window { surface: Id, instance: String },
    Layer(Id),
    Overlay(&'static str),
}

#[derive(Default)]
pub struct BackdropBlurRenderer {
    programs: Option<Programs>,
    outputs: HashMap<String, OutputResources>,
}

pub struct BackdropBlurElement {
    id: Id,
    commit: CommitCounter,
    size: Size<i32, Physical>,
    patches: Vec<BlurPatch>,
    textures: Rc<RefCell<BlurTextures>>,
    down: RawPassProgram,
    up: RawPassProgram,
    composite: GlesTexProgram,
    offset: f32,
    saturation: f32,
    noise: f32,
}

impl BackdropBlurRenderer {
    pub fn begin_scene(&mut self, output: &str, enabled: bool) {
        if !enabled {
            self.outputs.remove(output);
            return;
        }
        if let Some(resources) = self.outputs.get_mut(output) {
            resources.scene_identities.clear();
        }
    }

    pub fn remove_output(&mut self, output: &str) {
        self.outputs.remove(output);
    }

    pub fn blur_element(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &str,
        identity: BlurIdentity,
        size: Size<i32, Logical>,
        patches: Vec<BlurPatch>,
        config: halley_config::Blur,
    ) -> Result<Option<BackdropBlurElement>, Box<dyn Error>> {
        if !config.enabled || patches.is_empty() {
            return Ok(None);
        }
        if let Err(error) = self.ensure_programs(renderer) {
            eventline::error!("backdrop-blur: disabling effect after shader setup failed: {error}");
            return Ok(None);
        }
        let physical_size = size.to_physical(1);
        let context = renderer.context_id();
        let levels = config.passes.clamp(1, 5);
        let needs_textures = self.outputs.get(output).is_none_or(|resources| {
            let textures = resources.textures.borrow();
            textures.size != physical_size || textures.levels != levels
        });
        if needs_textures {
            let textures = match create_textures(renderer, physical_size, levels) {
                Ok(textures) => Rc::new(RefCell::new(textures)),
                Err(error) => {
                    eventline::error!(
                        "backdrop-blur: skipping effect after scratch allocation failed: {error}"
                    );
                    return Ok(None);
                }
            };
            match self.outputs.get_mut(output) {
                Some(resources) => resources.textures = textures,
                None => {
                    self.outputs.insert(
                        output.to_string(),
                        OutputResources {
                            textures,
                            ids: HashMap::new(),
                            scene_identities: HashSet::new(),
                        },
                    );
                }
            }
        }
        let resources = self.outputs.get_mut(output).expect("inserted above");
        let programs = self.programs.as_ref().expect("ensured above");
        debug_assert_eq!(programs.context, context);
        if !resources.scene_identities.insert(identity.clone()) {
            return Err(
                format!("duplicate backdrop blur identity in one scene: {identity:?}").into(),
            );
        }
        let id = resources
            .ids
            .entry(identity)
            .or_insert_with(Id::new)
            .clone();
        let commit = blur_commit(&patches, config);
        Ok(Some(BackdropBlurElement {
            // Each stack position keeps a stable identity. Smithay can then
            // retain its per-effect capture cache without ever aliasing two
            // z-ordered surfaces that share the scratch textures.
            id,
            commit,
            size: physical_size,
            patches,
            textures: Rc::clone(&resources.textures),
            down: programs.down.clone(),
            up: programs.up.clone(),
            composite: programs.composite.clone(),
            offset: blur_offset(config.radius),
            saturation: config.saturation,
            noise: config.noise,
        }))
    }

    fn ensure_programs(&mut self, renderer: &mut GlesRenderer) -> Result<(), Box<dyn Error>> {
        let context = renderer.context_id();
        if self
            .programs
            .as_ref()
            .is_some_and(|programs| programs.context == context)
        {
            return Ok(());
        }
        self.outputs.clear();
        let composite_uniforms = [
            UniformName::new("rect_size", UniformType::_2f),
            UniformName::new("patch_origin_uv", UniformType::_2f),
            UniformName::new("patch_size_uv", UniformType::_2f),
            UniformName::new("corner_radius", UniformType::_1f),
            UniformName::new("clip_origin", UniformType::_2f),
            UniformName::new("clip_size", UniformType::_2f),
            UniformName::new("clip_radii", UniformType::_2f),
            UniformName::new("use_clip", UniformType::_1f),
            UniformName::new("saturation", UniformType::_1f),
            UniformName::new("noise", UniformType::_1f),
        ];
        let down_source = raw_fragment_shader(DOWN_SHADER);
        let up_source = raw_fragment_shader(UP_SHADER);
        let (down, up) = renderer.with_context(|gl| unsafe {
            Ok::<_, GlesError>((
                compile_raw_pass_program(gl, &down_source)?,
                compile_raw_pass_program(gl, &up_source)?,
            ))
        })??;
        self.programs = Some(Programs {
            context,
            down,
            up,
            composite: renderer
                .compile_custom_texture_shader(COMPOSITE_SHADER, &composite_uniforms)?,
        });
        Ok(())
    }
}

fn raw_fragment_shader(source: &str) -> String {
    format!("#version 100\n{}", source.replacen("//_DEFINES\n", "", 1))
}

unsafe fn compile_raw_pass_program(
    gl: &ffi::Gles2,
    fragment: &str,
) -> Result<RawPassProgram, GlesError> {
    let program = unsafe { link_program(gl, BLUR_VERTEX_SHADER, fragment)? };
    Ok(RawPassProgram {
        program,
        texture: unsafe { gl.GetUniformLocation(program, c"tex".as_ptr()) },
        alpha: unsafe { gl.GetUniformLocation(program, c"alpha".as_ptr()) },
        halfpixel: unsafe { gl.GetUniformLocation(program, c"halfpixel".as_ptr()) },
        offset: unsafe { gl.GetUniformLocation(program, c"offset".as_ptr()) },
        vertex: unsafe { gl.GetAttribLocation(program, c"vert".as_ptr()) },
    })
}

fn level_size(size: Size<i32, Physical>, level: u32) -> Size<i32, Physical> {
    let shift = level + 1;
    ((size.w >> shift).max(1), (size.h >> shift).max(1)).into()
}

fn create_texture(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
) -> Result<GlesTexture, GlesError> {
    <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
        renderer,
        Fourcc::Argb8888,
        (size.w.max(1), size.h.max(1)).into(),
    )
}

fn create_textures(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    levels: u32,
) -> Result<BlurTextures, GlesError> {
    let levels = levels.clamp(1, 5);
    let mut chain = Vec::with_capacity(levels as usize);
    for level in 0..levels {
        chain.push(create_texture(renderer, level_size(size, level))?);
    }
    Ok(BlurTextures {
        size,
        levels,
        accum: create_texture(renderer, size)?,
        chain,
        result: create_texture(renderer, size)?,
        dirty: true,
        failed: false,
    })
}

fn run_blur(
    frame: &mut GlesFrame<'_, '_>,
    textures: &mut BlurTextures,
    down: RawPassProgram,
    up: RawPassProgram,
    offset: f32,
) -> Result<(), GlesError> {
    let size = textures.size;
    frame.with_context(|gl| unsafe {
        let mut draw_fbo = 0_i32;
        let mut viewport = [0_i32; 4];
        let mut active_texture = 0_i32;
        let mut texture_binding = 0_i32;
        let mut program = 0_i32;
        let mut array_buffer = 0_i32;
        gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut draw_fbo);
        gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());
        gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active_texture);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut texture_binding);
        gl.GetIntegerv(ffi::CURRENT_PROGRAM, &mut program);
        gl.GetIntegerv(ffi::ARRAY_BUFFER_BINDING, &mut array_buffer);
        let blend_enabled = gl.IsEnabled(ffi::BLEND) == ffi::TRUE;
        let scissor_enabled = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;

        let mut fbo = 0;
        gl.GenFramebuffers(1, &mut fbo);
        let result = (|| {
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);

            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            let render_pass = |source: &GlesTexture,
                               source_size: Size<i32, Physical>,
                               target: &GlesTexture,
                               target_size: Size<i32, Physical>,
                               pass: RawPassProgram|
             -> Result<(), GlesError> {
                gl.UseProgram(pass.program);
                gl.Uniform1i(pass.texture, 0);
                gl.Uniform1f(pass.alpha, 1.0);
                gl.Uniform2f(
                    pass.halfpixel,
                    0.5 / source_size.w.max(1) as f32,
                    0.5 / source_size.h.max(1) as f32,
                );
                gl.Uniform1f(pass.offset, offset);
                gl.Viewport(0, 0, target_size.w, target_size.h);
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    target.tex_id(),
                    0,
                );
                if gl.CheckFramebufferStatus(ffi::DRAW_FRAMEBUFFER) != ffi::FRAMEBUFFER_COMPLETE {
                    return Err(GlesError::FramebufferBindingError);
                }
                gl.BindTexture(ffi::TEXTURE_2D, source.tex_id());
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
                gl.EnableVertexAttribArray(pass.vertex as u32);
                gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
                gl.VertexAttribPointer(
                    pass.vertex as u32,
                    2,
                    ffi::FLOAT,
                    ffi::FALSE,
                    0,
                    vertices.as_ptr().cast(),
                );
                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
                gl.DisableVertexAttribArray(pass.vertex as u32);
                if gl.GetError() == ffi::NO_ERROR {
                    Ok(())
                } else {
                    Err(GlesError::BlitError)
                }
            };

            render_pass(
                &textures.accum,
                size,
                &textures.chain[0],
                level_size(size, 0),
                down,
            )?;
            for index in 1..textures.chain.len() {
                render_pass(
                    &textures.chain[index - 1],
                    level_size(size, index as u32 - 1),
                    &textures.chain[index],
                    level_size(size, index as u32),
                    down,
                )?;
            }
            for index in (1..textures.chain.len()).rev() {
                render_pass(
                    &textures.chain[index],
                    level_size(size, index as u32),
                    &textures.chain[index - 1],
                    level_size(size, index as u32 - 1),
                    up,
                )?;
            }
            render_pass(
                &textures.chain[0],
                level_size(size, 0),
                &textures.result,
                size,
                up,
            )?;
            Ok(())
        })();
        gl.DeleteFramebuffers(1, &fbo);

        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, draw_fbo as u32);
        gl.Viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        gl.BindBuffer(ffi::ARRAY_BUFFER, array_buffer as u32);
        gl.BindTexture(ffi::TEXTURE_2D, texture_binding as u32);
        gl.ActiveTexture(active_texture as u32);
        gl.UseProgram(program as u32);
        if blend_enabled {
            gl.Enable(ffi::BLEND);
        } else {
            gl.Disable(ffi::BLEND);
        }
        if scissor_enabled {
            gl.Enable(ffi::SCISSOR_TEST);
        } else {
            gl.Disable(ffi::SCISSOR_TEST);
        }
        result
    })?
}

impl Element for BackdropBlurElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::new(
            (0.0, 0.0).into(),
            (f64::from(self.size.w), f64::from(self.size.h)).into(),
        )
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_size(self.size)
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for BackdropBlurElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let mut textures = self.textures.borrow_mut();
        if textures.failed {
            return Ok(());
        }
        let size = textures.size;
        frame.with_context(|gl| unsafe {
            let mut prior_error = gl.GetError();
            while prior_error != ffi::NO_ERROR {
                eventline::error!(
                    "gles: error present before backdrop capture code=0x{prior_error:04x}"
                );
                prior_error = gl.GetError();
            }
            let mut current_draw_fbo = 0_i32;
            let mut current_read_fbo = 0_i32;
            gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_draw_fbo);
            gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut current_read_fbo);
            let scissor_was_enabled = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;
            gl.Disable(ffi::SCISSOR_TEST);
            let mut fbo = 0;
            gl.GenFramebuffers(1, &mut fbo);
            // Smithay leaves the output framebuffer bound for drawing. Bind it
            // explicitly for reading as well: blur passes use offscreen FBOs,
            // so relying on a stale READ binding can capture the wrong texture.
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, current_draw_fbo as u32);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                textures.accum.tex_id(),
                0,
            );
            gl.BlitFramebuffer(
                0,
                0,
                size.w,
                size.h,
                0,
                0,
                size.w,
                size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::NEAREST,
            );
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, current_read_fbo as u32);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_draw_fbo as u32);
            if scissor_was_enabled {
                gl.Enable(ffi::SCISSOR_TEST);
            } else {
                gl.Disable(ffi::SCISSOR_TEST);
            }
            gl.DeleteFramebuffers(1, &fbo);
            if gl.GetError() == ffi::NO_ERROR {
                Ok(())
            } else {
                Err(GlesError::BlitError)
            }
        })??;
        textures.dirty = true;
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let mut textures = self.textures.borrow_mut();
        if textures.failed {
            return Ok(());
        }
        if textures.dirty {
            if let Err(error) = run_blur(frame, &mut textures, self.down, self.up, self.offset) {
                eventline::error!("backdrop-blur: skipping effect after render failure: {error}");
                textures.failed = true;
                return Ok(());
            }
            textures.dirty = false;
        }
        for patch in &self.patches {
            composite_patch(
                frame,
                &textures.result,
                &self.composite,
                *patch,
                damage,
                self.saturation,
                self.noise,
            )?;
        }
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

fn composite_patch(
    frame: &mut GlesFrame<'_, '_>,
    texture: &GlesTexture,
    program: &GlesTexProgram,
    patch: BlurPatch,
    damage: &[Rectangle<i32, Physical>],
    saturation: f32,
    noise: f32,
) -> Result<(), GlesError> {
    let local_damage = damage
        .iter()
        .filter_map(|damage| {
            patch.rect.intersection(*damage).map(|visible| {
                Rectangle::new(
                    (
                        visible.loc.x - patch.rect.loc.x,
                        visible.loc.y - patch.rect.loc.y,
                    )
                        .into(),
                    visible.size,
                )
            })
        })
        .collect::<Vec<_>>();
    if local_damage.is_empty() || patch.alpha <= 0.0 {
        return Ok(());
    }
    let texture_size = texture.size();
    let (clip_origin, clip_size, clip_radii, use_clip) = patch
        .clip
        .map(|(clip, radii)| {
            (
                (clip.loc.x as f32, clip.loc.y as f32),
                (clip.size.w as f32, clip.size.h as f32),
                (radii.top.max(0.0), radii.bottom.max(0.0)),
                1.0,
            )
        })
        .unwrap_or(((0.0, 0.0), (1.0, 1.0), (0.0, 0.0), 0.0));
    frame.render_texture_from_to(
        texture,
        Rectangle::<f64, Buffer>::new(
            (f64::from(patch.rect.loc.x), f64::from(patch.rect.loc.y)).into(),
            (f64::from(patch.rect.size.w), f64::from(patch.rect.size.h)).into(),
        ),
        patch.rect,
        &local_damage,
        &[],
        Transform::Normal,
        patch.alpha.clamp(0.0, 1.0),
        Some(program),
        &[
            Uniform::new(
                "rect_size",
                (patch.rect.size.w as f32, patch.rect.size.h as f32),
            ),
            Uniform::new(
                "patch_origin_uv",
                (
                    patch.rect.loc.x as f32 / texture_size.w.max(1) as f32,
                    patch.rect.loc.y as f32 / texture_size.h.max(1) as f32,
                ),
            ),
            Uniform::new(
                "patch_size_uv",
                (
                    patch.rect.size.w as f32 / texture_size.w.max(1) as f32,
                    patch.rect.size.h as f32 / texture_size.h.max(1) as f32,
                ),
            ),
            Uniform::new("corner_radius", patch.radius.max(0.0)),
            Uniform::new("clip_origin", clip_origin),
            Uniform::new("clip_size", clip_size),
            Uniform::new("clip_radii", clip_radii),
            Uniform::new("use_clip", use_clip),
            Uniform::new("saturation", saturation.clamp(0.0, 4.0)),
            Uniform::new("noise", noise.clamp(0.0, 0.25)),
        ],
    )
}

fn blur_offset(radius: f32) -> f32 {
    (radius / 16.0).clamp(0.6, 3.0)
}

fn blur_commit(patches: &[BlurPatch], config: halley_config::Blur) -> CommitCounter {
    let mut hash = DefaultHasher::new();
    config.passes.hash(&mut hash);
    config.radius.to_bits().hash(&mut hash);
    config.saturation.to_bits().hash(&mut hash);
    config.noise.to_bits().hash(&mut hash);
    for patch in patches {
        patch.rect.loc.x.hash(&mut hash);
        patch.rect.loc.y.hash(&mut hash);
        patch.rect.size.w.hash(&mut hash);
        patch.rect.size.h.hash(&mut hash);
        patch.radius.to_bits().hash(&mut hash);
        patch.alpha.to_bits().hash(&mut hash);
        if let Some((clip, radii)) = patch.clip {
            true.hash(&mut hash);
            clip.loc.x.hash(&mut hash);
            clip.loc.y.hash(&mut hash);
            clip.size.w.hash(&mut hash);
            clip.size.h.hash(&mut hash);
            radii.top.to_bits().hash(&mut hash);
            radii.bottom.to_bits().hash(&mut hash);
        } else {
            false.hash(&mut hash);
        }
    }
    CommitCounter::from(hash.finish() as usize)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use smithay::backend::renderer::element::Id;
    use smithay::utils::{Physical, Size};

    use super::{BlurIdentity, DOWN_SHADER, level_size, raw_fragment_shader};

    #[test]
    fn typed_identities_do_not_alias_across_scene_roles() {
        let surface = Id::new();
        let identities = [
            BlurIdentity::Layer(surface.clone()),
            BlurIdentity::Window {
                surface,
                instance: "canonical".to_string(),
            },
            BlurIdentity::Overlay("canonical"),
        ];

        assert_eq!(identities.into_iter().collect::<HashSet<_>>().len(), 3);
    }

    #[test]
    fn a_stack_position_keeps_the_same_identity_across_frames() {
        assert_eq!(
            BlurIdentity::Overlay("shell-overlay"),
            BlurIdentity::Overlay("shell-overlay")
        );
    }

    #[test]
    fn blur_levels_stay_nonzero_and_halve_per_pass() {
        let size = Size::<i32, Physical>::from((9, 5));
        assert_eq!(level_size(size, 0), Size::from((4, 2)));
        assert_eq!(level_size(size, 1), Size::from((2, 1)));
        assert_eq!(level_size(size, 4), Size::from((1, 1)));
    }

    #[test]
    fn raw_blur_shader_has_a_gles_version_and_no_smithay_defines_marker() {
        let shader = raw_fragment_shader(DOWN_SHADER);
        assert!(shader.starts_with("#version 100\n"));
        assert!(!shader.contains("//_DEFINES"));
    }
}
