use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ContextId, ImportMem, Renderer, Texture};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};
use std::error::Error;

const SQUARE: &str = include_str!("shaders/node_square.frag");
const SQUIRCLE: &str = include_str!("shaders/node_squircle.frag");
const LABEL_SQUARE: &str = include_str!("shaders/ui_rect_square.frag");
const LABEL_ROUNDED: &str = include_str!("shaders/ui_rect_rounded.frag");

struct Resources {
    context: ContextId<GlesTexture>,
    texture: GlesTexture,
    square: GlesTexProgram,
    squircle: GlesTexProgram,
    label_square: GlesTexProgram,
    label_rounded: GlesTexProgram,
}

#[derive(Default)]
pub struct NodeRenderer {
    resources: Option<Resources>,
    icons: super::app_icon::AppIconCache,
}

#[derive(Debug)]
pub struct NodeRenderElement {
    base: TextureRenderElement<GlesTexture>,
    texture: GlesTexture,
    program: GlesTexProgram,
    border: (f32, f32, f32, f32),
    fill: (f32, f32, f32, f32),
    fill_alpha: f32,
    flat_fill: bool,
}

#[derive(Debug)]
pub struct LabelRenderElement {
    base: TextureRenderElement<GlesTexture>,
    texture: GlesTexture,
    program: GlesTexProgram,
    color: (f32, f32, f32, f32),
    size: (f32, f32),
    corner_radius: f32,
}

#[derive(Clone, Copy)]
pub struct NodeStyle {
    pub border_rgb: (f32, f32, f32),
    pub fill_rgb: (f32, f32, f32),
    pub opacity: f32,
    pub flat_fill: bool,
}

#[derive(Debug)]
pub struct NodeTextureElement {
    base: TextureRenderElement<GlesTexture>,
}

impl NodeRenderer {
    pub fn has_pending_icons(&self) -> bool {
        self.icons.has_pending()
    }

    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        shape: halley_config::NodeShape,
        style: NodeStyle,
    ) -> Result<NodeRenderElement, Box<dyn Error>> {
        self.ensure_resources(renderer)?;
        let resources = self.resources.as_ref().expect("ensured above");
        let program = match shape {
            halley_config::NodeShape::Square => resources.square.clone(),
            halley_config::NodeShape::Squircle => resources.squircle.clone(),
        };
        let source = Rectangle::<f64, Logical>::new(
            (0.0, 0.0).into(),
            (
                resources.texture.size().w as f64,
                resources.texture.size().h as f64,
            )
                .into(),
        );
        let base = TextureRenderElement::from_static_texture(
            Id::new(),
            resources.context.clone(),
            destination.loc.to_f64(),
            resources.texture.clone(),
            1,
            Transform::Normal,
            Some(1.0),
            Some(source),
            Some(destination.size.to_logical(1)),
            None,
            Kind::Unspecified,
        );
        let fill = (style.fill_rgb.0, style.fill_rgb.1, style.fill_rgb.2, 1.0);
        Ok(NodeRenderElement {
            base,
            texture: resources.texture.clone(),
            program,
            border: (
                style.border_rgb.0,
                style.border_rgb.1,
                style.border_rgb.2,
                3.0 / 26.0,
            ),
            fill,
            fill_alpha: style.opacity,
            flat_fill: style.flat_fill,
        })
    }

    pub fn label_element(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        shape: halley_config::NodeShape,
        rgb: (f32, f32, f32),
        alpha: f32,
    ) -> Result<LabelRenderElement, Box<dyn Error>> {
        self.ensure_resources(renderer)?;
        let resources = self.resources.as_ref().expect("ensured above");
        let program = match shape {
            halley_config::NodeShape::Square => resources.label_square.clone(),
            halley_config::NodeShape::Squircle => resources.label_rounded.clone(),
        };
        let source = Rectangle::<f64, Logical>::new(
            (0.0, 0.0).into(),
            (
                resources.texture.size().w as f64,
                resources.texture.size().h as f64,
            )
                .into(),
        );
        let base = TextureRenderElement::from_static_texture(
            Id::new(),
            resources.context.clone(),
            destination.loc.to_f64(),
            resources.texture.clone(),
            1,
            Transform::Normal,
            Some(alpha.clamp(0.0, 1.0)),
            Some(source),
            Some(destination.size.to_logical(1)),
            None,
            Kind::Unspecified,
        );
        Ok(LabelRenderElement {
            base,
            texture: resources.texture.clone(),
            program,
            color: (rgb.0, rgb.1, rgb.2, 1.0),
            size: (destination.size.w as f32, destination.size.h as f32),
            corner_radius: destination.size.h as f32 * 0.32,
        })
    }

    pub fn app_icon_element(
        &mut self,
        renderer: &mut GlesRenderer,
        app_id: &str,
        destination: Rectangle<i32, Physical>,
        alpha: f32,
    ) -> Option<NodeTextureElement> {
        self.icons
            .element(renderer, app_id, destination, alpha.clamp(0.0, 1.0))
            .map(|base| NodeTextureElement { base })
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
        let texture =
            renderer.import_memory(&[255_u8; 4 * 4 * 4], Fourcc::Abgr8888, (4, 4).into(), false)?;
        let uniforms = [
            UniformName::new("node_color", UniformType::_4f),
            UniformName::new("fill_color", UniformType::_4f),
            UniformName::new("flat_fill", UniformType::_1f),
            UniformName::new("center_flat_fill", UniformType::_1f),
            UniformName::new("fill_alpha", UniformType::_1f),
        ];
        let square = renderer.compile_custom_texture_shader(SQUARE, &uniforms)?;
        let squircle = renderer.compile_custom_texture_shader(SQUIRCLE, &uniforms)?;
        let label_uniforms = [
            UniformName::new("node_color", UniformType::_4f),
            UniformName::new("fill_color", UniformType::_4f),
            UniformName::new("rect_size", UniformType::_2f),
            UniformName::new("inner_rect_size", UniformType::_2f),
            UniformName::new("inner_rect_offset", UniformType::_2f),
            UniformName::new("corner_radius", UniformType::_1f),
            UniformName::new("inner_corner_radius", UniformType::_1f),
            UniformName::new("border_px", UniformType::_1f),
        ];
        let label_square = renderer.compile_custom_texture_shader(LABEL_SQUARE, &label_uniforms)?;
        let label_rounded =
            renderer.compile_custom_texture_shader(LABEL_ROUNDED, &label_uniforms)?;
        self.resources = Some(Resources {
            context,
            texture,
            square,
            squircle,
            label_square,
            label_rounded,
        });
        Ok(())
    }
}

impl Element for NodeRenderElement {
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
        1.0
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl Element for NodeTextureElement {
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
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.base.opaque_regions(scale)
    }
    fn alpha(&self) -> f32 {
        self.base.alpha()
    }
    fn kind(&self) -> Kind {
        self.base.kind()
    }
}

impl RenderElement<GlesRenderer> for NodeTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        <TextureRenderElement<GlesTexture> as RenderElement<GlesRenderer>>::draw(
            &self.base,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        <TextureRenderElement<GlesTexture> as RenderElement<GlesRenderer>>::underlying_storage(
            &self.base, renderer,
        )
    }
}

impl Element for LabelRenderElement {
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

impl RenderElement<GlesRenderer> for LabelRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.base.alpha(),
            Some(&self.program),
            &[
                Uniform::new("node_color", self.color),
                Uniform::new("fill_color", self.color),
                Uniform::new("rect_size", self.size),
                Uniform::new("inner_rect_size", self.size),
                Uniform::new("inner_rect_offset", (0.0_f32, 0.0_f32)),
                Uniform::new("corner_radius", self.corner_radius),
                Uniform::new("inner_corner_radius", self.corner_radius),
                Uniform::new("border_px", 0.0_f32),
            ],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl RenderElement<GlesRenderer> for NodeRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &[
                Uniform::new("node_color", self.border),
                Uniform::new("fill_color", self.fill),
                Uniform::new("flat_fill", if self.flat_fill { 1.0_f32 } else { 0.0_f32 }),
                Uniform::new("center_flat_fill", 0.0_f32),
                Uniform::new("fill_alpha", self.fill_alpha),
            ],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
