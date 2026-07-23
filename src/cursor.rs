use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;

/// Fallback size for the procedurally-generated cursor, used when no system
/// xcursor theme can be loaded.
const FALLBACK_SIZE: usize = 24;

/// A cursor image, renderer-agnostic until it's actually drawn - texture
/// import happens inside each backend's `render()`, where the renderer is
/// already bound (design decision 12). Fixed/test-position only this round;
/// no real pointer tracking exists yet (design decision 14).
pub struct CursorImage {
    pub buffer: MemoryRenderBuffer,
}

impl CursorImage {
    /// Tries the system xcursor theme first, falls back to a small
    /// procedurally-generated filled circle on any failure - never panics,
    /// always produces something to draw.
    pub fn load() -> Self {
        let buffer = Self::load_from_theme().unwrap_or_else(Self::procedural_fallback);
        Self { buffer }
    }

    fn load_from_theme() -> Option<MemoryRenderBuffer> {
        let theme_name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_string());
        let theme = xcursor::CursorTheme::load(&theme_name);
        let path = theme.load_icon("default")?;
        let data = std::fs::read(path).ok()?;
        let image = xcursor::parser::parse_xcursor(&data)?.into_iter().next()?;

        // xcursor's `pixels_argb` field is misleadingly named: its actual
        // in-memory byte order is [A, R, G, B] (confirmed against xcursor's
        // own test fixtures), not the little-endian [B, G, R, A] layout DRM's
        // Fourcc::Argb8888 expects - pairing them scrambles channels (alpha
        // leaks into blue, etc). `pixels_rgba` ([R, G, B, A]) paired with
        // Fourcc::Abgr8888 (DRM's little-endian [R, G, B, A] format) is the
        // combination that's actually byte-order-correct.
        Some(MemoryRenderBuffer::from_slice(
            &image.pixels_rgba,
            Fourcc::Abgr8888,
            (image.width as i32, image.height as i32),
            1,
            Transform::Normal,
            None,
        ))
    }

    /// A small opaque circle, ARGB8888 (byte order B, G, R, A - the standard
    /// little-endian layout for that format). Not a vendored binary asset
    /// like anvil's fallback - generated in code, since pixel-perfect
    /// system-theme fidelity isn't the point here, just proving the
    /// RenderElement plumbing works end to end.
    fn procedural_fallback() -> MemoryRenderBuffer {
        let pixels = filled_circle_argb8888(FALLBACK_SIZE);
        MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (FALLBACK_SIZE as i32, FALLBACK_SIZE as i32),
            1,
            Transform::Normal,
            None,
        )
    }
}

/// Pure pixel generation, factored out of `procedural_fallback` so it's
/// testable without going through the opaque `MemoryRenderBuffer` wrapper
/// (which exposes no size/content accessors once constructed).
fn filled_circle_argb8888(size: usize) -> Vec<u8> {
    let center = size as f32 / 2.0;
    let radius = center - 1.0;

    let mut pixels = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            if dx * dx + dy * dy <= radius * radius {
                let idx = (y * size + x) * 4;
                pixels[idx] = 0xFF; // B
                pixels[idx + 1] = 0xFF; // G
                pixels[idx + 2] = 0xFF; // R
                pixels[idx + 3] = 0xFF; // A
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filled_circle_produces_the_expected_byte_count() {
        let pixels = filled_circle_argb8888(FALLBACK_SIZE);
        assert_eq!(pixels.len(), FALLBACK_SIZE * FALLBACK_SIZE * 4);
    }

    #[test]
    fn filled_circle_has_an_opaque_center_and_a_transparent_corner() {
        let size = FALLBACK_SIZE;
        let pixels = filled_circle_argb8888(size);

        let center_idx = (size / 2 * size + size / 2) * 4;
        assert_eq!(pixels[center_idx + 3], 0xFF, "center pixel should be opaque");

        let corner_idx = 0; // (0, 0)
        assert_eq!(pixels[corner_idx + 3], 0x00, "corner pixel should be transparent");
    }

    #[test]
    fn procedural_fallback_constructs_without_panicking() {
        let _buffer = CursorImage::procedural_fallback();
    }
}
