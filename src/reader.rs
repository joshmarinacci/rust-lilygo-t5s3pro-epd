extern crate alloc;
use alloc::string::String;

use crate::layout::{layout_chapter, Layout, LayoutConfig};

/// Stateful e-reader: holds one chapter's text, its paginated layout, and the
/// current position expressed both as a page index and an anchor byte offset.
///
/// The anchor byte lets `relayout()` (called on font-size change) find the
/// page that contains the same sentence after re-pagination.
pub struct ReaderState {
    pub chapter_text: String,
    pub layout:       Layout,
    pub current_page: usize,
    /// Byte offset into `chapter_text` of the first character of the current page.
    /// Survives `relayout()` — the new page is the one whose range contains this offset.
    pub anchor_byte:  usize,
}

impl ReaderState {
    /// Create a new reader for `chapter_text` with the given layout config.
    pub fn new(chapter_text: String, cfg: &LayoutConfig) -> Self {
        let layout = layout_chapter(&chapter_text, cfg);
        Self {
            chapter_text,
            layout,
            current_page: 0,
            anchor_byte:  0,
        }
    }

    /// Re-paginate after a config change (e.g. font size). The current page is
    /// updated to the one containing `anchor_byte` so the reader lands on the
    /// same passage.
    pub fn relayout(&mut self, cfg: &LayoutConfig) {
        self.layout = layout_chapter(&self.chapter_text, cfg);
        self.current_page = self.layout.pages
            .iter()
            .position(|p| p.start <= self.anchor_byte && self.anchor_byte < p.end)
            .unwrap_or(0);
    }

    /// Turn one page forward or backward. No-op at the first/last page.
    pub fn turn_page(&mut self, forward: bool) {
        if forward {
            if self.current_page + 1 < self.layout.pages.len() {
                self.current_page += 1;
            }
        } else if self.current_page > 0 {
            self.current_page -= 1;
        }
        if let Some(p) = self.layout.pages.get(self.current_page) {
            self.anchor_byte = p.start;
        }
    }

    /// The slice of `chapter_text` that belongs to the current page.
    pub fn current_text(&self) -> &str {
        match self.layout.pages.get(self.current_page) {
            Some(p) => &self.chapter_text[p.start..p.end],
            None    => "",
        }
    }

    /// Total number of pages in the current layout.
    pub fn page_count(&self) -> usize {
        self.layout.pages.len()
    }

    /// Jump directly to a page by index (clamped to valid range).
    pub fn go_to_page(&mut self, page: usize) {
        self.current_page = page.min(self.layout.pages.len().saturating_sub(1));
        if let Some(p) = self.layout.pages.get(self.current_page) {
            self.anchor_byte = p.start;
        }
    }
}
