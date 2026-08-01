use std::error::Error;

use image::RgbaImage;
use resvg::{tiny_skia, usvg};
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

const CIRCLE_SHADER: &str = include_str!("shaders/node_circle_shader.frag");
const CLUSTER_ICON: &[u8] = include_bytes!("assets/clusters.svg");
const ICON_SIZE: u32 = 64;

struct Resources {
    context: ContextId<GlesTexture>,
    texture: GlesTexture,
    circle: GlesTexProgram,
    icon_colors: [[u8; 4]; 2],
    icons: [GlesTexture; 2],
}

#[derive(Default)]
pub struct ClusterRenderer {
    resources: Option<Resources>,
}

#[derive(Debug)]
pub struct ClusterCoreElement {
    base: TextureRenderElement<GlesTexture>,
    texture: GlesTexture,
    program: GlesTexProgram,
    border: (f32, f32, f32, f32),
    fill: (f32, f32, f32, f32),
    fill_alpha: f32,
    element_alpha: f32,
    flat_fill: f32,
    center_flat_fill: f32,
}

#[derive(Debug)]
pub struct ClusterIconElement {
    base: TextureRenderElement<GlesTexture>,
}

impl ClusterRenderer {
    pub fn core(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        border_rgb: (f32, f32, f32),
        fill_rgb: (f32, f32, f32),
        opacity: f32,
    ) -> Result<ClusterCoreElement, Box<dyn Error>> {
        self.core_with_alpha(renderer, destination, border_rgb, fill_rgb, opacity, 1.0)
    }

    pub fn core_with_alpha(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        border_rgb: (f32, f32, f32),
        fill_rgb: (f32, f32, f32),
        opacity: f32,
        alpha: f32,
    ) -> Result<ClusterCoreElement, Box<dyn Error>> {
        self.ensure(renderer, [[255; 4]; 2])?;
        let resources = self.resources.as_ref().expect("resources ensured above");
        let source = Rectangle::<f64, Logical>::new(
            (0.0, 0.0).into(),
            (
                resources.texture.size().w as f64,
                resources.texture.size().h as f64,
            )
                .into(),
        );
        Ok(ClusterCoreElement {
            base: TextureRenderElement::from_static_texture(
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
            ),
            texture: resources.texture.clone(),
            program: resources.circle.clone(),
            border: (border_rgb.0, border_rgb.1, border_rgb.2, 3.0 / 26.0),
            fill: (fill_rgb.0, fill_rgb.1, fill_rgb.2, 1.0),
            fill_alpha: opacity.clamp(0.0, 1.0),
            element_alpha: alpha.clamp(0.0, 1.0),
            flat_fill: 0.0,
            center_flat_fill: 0.0,
        })
    }

    pub fn join_affordance(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        fill_rgb: (f32, f32, f32),
    ) -> Result<ClusterCoreElement, Box<dyn Error>> {
        let mut element =
            self.core_with_alpha(renderer, destination, fill_rgb, fill_rgb, 1.0, 0.9)?;
        element.border.3 = 0.0;
        element.flat_fill = 1.0;
        element.center_flat_fill = 1.0;
        Ok(element)
    }

    pub fn icon(
        &mut self,
        renderer: &mut GlesRenderer,
        destination: Rectangle<i32, Physical>,
        focused: bool,
        colors: [[u8; 4]; 2],
        alpha: f32,
    ) -> Result<ClusterIconElement, Box<dyn Error>> {
        self.ensure(renderer, colors)?;
        let resources = self.resources.as_ref().expect("resources ensured above");
        let texture = resources.icons[usize::from(focused)].clone();
        let source = Rectangle::<f64, Logical>::new(
            (0.0, 0.0).into(),
            (texture.size().w as f64, texture.size().h as f64).into(),
        );
        Ok(ClusterIconElement {
            base: TextureRenderElement::from_static_texture(
                Id::new(),
                resources.context.clone(),
                destination.loc.to_f64(),
                texture,
                1,
                Transform::Normal,
                Some(alpha.clamp(0.0, 1.0)),
                Some(source),
                Some(destination.size.to_logical(1)),
                None,
                Kind::Unspecified,
            ),
        })
    }

    fn ensure(
        &mut self,
        renderer: &mut GlesRenderer,
        icon_colors: [[u8; 4]; 2],
    ) -> Result<(), Box<dyn Error>> {
        let context = renderer.context_id();
        if self.resources.as_ref().is_some_and(|resources| {
            resources.context == context && resources.icon_colors == icon_colors
        }) {
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
        let circle = renderer.compile_custom_texture_shader(CIRCLE_SHADER, &uniforms)?;
        let [unfocused, focused] = icon_colors.map(|color| {
            let raster = raster_icon(color).ok_or("cluster SVG could not be rasterized")?;
            renderer
                .import_memory(
                    raster.as_raw(),
                    Fourcc::Abgr8888,
                    (ICON_SIZE as i32, ICON_SIZE as i32).into(),
                    false,
                )
                .map_err(|error| -> Box<dyn Error> { Box::new(error) })
        });
        self.resources = Some(Resources {
            context,
            texture,
            circle,
            icon_colors,
            icons: [unfocused?, focused?],
        });
        Ok(())
    }
}

fn raster_icon(color: [u8; 4]) -> Option<RgbaImage> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(CLUSTER_ICON, &options).ok()?;
    let svg_size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE)?;
    let scale = (ICON_SIZE as f32 / svg_size.width() as f32)
        .min(ICON_SIZE as f32 / svg_size.height() as f32);
    let dx = (ICON_SIZE as f32 - svg_size.width() as f32 * scale) * 0.5;
    let dy = (ICON_SIZE as f32 - svg_size.height() as f32 * scale) * 0.5;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut image = RgbaImage::from_vec(ICON_SIZE, ICON_SIZE, pixmap.data().to_vec())?;
    for pixel in image.pixels_mut() {
        let alpha = pixel[3] as u16;
        if alpha == 0 {
            continue;
        }
        let tinted_alpha = ((alpha * color[3] as u16) / 255) as u8;
        pixel[0] = ((color[0] as u16 * tinted_alpha as u16) / 255) as u8;
        pixel[1] = ((color[1] as u16 * tinted_alpha as u16) / 255) as u8;
        pixel[2] = ((color[2] as u16 * tinted_alpha as u16) / 255) as u8;
        pixel[3] = tinted_alpha;
    }
    Some(image)
}

impl Element for ClusterCoreElement {
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
        self.element_alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for ClusterCoreElement {
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
            self.element_alpha,
            Some(&self.program),
            &[
                Uniform::new("node_color", self.border),
                Uniform::new("fill_color", self.fill),
                Uniform::new("flat_fill", self.flat_fill),
                Uniform::new("center_flat_fill", self.center_flat_fill),
                Uniform::new("fill_alpha", self.fill_alpha),
            ],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl Element for ClusterIconElement {
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

impl RenderElement<GlesRenderer> for ClusterIconElement {
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

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn restored_assets_do_not_drift() {
        assert_eq!(
            format!("{:x}", Sha256::digest(CLUSTER_ICON)),
            "6a7bb2aea3ffea277874ab353415455f8c4b51123c5b46aca4bbdaa26d97442a"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(CIRCLE_SHADER.as_bytes())),
            "6f8dc2768418324866572805187f37052cbe877af4de982ee031cd987e327ada"
        );
    }
}
