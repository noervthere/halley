use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, ffi,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{
    Bind, Color32F, ContextId, Frame, FrameContext, Offscreen, Renderer, Texture,
};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};

const DOWN_SHADER: &str = include_str!("shaders/blur_down.frag");
const UP_SHADER: &str = include_str!("shaders/blur_up.frag");
const COMPOSITE_SHADER: &str = include_str!("shaders/blur_composite.frag");
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlurPatch {
    pub rect: Rectangle<i32, Physical>,
    pub radius: f32,
    pub alpha: f32,
    pub clip: Option<(Rectangle<i32, Physical>, f32)>,
}

struct Programs {
    context: ContextId<GlesTexture>,
    down: GlesTexProgram,
    up: GlesTexProgram,
    composite: GlesTexProgram,
}

struct BlurTextures {
    size: Size<i32, Physical>,
    levels: u32,
    accum: GlesTexture,
    chain: Vec<GlesTexture>,
    result: GlesTexture,
    dirty: bool,
}

struct OutputResources {
    textures: Rc<RefCell<BlurTextures>>,
    ids: HashMap<String, Id>,
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
    down: GlesTexProgram,
    up: GlesTexProgram,
    composite: GlesTexProgram,
    offset: f32,
    saturation: f32,
    noise: f32,
}

impl BackdropBlurRenderer {
    pub fn blur_element(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &str,
        identity: impl Into<String>,
        size: Size<i32, Logical>,
        patches: Vec<BlurPatch>,
        config: halley_config::Blur,
    ) -> Result<Option<BackdropBlurElement>, Box<dyn Error>> {
        if !config.enabled || patches.is_empty() {
            return Ok(None);
        }
        self.ensure_programs(renderer)?;
        let physical_size = size.to_physical(1);
        let context = renderer.context_id();
        let levels = config.passes.clamp(1, 5);
        let needs_textures = self.outputs.get(output).is_none_or(|resources| {
            let textures = resources.textures.borrow();
            textures.size != physical_size || textures.levels != levels
        });
        if needs_textures {
            let textures = Rc::new(RefCell::new(create_textures(
                renderer,
                physical_size,
                levels,
            )?));
            match self.outputs.get_mut(output) {
                Some(resources) => resources.textures = textures,
                None => {
                    self.outputs.insert(
                        output.to_string(),
                        OutputResources {
                            textures,
                            ids: HashMap::new(),
                        },
                    );
                }
            }
        }
        let resources = self.outputs.get_mut(output).expect("inserted above");
        let programs = self.programs.as_ref().expect("ensured above");
        debug_assert_eq!(programs.context, context);
        let identity = identity.into();
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
        let pass_uniforms = [
            UniformName::new("halfpixel", UniformType::_2f),
            UniformName::new("offset", UniformType::_1f),
        ];
        let composite_uniforms = [
            UniformName::new("rect_size", UniformType::_2f),
            UniformName::new("patch_origin_uv", UniformType::_2f),
            UniformName::new("patch_size_uv", UniformType::_2f),
            UniformName::new("corner_radius", UniformType::_1f),
            UniformName::new("clip_origin", UniformType::_2f),
            UniformName::new("clip_size", UniformType::_2f),
            UniformName::new("clip_radius", UniformType::_1f),
            UniformName::new("use_clip", UniformType::_1f),
            UniformName::new("saturation", UniformType::_1f),
            UniformName::new("noise", UniformType::_1f),
        ];
        self.programs = Some(Programs {
            context,
            down: renderer.compile_custom_texture_shader(DOWN_SHADER, &pass_uniforms)?,
            up: renderer.compile_custom_texture_shader(UP_SHADER, &pass_uniforms)?,
            composite: renderer
                .compile_custom_texture_shader(COMPOSITE_SHADER, &composite_uniforms)?,
        });
        Ok(())
    }
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
    })
}

fn blur_pass(
    renderer: &mut GlesRenderer,
    target: &mut GlesTexture,
    target_size: Size<i32, Physical>,
    source: &GlesTexture,
    source_size: Size<i32, Physical>,
    program: &GlesTexProgram,
    offset: f32,
) -> Result<(), GlesError> {
    let mut bound = renderer.bind(target)?;
    let damage = Rectangle::<i32, Physical>::from_size(target_size);
    let mut frame = renderer.render(&mut bound, target_size, Transform::Normal)?;
    frame.clear(Color32F::TRANSPARENT, &[damage])?;
    frame.render_texture_from_to(
        source,
        Rectangle::<f64, Buffer>::new(
            (0.0, 0.0).into(),
            (f64::from(source_size.w), f64::from(source_size.h)).into(),
        ),
        Rectangle::from_size(target_size),
        &[damage],
        &[],
        Transform::Normal,
        1.0,
        Some(program),
        &[
            Uniform::new(
                "halfpixel",
                (
                    0.5 / source_size.w.max(1) as f32,
                    0.5 / source_size.h.max(1) as f32,
                ),
            ),
            Uniform::new("offset", offset),
        ],
    )?;
    let _ = frame.finish()?;
    Ok(())
}

fn run_blur(
    renderer: &mut GlesRenderer,
    textures: &mut BlurTextures,
    down: &GlesTexProgram,
    up: &GlesTexProgram,
    offset: f32,
) -> Result<(), GlesError> {
    let size = textures.size;
    blur_pass(
        renderer,
        &mut textures.chain[0],
        level_size(size, 0),
        &textures.accum,
        size,
        down,
        offset,
    )?;
    for index in 1..textures.chain.len() {
        let (lower, upper) = textures.chain.split_at_mut(index);
        blur_pass(
            renderer,
            &mut upper[0],
            level_size(size, index as u32),
            &lower[index - 1],
            level_size(size, index as u32 - 1),
            down,
            offset,
        )?;
    }
    for index in (1..textures.chain.len()).rev() {
        let (lower, upper) = textures.chain.split_at_mut(index);
        blur_pass(
            renderer,
            &mut lower[index - 1],
            level_size(size, index as u32 - 1),
            &upper[0],
            level_size(size, index as u32),
            up,
            offset,
        )?;
    }
    blur_pass(
        renderer,
        &mut textures.result,
        size,
        &textures.chain[0],
        level_size(size, 0),
        up,
        offset,
    )
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
        let size = textures.size;
        frame.with_context(|gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}
            let mut current_fbo = 0_i32;
            gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo);
            gl.Disable(ffi::SCISSOR_TEST);
            let mut fbo = 0;
            gl.GenFramebuffers(1, &mut fbo);
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
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
            gl.Enable(ffi::SCISSOR_TEST);
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
        if textures.dirty {
            let mut renderer = frame.renderer();
            run_blur(
                renderer.as_mut(),
                &mut textures,
                &self.down,
                &self.up,
                self.offset,
            )?;
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
    let (clip_origin, clip_size, clip_radius, use_clip) = patch
        .clip
        .map(|(clip, radius)| {
            (
                (clip.loc.x as f32, clip.loc.y as f32),
                (clip.size.w as f32, clip.size.h as f32),
                radius.max(0.0),
                1.0,
            )
        })
        .unwrap_or(((0.0, 0.0), (1.0, 1.0), 0.0, 0.0));
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
            Uniform::new("clip_radius", clip_radius),
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
        if let Some((clip, radius)) = patch.clip {
            true.hash(&mut hash);
            clip.loc.x.hash(&mut hash);
            clip.loc.y.hash(&mut hash);
            clip.size.w.hash(&mut hash);
            clip.size.h.hash(&mut hash);
            radius.to_bits().hash(&mut hash);
        } else {
            false.hash(&mut hash);
        }
    }
    CommitCounter::from(hash.finish() as usize)
}
