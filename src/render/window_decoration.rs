use std::error::Error;

use cgmath::{Matrix3, SquareMatrix, Vector2};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, WaylandSurfaceTexture,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ContextId, ImportMem, Renderer, Texture};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};

const JOIN_OVERLAP_PX: f32 = 0.75;

const SURFACE_SHADER: &str = r#"
//_DEFINES
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

precision highp float;
varying vec2 v_coords;

uniform float alpha;
uniform vec2 clip_size;
uniform vec2 draw_offset;
uniform vec2 corner_radii;
uniform vec3 uv_to_draw_col_0;
uniform vec3 uv_to_draw_col_1;
uniform vec3 uv_to_draw_col_2;
uniform vec4 content_color;

// Signed distance to a rounded rectangle, negative inside.
//
// The interior term `min(max(q.x, q.y), 0.0)` is load-bearing: without it an
// interior pixel reports a distance of exactly -radius rather than its real
// depth, so once the radius eases below the smoothstep half-width the band
// swallows the whole surface and fades it toward 50% opacity. That is only
// invisible while the radius stays large - a radius animating to zero, as the
// fullscreen transition does, dims the entire window just before it lands.
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
    mat3 uv_to_draw = mat3(uv_to_draw_col_0, uv_to_draw_col_1, uv_to_draw_col_2);
    vec2 coords = draw_offset + (uv_to_draw * vec3(v_coords, 1.0)).xy;
    vec2 size = max(clip_size, vec2(1.0));
    float mask = 0.0;
    if (coords.x >= 0.0 && coords.y >= 0.0 && coords.x <= size.x && coords.y <= size.y) {
        mask = rounded_alpha(coords, size, corner_radii);
    }

    vec4 sampled = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    sampled.a = 1.0;
#endif
    // Smithay uses premultiplied blending. Apply the mask exactly once to
    // both RGB and alpha; multiplying RGB by alpha here creates dark halos.
    gl_FragColor = sampled * content_color * (mask * alpha);
}
"#;

const BORDER_SHADER: &str = r#"
precision highp float;
//_DEFINES

varying vec2 v_coords;
uniform sampler2D tex;
uniform float alpha;
uniform vec4 border_color;
uniform vec2 rect_size;
uniform vec2 inner_rect_size;
uniform vec2 inner_rect_offset;
uniform vec2 corner_radii;
uniform vec2 inner_corner_radii;

float rounded_rect_sdf(vec2 p, vec2 size, vec2 radii) {
    float radius = p.y < 0.0 ? radii.x : radii.y;
    radius = min(max(radius, 0.0), min(size.x, size.y) * 0.5);
    vec2 half_size = size * 0.5;
    vec2 q = abs(p) - (half_size - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

float sdf_alpha(float distance_to_edge) {
    return 1.0 - smoothstep(-0.75, 0.75, distance_to_edge);
}

void main() {
    vec2 size = max(rect_size, vec2(1.0));
    vec2 p = v_coords * size - size * 0.5;
    float outer = sdf_alpha(rounded_rect_sdf(p, size, corner_radii));

    vec2 inner_size = max(inner_rect_size, vec2(1.0));
    vec2 inner_center = inner_rect_offset + inner_size * 0.5 - size * 0.5;
    float inner = sdf_alpha(
        rounded_rect_sdf(p - inner_center, inner_size, inner_corner_radii)
    );
    float border = max(outer - inner, 0.0);
    gl_FragColor = border_color * (border * alpha);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub content_radius: f32,
    pub border_width: f32,
    pub outer_radius: f32,
    pub inner_offset: f32,
    pub inner_radius: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CornerRadii {
    pub top: f32,
    pub bottom: f32,
}

impl CornerRadii {
    pub fn all(radius: f32) -> Self {
        Self {
            top: radius,
            bottom: radius,
        }
    }

    pub fn top(radius: f32) -> Self {
        Self {
            top: radius,
            bottom: 0.0,
        }
    }

    pub fn bottom(radius: f32) -> Self {
        Self {
            top: 0.0,
            bottom: radius,
        }
    }
}

pub fn metrics(content_radius: f32, border_width: f32) -> Metrics {
    let content_radius = content_radius.max(0.0);
    let border_width = border_width.max(0.0);
    Metrics {
        content_radius,
        border_width,
        outer_radius: if content_radius > 0.0 {
            content_radius + border_width
        } else {
            0.0
        },
        inner_offset: border_width + JOIN_OVERLAP_PX,
        inner_radius: (content_radius - JOIN_OVERLAP_PX).max(0.0),
    }
}

struct Resources {
    context: ContextId<GlesTexture>,
    white: GlesTexture,
    surface: GlesTexProgram,
    border: GlesTexProgram,
}

#[derive(Default)]
pub struct WindowDecorationRenderer {
    resources: Option<Resources>,
    failed_context: Option<ContextId<GlesTexture>>,
}

impl WindowDecorationRenderer {
    pub fn available(&mut self, renderer: &mut GlesRenderer) -> bool {
        if self.ensure_resources(renderer).is_ok() {
            true
        } else {
            let context = renderer.context_id();
            if self.failed_context.as_ref() != Some(&context) {
                eventline::warn!(
                    "rounded window rendering unavailable; using square fallback for this GL context"
                );
                self.failed_context = Some(context);
            }
            false
        }
    }

    pub fn surface_element(
        &mut self,
        renderer: &mut GlesRenderer,
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        destination: Rectangle<i32, Physical>,
        clip: Rectangle<i32, Physical>,
        radius: f32,
    ) -> Option<RoundedSurfaceElement> {
        self.surface_element_with_radii(
            renderer,
            inner,
            destination,
            clip,
            CornerRadii::all(radius),
        )
    }

    pub fn surface_element_with_radii(
        &mut self,
        renderer: &mut GlesRenderer,
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        destination: Rectangle<i32, Physical>,
        clip: Rectangle<i32, Physical>,
        radii: CornerRadii,
    ) -> Option<RoundedSurfaceElement> {
        self.available(renderer);
        let resources = self.resources.as_ref()?;
        Some(RoundedSurfaceElement {
            inner,
            destination,
            clip,
            radii: clamp_radii(radii, clip.size.w, clip.size.h),
            program: resources.surface.clone(),
            white: resources.white.clone(),
        })
    }

    pub fn border_element(
        &mut self,
        renderer: &mut GlesRenderer,
        content: Rectangle<i32, Physical>,
        width: i32,
        radius: f32,
        color: smithay::backend::renderer::Color32F,
        alpha: f32,
    ) -> Option<RoundedBorderElement> {
        if width <= 0 || alpha <= 0.0 || radius <= 0.0 {
            return None;
        }
        self.available(renderer);
        let resources = self.resources.as_ref()?;
        let width = width as f32;
        let metrics = metrics(radius, width);
        let width_i = width.round() as i32;
        let destination = Rectangle::new(
            (content.loc.x - width_i, content.loc.y - width_i).into(),
            (
                (content.size.w + width_i * 2).max(1),
                (content.size.h + width_i * 2).max(1),
            )
                .into(),
        );
        let source = Rectangle::<f64, Logical>::from_size(
            resources
                .white
                .size()
                .to_logical(1, Transform::Normal)
                .to_f64(),
        );
        let base = TextureRenderElement::from_static_texture(
            Id::new(),
            resources.context.clone(),
            destination.loc.to_f64(),
            resources.white.clone(),
            1,
            Transform::Normal,
            Some(alpha.clamp(0.0, 1.0)),
            Some(source),
            Some(destination.size.to_logical(1)),
            None,
            Kind::Unspecified,
        );
        Some(RoundedBorderElement {
            base,
            white: resources.white.clone(),
            program: resources.border.clone(),
            color: (color.r(), color.g(), color.b(), color.a()),
            size: (destination.size.w as f32, destination.size.h as f32),
            inner_size: (
                (content.size.w as f32 - JOIN_OVERLAP_PX * 2.0).max(1.0),
                (content.size.h as f32 - JOIN_OVERLAP_PX * 2.0).max(1.0),
            ),
            inner_offset: (metrics.inner_offset, metrics.inner_offset),
            outer_radii: CornerRadii::all(metrics.outer_radius),
            inner_radii: CornerRadii::all(metrics.inner_radius),
        })
    }

    pub fn body_border_element(
        &mut self,
        renderer: &mut GlesRenderer,
        content: Rectangle<i32, Physical>,
        width: i32,
        bottom_radius: f32,
        color: smithay::backend::renderer::Color32F,
        alpha: f32,
    ) -> Option<RoundedBorderElement> {
        if width <= 0 || alpha <= 0.0 {
            return None;
        }
        self.available(renderer);
        let resources = self.resources.as_ref()?;
        let width_i = width.max(0);
        let width_f = width_i as f32;
        let metrics = metrics(bottom_radius, width_f);
        let destination = Rectangle::new(
            (content.loc.x - width_i, content.loc.y).into(),
            (
                (content.size.w + width_i * 2).max(1),
                (content.size.h + width_i).max(1),
            )
                .into(),
        );
        let source = Rectangle::<f64, Logical>::from_size(
            resources
                .white
                .size()
                .to_logical(1, Transform::Normal)
                .to_f64(),
        );
        let base = TextureRenderElement::from_static_texture(
            Id::new(),
            resources.context.clone(),
            destination.loc.to_f64(),
            resources.white.clone(),
            1,
            Transform::Normal,
            Some(alpha.clamp(0.0, 1.0)),
            Some(source),
            Some(destination.size.to_logical(1)),
            None,
            Kind::Unspecified,
        );
        Some(RoundedBorderElement {
            base,
            white: resources.white.clone(),
            program: resources.border.clone(),
            color: (color.r(), color.g(), color.b(), color.a()),
            size: (destination.size.w as f32, destination.size.h as f32),
            inner_size: (
                (content.size.w as f32 - JOIN_OVERLAP_PX * 2.0).max(1.0),
                (content.size.h as f32 + JOIN_OVERLAP_PX).max(1.0),
            ),
            inner_offset: (metrics.inner_offset, -JOIN_OVERLAP_PX),
            outer_radii: CornerRadii::bottom(metrics.outer_radius),
            inner_radii: CornerRadii::bottom(metrics.inner_radius),
        })
    }

    pub fn texture_element(
        &mut self,
        renderer: &mut GlesRenderer,
        base: TextureRenderElement<GlesTexture>,
        texture: GlesTexture,
        clip: Rectangle<i32, Physical>,
        radius: f32,
    ) -> Option<RoundedTextureElement> {
        self.texture_element_with_radii(
            renderer,
            base,
            texture,
            clip,
            CornerRadii::all(radius),
            (1.0, 1.0, 1.0, 1.0),
        )
    }

    pub fn texture_element_with_radii(
        &mut self,
        renderer: &mut GlesRenderer,
        base: TextureRenderElement<GlesTexture>,
        texture: GlesTexture,
        clip: Rectangle<i32, Physical>,
        radii: CornerRadii,
        content_color: (f32, f32, f32, f32),
    ) -> Option<RoundedTextureElement> {
        self.available(renderer);
        let resources = self.resources.as_ref()?;
        Some(RoundedTextureElement {
            base,
            texture,
            clip,
            radii: clamp_radii(radii, clip.size.w, clip.size.h),
            program: resources.surface.clone(),
            content_color,
        })
    }

    pub fn tint_element(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        radius: f32,
        color: smithay::backend::renderer::Color32F,
        alpha: f32,
    ) -> Option<RoundedTextureElement> {
        self.tint_element_with_radii(
            renderer,
            destination,
            CornerRadii::all(radius),
            color,
            alpha,
        )
    }

    pub fn tint_element_with_radii(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        radii: CornerRadii,
        color: smithay::backend::renderer::Color32F,
        alpha: f32,
    ) -> Option<RoundedTextureElement> {
        if destination.size.w <= 0 || destination.size.h <= 0 || alpha <= 0.0 {
            return None;
        }
        self.available(renderer);
        let resources = self.resources.as_ref()?;
        let source = Rectangle::<f64, Logical>::from_size(
            resources
                .white
                .size()
                .to_logical(1, Transform::Normal)
                .to_f64(),
        );
        let base = TextureRenderElement::from_static_texture(
            Id::new(),
            resources.context.clone(),
            destination.loc.to_f64(),
            resources.white.clone(),
            1,
            Transform::Normal,
            Some(alpha.clamp(0.0, 1.0)),
            Some(source),
            Some(destination.size.to_logical(1)),
            None,
            Kind::Unspecified,
        );
        Some(RoundedTextureElement {
            base,
            texture: resources.white.clone(),
            clip: destination,
            radii: clamp_radii(radii, destination.size.w, destination.size.h),
            program: resources.surface.clone(),
            content_color: (color.r(), color.g(), color.b(), color.a()),
        })
    }

    fn ensure_resources(&mut self, renderer: &mut GlesRenderer) -> Result<(), Box<dyn Error>> {
        let context = renderer.context_id();
        if self
            .resources
            .as_ref()
            .is_some_and(|resources| resources.context == context)
        {
            return Ok(());
        }
        if self.failed_context.as_ref() == Some(&context) {
            return Err("rounded shaders previously failed for this GL context".into());
        }

        let result = (|| {
            let white = renderer.import_memory(
                &[255_u8; 4 * 4 * 4],
                Fourcc::Abgr8888,
                (4, 4).into(),
                false,
            )?;
            let surface = renderer.compile_custom_texture_shader(
                SURFACE_SHADER,
                &[
                    UniformName::new("clip_size", UniformType::_2f),
                    UniformName::new("draw_offset", UniformType::_2f),
                    UniformName::new("corner_radii", UniformType::_2f),
                    UniformName::new("uv_to_draw_col_0", UniformType::_3f),
                    UniformName::new("uv_to_draw_col_1", UniformType::_3f),
                    UniformName::new("uv_to_draw_col_2", UniformType::_3f),
                    UniformName::new("content_color", UniformType::_4f),
                ],
            )?;
            let border = renderer.compile_custom_texture_shader(
                BORDER_SHADER,
                &[
                    UniformName::new("border_color", UniformType::_4f),
                    UniformName::new("rect_size", UniformType::_2f),
                    UniformName::new("inner_rect_size", UniformType::_2f),
                    UniformName::new("inner_rect_offset", UniformType::_2f),
                    UniformName::new("corner_radii", UniformType::_2f),
                    UniformName::new("inner_corner_radii", UniformType::_2f),
                ],
            )?;
            Ok::<_, Box<dyn Error>>(Resources {
                context: context.clone(),
                white,
                surface,
                border,
            })
        })();

        match result {
            Ok(resources) => {
                self.resources = Some(resources);
                self.failed_context = None;
                Ok(())
            }
            Err(err) => {
                self.resources = None;
                self.failed_context = Some(context);
                Err(err)
            }
        }
    }
}

pub struct RoundedSurfaceElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    destination: Rectangle<i32, Physical>,
    clip: Rectangle<i32, Physical>,
    radii: CornerRadii,
    program: GlesTexProgram,
    white: GlesTexture,
}

impl Element for RoundedSurfaceElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.destination
    }

    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.destination.loc
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        // A hardware plane would bypass the rounded mask.
        Kind::Unspecified
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
}

impl RenderElement<GlesRenderer> for RoundedSurfaceElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        match self.inner.texture() {
            WaylandSurfaceTexture::Texture(texture) => draw_masked_texture(
                frame,
                texture,
                src,
                dst,
                damage,
                self.transform(),
                self.alpha(),
                &self.program,
                self.clip,
                self.radii,
                (1.0, 1.0, 1.0, 1.0),
            ),
            WaylandSurfaceTexture::SolidColor(color) => {
                let white_src = Rectangle::<f64, Buffer>::from_size(self.white.size().to_f64());
                draw_masked_texture(
                    frame,
                    &self.white,
                    white_src,
                    dst,
                    damage,
                    Transform::Normal,
                    self.alpha(),
                    &self.program,
                    self.clip,
                    self.radii,
                    (color.r(), color.g(), color.b(), color.a()),
                )
            }
        }
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[derive(Debug)]
pub struct RoundedTextureElement {
    base: TextureRenderElement<GlesTexture>,
    texture: GlesTexture,
    clip: Rectangle<i32, Physical>,
    radii: CornerRadii,
    program: GlesTexProgram,
    content_color: (f32, f32, f32, f32),
}

impl Element for RoundedTextureElement {
    fn id(&self) -> &Id {
        self.base.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.base.current_commit()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.base.geometry(scale)
    }
    fn transform(&self) -> Transform {
        self.base.transform()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.base.src()
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.base.damage_since(scale, commit)
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.base.alpha()
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for RoundedTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        draw_masked_texture(
            frame,
            &self.texture,
            src,
            dst,
            damage,
            self.transform(),
            self.alpha(),
            &self.program,
            self.clip,
            self.radii,
            self.content_color,
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[derive(Debug)]
pub struct RoundedBorderElement {
    base: TextureRenderElement<GlesTexture>,
    white: GlesTexture,
    program: GlesTexProgram,
    color: (f32, f32, f32, f32),
    size: (f32, f32),
    inner_size: (f32, f32),
    inner_offset: (f32, f32),
    outer_radii: CornerRadii,
    inner_radii: CornerRadii,
}

impl Element for RoundedBorderElement {
    fn id(&self) -> &Id {
        self.base.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.base.current_commit()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.base.geometry(scale)
    }
    fn transform(&self) -> Transform {
        self.base.transform()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.base.src()
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.base.damage_since(scale, commit)
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.base.alpha()
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for RoundedBorderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.white,
            src,
            dst,
            damage,
            &[],
            Transform::Normal,
            self.alpha(),
            Some(&self.program),
            &[
                Uniform::new("border_color", self.color),
                Uniform::new("rect_size", self.size),
                Uniform::new("inner_rect_size", self.inner_size),
                Uniform::new("inner_rect_offset", self.inner_offset),
                Uniform::new(
                    "corner_radii",
                    (self.outer_radii.top, self.outer_radii.bottom),
                ),
                Uniform::new(
                    "inner_corner_radii",
                    (self.inner_radii.top, self.inner_radii.bottom),
                ),
            ],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_masked_texture(
    frame: &mut GlesFrame<'_, '_>,
    texture: &GlesTexture,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    damage: &[Rectangle<i32, Physical>],
    transform: Transform,
    alpha: f32,
    program: &GlesTexProgram,
    clip: Rectangle<i32, Physical>,
    radii: CornerRadii,
    content_color: (f32, f32, f32, f32),
) -> Result<(), GlesError> {
    let uv_to_draw = texture_matrix(src, dst, texture.size(), transform, texture.is_y_inverted())
        .invert()
        .unwrap_or_else(Matrix3::identity);
    let uniforms = [
        Uniform::new("clip_size", (clip.size.w as f32, clip.size.h as f32)),
        Uniform::new(
            "draw_offset",
            (
                (dst.loc.x - clip.loc.x) as f32,
                (dst.loc.y - clip.loc.y) as f32,
            ),
        ),
        Uniform::new("corner_radii", (radii.top, radii.bottom)),
        Uniform::new(
            "uv_to_draw_col_0",
            (uv_to_draw.x.x, uv_to_draw.x.y, uv_to_draw.x.z),
        ),
        Uniform::new(
            "uv_to_draw_col_1",
            (uv_to_draw.y.x, uv_to_draw.y.y, uv_to_draw.y.z),
        ),
        Uniform::new(
            "uv_to_draw_col_2",
            (uv_to_draw.z.x, uv_to_draw.z.y, uv_to_draw.z.z),
        ),
        Uniform::new("content_color", content_color),
    ];
    frame.render_texture_from_to(
        texture,
        src,
        dst,
        damage,
        &[],
        transform,
        alpha,
        Some(program),
        &uniforms,
    )
}

fn texture_matrix(
    src: Rectangle<f64, Buffer>,
    destination: Rectangle<i32, Physical>,
    texture_size: smithay::utils::Size<i32, Buffer>,
    transform: Transform,
    y_inverted: bool,
) -> Matrix3<f32> {
    let transformed_source_size = transform.transform_size(src.size);
    let scale = transformed_source_size.to_f64() / destination.size.to_f64();
    let mut matrix = Matrix3::from_nonuniform_scale(scale.x as f32, scale.y as f32);
    let transformed_size = transformed_source_size;
    let translation = match transform {
        Transform::Normal => Matrix3::identity(),
        Transform::_90 => Matrix3::from_translation(Vector2::new(0.0, transformed_size.w as f32)),
        Transform::_180 => Matrix3::from_translation(Vector2::new(
            transformed_size.w as f32,
            transformed_size.h as f32,
        )),
        Transform::_270 => Matrix3::from_translation(Vector2::new(transformed_size.h as f32, 0.0)),
        Transform::Flipped => {
            Matrix3::from_translation(Vector2::new(transformed_size.w as f32, 0.0))
        }
        Transform::Flipped90 => Matrix3::identity(),
        Transform::Flipped180 => {
            Matrix3::from_translation(Vector2::new(0.0, transformed_size.h as f32))
        }
        Transform::Flipped270 => Matrix3::from_translation(Vector2::new(
            transformed_size.h as f32,
            transformed_size.w as f32,
        )),
    };
    matrix = transform.matrix() * matrix;
    matrix = translation * matrix;
    matrix = Matrix3::from_translation(Vector2::new(src.loc.x as f32, src.loc.y as f32)) * matrix;
    matrix = Matrix3::from_nonuniform_scale(
        1.0 / texture_size.w.max(1) as f32,
        1.0 / texture_size.h.max(1) as f32,
    ) * matrix;
    if y_inverted {
        matrix = Matrix3::new(1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0) * matrix;
    }
    matrix
}

pub fn scaled_metric(base: i32, scale: f32) -> i32 {
    let base = base.max(0) as f32;
    let scaled = (base * scale.max(0.0)).round();
    if base > 0.0 {
        scaled.max(1.0) as i32
    } else {
        0
    }
}

fn clamp_radius(radius: f32, width: i32, height: i32) -> f32 {
    radius.max(0.0).min(width.min(height).max(0) as f32 * 0.5)
}

fn clamp_radii(radii: CornerRadii, width: i32, height: i32) -> CornerRadii {
    CornerRadii {
        top: clamp_radius(radii.top, width, height),
        bottom: clamp_radius(radii.bottom, width, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_border_radii_are_concentric() {
        let metrics = metrics(8.0, 3.0);
        assert_eq!(metrics.content_radius, 8.0);
        assert_eq!(metrics.outer_radius, 11.0);
        assert_eq!(metrics.inner_offset, 3.75);
        assert_eq!(metrics.inner_radius, 7.25);
    }

    #[test]
    fn zero_radius_keeps_square_border_geometry() {
        assert_eq!(metrics(0.0, 3.0).outer_radius, 0.0);
    }

    #[test]
    fn nonzero_metrics_survive_zooming_out() {
        assert_eq!(scaled_metric(8, 0.05), 1);
        assert_eq!(scaled_metric(0, 0.05), 0);
    }

    #[test]
    fn tiny_windows_clamp_the_content_radius() {
        assert_eq!(clamp_radius(8.0, 10, 6), 3.0);
    }

    /// Rust mirror of the `rounded_alpha` GLSL in `SURFACE_SHADER` and
    /// `fullscreen_texture.rs`. Kept in step by hand; it exists to pin the
    /// interior term, which has no other executable coverage.
    fn rounded_alpha(coords: (f32, f32), size: (f32, f32), radius: f32) -> f32 {
        let radius = radius.clamp(0.0, size.0.min(size.1) * 0.5);
        let half = (size.0 * 0.5, size.1 * 0.5);
        let q = (
            (coords.0 - half.0).abs() - (half.0 - radius),
            (coords.1 - half.1).abs() - (half.1 - radius),
        );
        let outside = (q.0.max(0.0).powi(2) + q.1.max(0.0).powi(2)).sqrt();
        let distance = outside + q.0.max(q.1).min(0.0) - radius;
        let t = ((distance + 0.75) / 1.5).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }

    #[test]
    fn a_radius_easing_to_zero_never_dims_the_interior() {
        // The fullscreen transition eases the content radius to zero. Once it
        // drops under the smoothstep half-width, an interior-blind distance
        // reports -radius everywhere and fades the whole window toward 50%,
        // which read as an opacity flash just before the animation landed.
        let size = (1280.0, 800.0);
        for radius in [12.0, 1.0, 0.75, 0.5, 0.1, 0.001, 0.0] {
            let center = rounded_alpha((640.0, 400.0), size, radius);
            assert!(
                (center - 1.0).abs() < 1e-4,
                "radius {radius} dimmed the interior to {center}"
            );
        }
    }

    #[test]
    fn corners_are_still_rounded_and_edges_antialiased() {
        let size = (1280.0, 800.0);
        assert!(
            rounded_alpha((0.0, 0.0), size, 12.0) < 0.01,
            "corner is cut"
        );
        assert!(
            (rounded_alpha((0.0, 400.0), size, 12.0) - 0.5).abs() < 0.1,
            "straight edge keeps its half-covered antialiasing"
        );
    }
}
