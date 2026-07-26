static FONT_DATA: &[u8] = include_bytes!("../fonts/Georgia.ttf");

pub struct TextRenderer {
    font: fontdue::Font,
}

impl TextRenderer {
    pub fn new() -> Self {
        let settings = fontdue::FontSettings::default();
        let font = fontdue::Font::from_bytes(FONT_DATA, settings)
            .expect("Georgia.ttf parse error");
        TextRenderer { font }
    }

    /// Create a renderer using arbitrary TTF/OTF data embedded via `include_bytes!`.
    pub fn with_font(data: &'static [u8]) -> Self {
        let font = fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
            .expect("font parse error");
        TextRenderer { font }
    }

    /// Draw `text` with its baseline at (start_x, baseline_y).
    /// `bg` is the background Gray4 value (15 = white). Returns the x position after the last glyph.
    /// `draw` receives (x: i32, y: i32, gray4: u8) for each non-transparent pixel.
    pub fn draw_str(
        &self,
        text: &str,
        start_x: i32,
        baseline_y: i32,
        font_px: f32,
        bg: u8,
        draw: &mut dyn FnMut(i32, i32, u8),
    ) -> i32 {
        let mut cursor_x = start_x as f32;
        for c in text.chars() {
            let (metrics, bitmap) = self.font.rasterize(c, font_px);
            let gx = cursor_x + metrics.xmin as f32;
            let gy = baseline_y - metrics.ymin - metrics.height as i32;
            for (idx, &cov) in bitmap.iter().enumerate() {
                if cov == 0 {
                    continue;
                }
                let px = gx as i32 + (idx % metrics.width) as i32;
                let py = gy + (idx / metrics.width) as i32;
                draw(px, py, blend_gray4(cov, 0, bg));
            }
            cursor_x += metrics.advance_width;
        }
        cursor_x as i32
    }

    /// Measure the pixel width of `text` at the given font size without rasterizing bitmaps.
    pub fn measure_width(&self, text: &str, font_px: f32) -> i32 {
        let mut width = 0.0f32;
        for c in text.chars() {
            width += self.font.metrics(c, font_px).advance_width;
        }
        width as i32
    }

    /// Advance width of a single character at the given font size (no bitmap allocated).
    pub fn char_advance(&self, c: char, font_px: f32) -> f32 {
        self.font.metrics(c, font_px).advance_width
    }

    /// Recommended line height (ascent - descent) at the given font size.
    pub fn line_height(&self, font_px: f32) -> i32 {
        match self.font.horizontal_line_metrics(font_px) {
            Some(m) => (m.ascent - m.descent) as i32,
            None => font_px as i32,
        }
    }
}

/// Blend coverage (0=transparent, 255=opaque) against fg/bg Gray4 values.
#[inline(always)]
fn blend_gray4(coverage: u8, fg_g4: u8, bg_g4: u8) -> u8 {
    let a = coverage as u16;
    ((fg_g4 as u16 * a + bg_g4 as u16 * (255 - a)) / 255) as u8
}
