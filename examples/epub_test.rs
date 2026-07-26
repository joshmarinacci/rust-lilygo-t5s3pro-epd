//! Smoke test for the three EPUB library modules: epub, layout, reader.
//!
//! Embeds `examples/test.epub` at compile time, exercises `EpubArchive`,
//! `layout_chapter`, and `ReaderState`, then prints results over serial.
//! No display hardware required.

#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::{clock::CpuClock, main};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

use epaper::epub::EpubArchive;
use epaper::layout::{FontMetrics, LayoutConfig};
use epaper::reader::ReaderState;

// Embedded test EPUB — place examples/test.epub in the repo (git-ignored if large).
const EPUB_DATA: &[u8] = include_bytes!("test.epub");

// A fixed-width measure function (10 px per ASCII character) for testing
// without a real font renderer attached.
fn fixed_measure(s: &str) -> u32 {
    s.chars().count() as u32 * 10
}

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let _p = esp_hal::init(config);

    // ── Set up the allocator (PSRAM not available in this minimal example;
    //    use internal SRAM only, 64 KB). ─────────────────────────────────────
    esp_alloc::heap_allocator!(size: 65536);

    println!("=== epub_test ===");

    // ── 1. Parse the EPUB archive ────────────────────────────────────────────
    let archive = EpubArchive::new(EPUB_DATA).expect("EpubArchive::new failed");
    let spine = archive.spine().expect("spine() failed");
    println!("Spine ({} chapter(s)):", spine.len());
    for (i, path) in spine.iter().enumerate() {
        println!("  [{}] {}", i, path);
    }

    // ── 2. Extract and strip chapter 1 ──────────────────────────────────────
    let text = archive.chapter_text(&spine[0]).expect("chapter_text failed");
    println!("\nChapter 1 plain text ({} bytes):", text.len());
    // Print first 400 chars so we can see the content on serial without flooding.
    let preview_end = text
        .char_indices()
        .nth(400)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    println!("{}", &text[..preview_end]);
    if text.len() > preview_end {
        println!("... [{} bytes more]", text.len() - preview_end);
    }

    // ── 3. Lay out chapter 1 with fixed-width metrics ────────────────────────
    let cfg = LayoutConfig {
        screen_width:  960,
        screen_height: 540,
        margin_x:      40,
        margin_y:      30,
        font: FontMetrics {
            line_height_px: 28,
            space_width_px: 10,
            measure: fixed_measure,
        },
    };

    let mut reader = ReaderState::new(text, &cfg);
    println!("\nLayout: {} pages", reader.page_count());
    println!("Page 1 of {}:", reader.page_count());
    println!("---");
    println!("{}", reader.current_text());
    println!("---");

    // ── 4. Turn pages forward through all pages ──────────────────────────────
    let total = reader.page_count();
    for p in 1..total {
        reader.turn_page(true);
        println!("\nPage {} of {}:", p + 1, total);
        println!("---");
        println!("{}", reader.current_text());
        println!("---");
    }

    // ── 5. Simulate a font-size change via relayout ──────────────────────────
    let larger_cfg = LayoutConfig {
        screen_width:  960,
        screen_height: 540,
        margin_x:      40,
        margin_y:      30,
        font: FontMetrics {
            line_height_px: 36,
            space_width_px: 13,
            measure: |s| s.chars().count() as u32 * 13,
        },
    };
    reader.go_to_page(0);
    // Anchor somewhere in the middle of the text (or byte 200 if only 1 page).
    reader.anchor_byte = reader.layout.pages
        .get(1)
        .map(|p| p.start)
        .unwrap_or_else(|| (reader.chapter_text.len() / 2).min(200));
    reader.relayout(&larger_cfg);
    println!(
        "\nAfter relayout (larger font): {} pages, landed on page {}",
        reader.page_count(),
        reader.current_page + 1,
    );

    // ── 6. Chapter 2 ─────────────────────────────────────────────────────────
    let text2 = archive.chapter_text(&spine[1]).expect("chapter 2 failed");
    println!("\nChapter 2 plain text ({} bytes):", text2.len());
    let end2 = text2
        .char_indices()
        .nth(300)
        .map(|(i, _)| i)
        .unwrap_or(text2.len());
    println!("{}", &text2[..end2]);

    println!("\n=== epub_test PASSED ===");

    loop {}
}
