use std::collections::HashMap;
use std::error::Error;
use std::time::{Duration, Instant};

use cosmic_text::{
    Attrs, Buffer as TextBuffer, Color, Family, FontSystem, Hinting, Metrics, Shaping, Style,
    SwashCache, Weight,
};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ContextId, ImportMem, Renderer};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};

const TEXT_CACHE_TTL: Duration = Duration::from_secs(30);
const HINTING_MAX_SIZE_PX: u16 = 16;

struct TextTexture {
    texture: GlesTexture,
    size: smithay::utils::Size<i32, Buffer>,
    last_used: Instant,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TextKey {
    text: String,
    family: String,
    size_px: u16,
    rgb: [u8; 3],
}

pub struct UiTextRenderer {
    context: Option<ContextId<GlesTexture>>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    font: halley_config::Font,
    text: HashMap<TextKey, TextTexture>,
}

impl Default for UiTextRenderer {
    fn default() -> Self {
        Self::new(&halley_config::Font::default())
    }
}

impl UiTextRenderer {
    pub fn new(font: &halley_config::Font) -> Self {
        Self {
            context: None,
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font: font.clone(),
            text: HashMap::new(),
        }
    }

    /// Replace global typography atomically and report whether a frame must
    /// be redrawn. Textures are configuration-specific and cannot survive a
    /// font change.
    pub fn reload_font(&mut self, font: &halley_config::Font) -> bool {
        if self.font == *font {
            return false;
        }
        self.font = font.clone();
        self.text.clear();
        true
    }

    pub fn measure(
        &mut self,
        renderer: &mut GlesRenderer,
        text: &str,
        scale: i32,
        rgb: [u8; 3],
    ) -> Result<Option<smithay::utils::Size<i32, Buffer>>, Box<dyn Error>> {
        let Some(key) = self.prepare_key(renderer, text, scale, rgb)? else {
            return Ok(None);
        };
        let entry = self.text.get_mut(&key).expect("text entry prepared");
        entry.last_used = Instant::now();
        Ok(Some(entry.size))
    }

    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        origin: Point<i32, Physical>,
        text: &str,
        scale: i32,
        rgb: [u8; 3],
        alpha: f32,
    ) -> Result<Option<PreparedUiText>, Box<dyn Error>> {
        if alpha <= 0.001 {
            return Ok(None);
        }
        let Some(key) = self.prepare_key(renderer, text, scale, rgb)? else {
            return Ok(None);
        };
        let context = self.context.as_ref().expect("context prepared").clone();
        let entry = self.text.get_mut(&key).expect("text entry prepared");
        entry.last_used = Instant::now();
        let source = Rectangle::<f64, Logical>::new(
            (0.0, 0.0).into(),
            (entry.size.w as f64, entry.size.h as f64).into(),
        );
        let base = TextureRenderElement::from_static_texture(
            Id::new(),
            context,
            origin.to_f64(),
            entry.texture.clone(),
            1,
            Transform::Normal,
            Some(alpha.clamp(0.0, 1.0)),
            Some(source),
            // The texture is already rasterized at its final physical size.
            // A 1:1 destination avoids the blur caused by stretching glyphs.
            Some(entry.size.to_logical(1, Transform::Normal)),
            None,
            Kind::Unspecified,
        );
        Ok(Some(PreparedUiText {
            element: UiTextElement { base },
            size: entry.size,
        }))
    }

    fn prepare_key(
        &mut self,
        renderer: &mut GlesRenderer,
        text: &str,
        scale: i32,
        rgb: [u8; 3],
    ) -> Result<Option<TextKey>, Box<dyn Error>> {
        if text.is_empty() {
            return Ok(None);
        }
        self.ensure_context(renderer);
        let now = Instant::now();
        self.text
            .retain(|_, entry| now.saturating_duration_since(entry.last_used) < TEXT_CACHE_TTL);
        let key = TextKey {
            text: text.to_string(),
            family: normalized_family(&self.font),
            size_px: font_size_for_scale(&self.font, scale),
            rgb,
        };
        if !self.text.contains_key(&key) {
            let Some((pixels, size)) = raster_text(
                &mut self.font_system,
                &mut self.swash_cache,
                &key.text,
                key.size_px,
                key.rgb,
                &key.family,
            ) else {
                return Ok(None);
            };
            let texture = renderer.import_memory(&pixels, Fourcc::Abgr8888, size.into(), false)?;
            self.text.insert(
                key.clone(),
                TextTexture {
                    texture,
                    size: size.into(),
                    last_used: now,
                },
            );
        }
        Ok(Some(key))
    }

    fn ensure_context(&mut self, renderer: &GlesRenderer) {
        let context = renderer.context_id();
        if self.context.as_ref() == Some(&context) {
            return;
        }
        self.context = Some(context);
        self.text.clear();
    }
}

pub struct PreparedUiText {
    pub element: UiTextElement,
    pub size: smithay::utils::Size<i32, Buffer>,
}

#[derive(Debug)]
pub struct UiTextElement {
    base: TextureRenderElement<GlesTexture>,
}

fn normalized_family(font: &halley_config::Font) -> String {
    let family = font.family.trim();
    if family.is_empty() {
        halley_config::Font::default().family
    } else {
        family.to_string()
    }
}

fn font_size_for_scale(font: &halley_config::Font, scale: i32) -> u16 {
    match scale.max(1) {
        1 => font.size.saturating_sub(2).max(8),
        2 => font.size,
        3 => font.size.saturating_add(4),
        value => font
            .size
            .saturating_add(((value - 2) as u16).saturating_mul(4)),
    }
}

fn raster_text(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    text: &str,
    size_px: u16,
    rgb: [u8; 3],
    family: &str,
) -> Option<(Vec<u8>, (i32, i32))> {
    let font_size = size_px.max(1) as f32;
    let mut buffer = TextBuffer::new(
        font_system,
        Metrics::new(font_size, (font_size * 1.25).ceil()),
    );
    buffer.set_hinting(if size_px <= HINTING_MAX_SIZE_PX {
        Hinting::Enabled
    } else {
        Hinting::Disabled
    });
    buffer.set_size(None, None);
    let request = parse_font_request(family);
    let resolved = resolve_named_family(font_system, request.family);
    buffer.set_text(
        text,
        &Attrs::new()
            .family(resolve_family(
                resolved.as_deref().unwrap_or(request.family),
            ))
            .style(request.style)
            .weight(request.weight),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, true);

    let mut width = 0_i32;
    let mut height = 0_i32;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w.ceil() as i32);
        height = height.max((run.line_top + run.line_height).ceil() as i32);
    }
    width = width.max(1);
    height = height.max((font_size * 1.25).ceil() as i32).max(1);

    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let color = Color::rgba(rgb[0], rgb[1], rgb[2], 255);
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            let physical = glyph.physical((0.0, run.line_y), 1.0);
            swash_cache.with_pixels(font_system, physical.cache_key, color, |gx, gy, pixel| {
                let x = physical.x + gx;
                let y = physical.y + gy;
                if x < 0 || y < 0 || x >= width || y >= height {
                    return;
                }
                let index = ((y as usize * width as usize) + x as usize) * 4;
                let alpha = pixel.a() as f32 / 255.0;
                // Smithay's GLES path expects premultiplied RGBA.
                pixels[index] = (pixel.r() as f32 * alpha).round() as u8;
                pixels[index + 1] = (pixel.g() as f32 * alpha).round() as u8;
                pixels[index + 2] = (pixel.b() as f32 * alpha).round() as u8;
                pixels[index + 3] = pixel.a();
            });
        }
    }
    Some((pixels, (width, height)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FontRequest<'a> {
    family: &'a str,
    style: Style,
    weight: Weight,
}

fn parse_font_request(requested: &str) -> FontRequest<'_> {
    let trimmed = requested.trim();
    let mut family = if trimmed.is_empty() {
        "monospace"
    } else {
        trimmed
    };
    let mut style = Style::Normal;
    let mut weight = Weight::NORMAL;
    loop {
        if style == Style::Normal {
            if let Some(stripped) = strip_suffix(family, &[" italic"]) {
                family = stripped;
                style = Style::Italic;
                continue;
            }
            if let Some(stripped) = strip_suffix(family, &[" oblique"]) {
                family = stripped;
                style = Style::Oblique;
                continue;
            }
        }
        if weight == Weight::NORMAL {
            for (suffixes, parsed) in [
                (
                    &[" extra bold", " extra-bold", " extrabold"][..],
                    Weight::EXTRA_BOLD,
                ),
                (
                    &[" semi bold", " semi-bold", " semibold"][..],
                    Weight::SEMIBOLD,
                ),
                (
                    &[" extra light", " extra-light", " extralight"][..],
                    Weight::EXTRA_LIGHT,
                ),
                (&[" bold"][..], Weight::BOLD),
                (&[" medium"][..], Weight::MEDIUM),
                (&[" light"][..], Weight::LIGHT),
                (&[" thin"][..], Weight::THIN),
                (&[" black"][..], Weight::BLACK),
            ] {
                if let Some(stripped) = strip_suffix(family, suffixes) {
                    family = stripped;
                    weight = parsed;
                    break;
                }
            }
            if weight != Weight::NORMAL {
                continue;
            }
        }
        break;
    }
    FontRequest {
        family,
        style,
        weight,
    }
}

fn strip_suffix<'a>(family: &'a str, suffixes: &[&str]) -> Option<&'a str> {
    let folded = family.to_ascii_lowercase();
    suffixes.iter().find_map(|suffix| {
        folded
            .ends_with(suffix)
            .then(|| family[..family.len() - suffix.len()].trim_end())
    })
}

fn resolve_family(family: &str) -> Family<'_> {
    match family.trim().to_ascii_lowercase().as_str() {
        "serif" => Family::Serif,
        "sans-serif" | "sans_serif" | "sansserif" | "sans" => Family::SansSerif,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        "monospace" => Family::Monospace,
        _ => Family::Name(family),
    }
}

fn resolve_named_family(font_system: &FontSystem, requested: &str) -> Option<String> {
    if matches!(
        resolve_family(requested),
        Family::Serif | Family::SansSerif | Family::Cursive | Family::Fantasy | Family::Monospace
    ) {
        return None;
    }
    let requested = fold_font_name(requested);
    font_system.db().faces().find_map(|face| {
        face.families
            .iter()
            .map(|(family, _)| family.as_str())
            .find(|family| fold_font_name(family) == requested)
            .map(str::to_string)
            .or_else(|| {
                (fold_font_name(&face.post_script_name) == requested)
                    .then(|| face.families.first().map(|(family, _)| family.clone()))
                    .flatten()
            })
    })
}

fn fold_font_name(name: &str) -> String {
    name.chars()
        .filter(|character| !matches!(character, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

impl Element for UiTextElement {
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

impl RenderElement<GlesRenderer> for UiTextElement {
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
    use super::*;

    #[test]
    fn legacy_scales_map_from_the_global_base_size() {
        let font = halley_config::Font {
            family: "monospace".to_string(),
            size: 11,
        };
        assert_eq!(font_size_for_scale(&font, 1), 9);
        assert_eq!(font_size_for_scale(&font, 2), 11);
        assert_eq!(font_size_for_scale(&font, 3), 15);
        assert_eq!(font_size_for_scale(&font, 4), 19);
    }

    #[test]
    fn family_suffixes_select_weight_and_style_without_losing_the_name() {
        let parsed = parse_font_request("CommitMono Nerd Font Semi-Bold Italic");
        assert_eq!(parsed.family, "CommitMono Nerd Font");
        assert_eq!(parsed.weight, Weight::SEMIBOLD);
        assert_eq!(parsed.style, Style::Italic);
    }

    #[test]
    fn cosmic_text_raster_is_nonempty_and_premultiplied() {
        let mut fonts = FontSystem::new();
        let mut swash = SwashCache::new();
        let (pixels, (width, height)) = raster_text(
            &mut fonts,
            &mut swash,
            "Halley",
            11,
            [80, 160, 240],
            "monospace",
        )
        .unwrap();
        assert!(width > 0 && height > 0);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
        assert!(
            pixels.chunks_exact(4).all(|pixel| {
                pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3]
            })
        );
    }

    #[test]
    fn font_reload_only_invalidates_on_change() {
        let mut renderer = UiTextRenderer::default();
        assert!(!renderer.reload_font(&halley_config::Font::default()));
        assert!(renderer.reload_font(&halley_config::Font {
            family: "sans-serif".to_string(),
            size: 12,
        }));
    }
}
