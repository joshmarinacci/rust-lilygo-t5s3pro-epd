extern crate alloc;
use alloc::vec::Vec;

// ── Public types ──────────────────────────────────────────────────────────────

/// A half-open byte range [start, end) into the chapter text string.
#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub start: usize,
    pub end:   usize,
}

/// The result of laying out one chapter: an ordered list of pages.
#[derive(Debug, Clone)]
pub struct Layout {
    pub pages: Vec<Page>,
}

/// Metrics needed to measure text for layout.
///
/// `measure` must be a plain function pointer (not a closure capturing state).
/// For integration with `TextRenderer`, store the renderer in a module-level
/// static and write a thin wrapper: `fn my_measure(s: &str) -> u32 { RENDERER.measure(...) }`.
pub struct FontMetrics {
    pub line_height_px: u32,
    /// Width of a single space character, in pixels. If 0, the cached ' ' width is used.
    pub space_width_px: u32,
    /// Returns the pixel advance width of the given UTF-8 string at the current font size.
    pub measure: fn(&str) -> u32,
}

/// Configuration for the layout engine.
pub struct LayoutConfig {
    pub screen_width:  u32,  // physical display width  (e.g. 960)
    pub screen_height: u32,  // physical display height (e.g. 540)
    pub margin_x:      u32,  // horizontal margin on each side
    pub margin_y:      u32,  // vertical margin on each side
    pub font:          FontMetrics,
}

// ── Core layout function ──────────────────────────────────────────────────────

/// Word-wrap `text` into pages using pixel-accurate font metrics.
///
/// Rules:
/// - Words are runs of non-whitespace characters.
/// - A word that does not fit on the current line wraps to the next.
/// - A word wider than `content_width` is placed alone on its line (no infinite loop).
/// - `'\n'` is a forced line break; `'\n\n'` is a paragraph break (adds `line_h / 2`
///   of extra vertical space between paragraphs).
/// - Leading spaces at the start of a line are silently dropped.
/// - `Page.start` / `Page.end` are byte offsets into `text` (UTF-8 safe).
pub fn layout_chapter(text: &str, cfg: &LayoutConfig) -> Layout {
    let content_w = cfg.screen_width.saturating_sub(2 * cfg.margin_x);
    let content_h = cfg.screen_height.saturating_sub(2 * cfg.margin_y);
    let line_h    = cfg.font.line_height_px;
    let para_gap  = line_h / 2;

    // Degenerate / zero-sized config: one page for everything.
    if content_w == 0 || content_h == 0 || line_h == 0 {
        let pages = if text.is_empty() {
            Vec::new()
        } else {
            alloc::vec![Page { start: 0, end: text.len() }]
        };
        return Layout { pages };
    }

    // ── ASCII glyph width cache ───────────────────────────────────────────────
    // Pre-measuring every printable ASCII character avoids calling `measure`
    // repeatedly for the overwhelmingly common case of ASCII text.
    let mut gcache = [0u32; 128];
    {
        let mut buf = [0u8; 4];
        for b in 32u8..127u8 {
            let s = char::from(b).encode_utf8(&mut buf);
            gcache[b as usize] = (cfg.font.measure)(s);
        }
    }
    let space_w = if cfg.font.space_width_px > 0 {
        cfg.font.space_width_px
    } else {
        gcache[b' ' as usize]
    };

    let bytes = text.as_bytes();
    let total = bytes.len();

    let mut pages         = Vec::new();
    let mut page_start    = 0usize;
    let mut line_y        = 0u32;   // top of current line, relative to page top
    let mut line_px       = 0u32;   // pixel width consumed so far on this line
    let mut pos           = 0usize;
    let mut pending_space = false;  // space token seen before the next word

    // Advance to the next line.  `extra` adds paragraph spacing.  `at` is the
    // byte offset that will become page_start if a page boundary is crossed.
    // Callers are responsible for resetting line_px and pending_space afterwards.
    macro_rules! next_line {
        ($extra:expr, $at:expr) => {{
            line_y += line_h + $extra;
            if line_y + line_h > content_h {
                pages.push(Page { start: page_start, end: $at });
                page_start = $at;
                line_y = 0;
            }
        }};
    }

    while pos < total {
        let b = bytes[pos];

        // ── Newlines ──────────────────────────────────────────────────────────
        if b == b'\n' {
            let double = pos + 1 < total && bytes[pos + 1] == b'\n';
            let skip   = if double { 2 } else { 1 };
            let extra  = if double { para_gap } else { 0 };
            next_line!(extra, pos + skip);
            line_px = 0;
            pending_space = false;
            pos += skip;
            continue;
        }

        // ── Spaces ────────────────────────────────────────────────────────────
        if b == b' ' {
            // Consume all consecutive spaces; only set pending_space when we're
            // mid-line so that lines never start with whitespace.
            if line_px > 0 { pending_space = true; }
            while pos < total && bytes[pos] == b' ' { pos += 1; }
            continue;
        }

        // ── Word ──────────────────────────────────────────────────────────────
        let word_start = pos;
        let mut word_px = 0u32;

        while pos < total && bytes[pos] != b' ' && bytes[pos] != b'\n' {
            let wb = bytes[pos];
            if wb < 128 {
                // Fast path: ASCII — look up cache.
                word_px += gcache[wb as usize];
                pos += 1;
            } else {
                // Slow path: multi-byte UTF-8 — scan to the end of the codepoint,
                // then measure the whole character as a string.
                let cs = pos;
                pos += 1;
                while pos < total && (bytes[pos] & 0xC0) == 0x80 { pos += 1; }
                word_px += (cfg.font.measure)(&text[cs..pos]);
            }
        }

        if word_start == pos { pos += 1; continue; } // safety skip for unexpected bytes

        // How much horizontal space does this word need?
        let gap          = if pending_space && line_px > 0 { space_w } else { 0 };
        let needed       = line_px + gap + word_px;

        if needed > content_w && line_px > 0 {
            // Word doesn't fit on the current line: wrap, then place at line start.
            next_line!(0, word_start);
            line_px       = word_px;
            pending_space = false;
        } else {
            // Fits (or is alone on an empty line — overflowing is unavoidable).
            line_px       = needed;
            pending_space = false;
        }
    }

    // Emit the final (possibly partial) page.
    if page_start < total || pages.is_empty() {
        pages.push(Page { start: page_start, end: total });
    }

    Layout { pages }
}
