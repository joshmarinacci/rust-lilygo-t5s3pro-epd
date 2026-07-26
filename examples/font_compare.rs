//! Font comparison example — portrait orientation, six serif/display families.
//!
//! Two views, two buttons:
//!   BOOT  (GPIO0)  — in all-fonts view: cycle display size
//!                    in single-font view: return to all-fonts view
//!   NEXT  (GPIO38) — in all-fonts view: enter single-font view (first face)
//!                    in single-font view: advance to next font face
//!
//! Fonts are subsetted to the 62 glyphs actually used, so fontdue is fast.
//! Each font is loaded one at a time (load→draw→drop) to keep PSRAM use low.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Pull},
    main,
};
use esp_println::println;

use embedded_graphics::{
    mono_font::{ascii::{FONT_7X13, FONT_10X20}, MonoTextStyle},
    pixelcolor::Gray4,
    prelude::*,
    text::{Alignment, Text},
};

use epaper::driver::{Display, DrawMode};
use epaper::driver::display::DisplayRotation;
use epaper::font::TextRenderer;

esp_bootloader_esp_idf::esp_app_desc!();

// ── Embedded font data (subsetted to 62 glyphs) ───────────────────────────────
static LITERATA_REG:  &[u8] = include_bytes!("../fonts/Literata-Regular.ttf");
static LITERATA_BOLD: &[u8] = include_bytes!("../fonts/Literata-Bold.ttf");
static LITERATA_IT:   &[u8] = include_bytes!("../fonts/Literata-Italic.ttf");
static VOLLKORN_REG:  &[u8] = include_bytes!("../fonts/Vollkorn-Regular.ttf");
static VOLLKORN_BOLD: &[u8] = include_bytes!("../fonts/Vollkorn-Bold.ttf");
static VOLLKORN_IT:   &[u8] = include_bytes!("../fonts/Vollkorn-Italic.ttf");
static NOTICIA_REG:   &[u8] = include_bytes!("../fonts/NoticiaText-Regular.ttf");
static NOTICIA_BOLD:  &[u8] = include_bytes!("../fonts/NoticiaText-Bold.ttf");
static NOTICIA_IT:    &[u8] = include_bytes!("../fonts/NoticiaText-Italic.ttf");
static CRIMSON_REG:   &[u8] = include_bytes!("../fonts/CrimsonPro-Regular.ttf");
static CRIMSON_BOLD:  &[u8] = include_bytes!("../fonts/CrimsonPro-Bold.ttf");
static CRIMSON_IT:    &[u8] = include_bytes!("../fonts/CrimsonPro-Italic.ttf");
static ATKINSON_REG:  &[u8] = include_bytes!("../fonts/AtkinsonHyperlegible-Regular.ttf");
static ATKINSON_BOLD: &[u8] = include_bytes!("../fonts/AtkinsonHyperlegible-Bold.ttf");
static ATKINSON_IT:   &[u8] = include_bytes!("../fonts/AtkinsonHyperlegible-Italic.ttf");
static ALEGREYA_REG:  &[u8] = include_bytes!("../fonts/Alegreya-Regular.ttf");
static ALEGREYA_BOLD: &[u8] = include_bytes!("../fonts/Alegreya-Bold.ttf");
static ALEGREYA_IT:   &[u8] = include_bytes!("../fonts/Alegreya-Italic.ttf");

// ── Sample text ───────────────────────────────────────────────────────────────
const TEXT_REG:  &str = "The five boxing wizards jump quickly. 0123";
const TEXT_BOLD: &str = "Bold \u{2014} Aa Bb Gg Qq Yy; Pack my box!";
const TEXT_ITAL: &str = "Italic \u{2014} Sphinx of black quartz.";

// ── Font table ────────────────────────────────────────────────────────────────
const FONTS: [(&str, &[u8], &[u8], &[u8]); 6] = [
    ("Literata",              LITERATA_REG, LITERATA_BOLD, LITERATA_IT),
    ("Vollkorn",              VOLLKORN_REG, VOLLKORN_BOLD, VOLLKORN_IT),
    ("Noticia Text",          NOTICIA_REG,  NOTICIA_BOLD,  NOTICIA_IT),
    ("Crimson Pro",           CRIMSON_REG,  CRIMSON_BOLD,  CRIMSON_IT),
    ("Atkinson Hyperlegible", ATKINSON_REG, ATKINSON_BOLD, ATKINSON_IT),
    ("Alegreya",              ALEGREYA_REG, ALEGREYA_BOLD, ALEGREYA_IT),
];

// ── Font sizes ────────────────────────────────────────────────────────────────
const SIZES: [f32; 5] = [15.0, 18.0, 22.0, 26.0, 32.0];

// ── Portrait logical canvas (display rotated 90°) ─────────────────────────────
const W: i32 = Display::HEIGHT as i32; // 540  logical width
const H: i32 = Display::WIDTH  as i32; // 960  logical height

const MARGIN_X: i32 = 20;
const HEADER_H: i32 = 34;
const GROUP_GAP: i32 = 6;

// ── Pixel helper: logical portrait → physical framebuffer (Rotate90) ──────────
#[inline(always)]
fn put_pixel(display: &mut Display<'_>, lx: i32, ly: i32, g4: u8) {
    if lx < 0 || lx >= W || ly < 0 || ly >= H { return; }
    let px = (Display::WIDTH as i32 - 1 - ly) as u16;
    let py = lx as u16;
    let _ = display.set_pixel(px, py, g4);
}

// ── Draw one text line (load → rasterize → drop) ─────────────────────────────
fn draw_line(
    display: &mut Display<'_>,
    font_data: &'static [u8],
    text: &str,
    x: i32,
    y: i32,
    font_px: f32,
) -> i32 {
    let renderer = TextRenderer::with_font(font_data);
    let lh = renderer.line_height(font_px);
    renderer.draw_str(text, x, y, font_px, 15, &mut |lx, ly, g4| {
        put_pixel(display, lx, ly, g4);
    });
    lh
}

// ── Horizontal rule ───────────────────────────────────────────────────────────
fn hline(display: &mut Display<'_>, y: i32, gray4: u8) {
    for x in 0..W { put_pixel(display, x, y, gray4); }
}

// ── Bitmap font label (FONT_7X13) above a group of text lines ─────────────────
// Returns the y advancement (= label height + gap sized to avoid TTF ascender overlap).
fn draw_label(display: &mut Display<'_>, text: &str, x: i32, y: i32, font_px: f32) -> i32 {
    Text::with_alignment(
        text,
        Point::new(x, y + 10), // y+10 ≈ FONT_7X13 ascent, so glyphs start at y
        MonoTextStyle::new(&FONT_7X13, Gray4::BLACK),
        Alignment::Left,
    ).draw(display).unwrap();
    // Advance far enough that the next TTF line's ascenders (≈ font_px * 0.75)
    // start below the label's descender line (y + 13), leaving a 3 px gap.
    13 + 3 + font_px as i32 * 3 / 4
}

// ── Bitmap font header (FONT_10X20) ──────────────────────────────────────────
fn draw_header(display: &mut Display<'_>, text: &str) {
    Text::with_alignment(
        text,
        Point::new(MARGIN_X, HEADER_H - 4),
        MonoTextStyle::new(&FONT_10X20, Gray4::BLACK),
        Alignment::Left,
    ).draw(display).unwrap();
    hline(display, HEADER_H, 0);
}

// ── Taint all physical rows so flush covers the full panel ────────────────────
fn taint_all(display: &mut Display<'_>) {
    for y in 0..Display::HEIGHT {
        let _ = display.set_pixel(0, y, 15);
    }
}

// ── Loading screen (bitmap font, no TTF parse, appears instantly) ─────────────
fn show_loading(display: &mut Display<'_>, text: &str) {
    Text::with_alignment(
        text,
        Point::new(W / 2, H / 2),
        MonoTextStyle::new(&FONT_10X20, Gray4::BLACK),
        Alignment::Center,
    ).draw(display).unwrap();
    display.flush(DrawMode::BlackOnWhite).unwrap();
}

// ── Two-flush clear: WhiteOnBlack erases previous dark pixels, ───────────────
//    BlackOnWhite then sets the new content cleanly.
fn two_flush(display: &mut Display<'_>, render_fn: &mut impl FnMut(&mut Display<'_>)) {
    render_fn(display);
    display.flush(DrawMode::WhiteOnBlack).unwrap(); // actively drives dark→white
    render_fn(display);
    display.flush(DrawMode::BlackOnWhite).unwrap(); // sets new dark pixels
}

// ── All-fonts view: one size, all six faces ───────────────────────────────────
fn render_all_fonts(display: &mut Display<'_>, size_idx: usize) {
    taint_all(display);
    let font_px = SIZES[size_idx];

    let hdr = format!("Font compare  |  {}px  |  NEXT = single face", font_px as u32);
    draw_header(display, &hdr);

    let mut y = HEADER_H + GROUP_GAP;

    for (name, reg, bold, italic) in FONTS {
        let adv = draw_label(display, name, MARGIN_X, y, font_px);
        y += adv;
        if y >= H { break; }

        let lh = draw_line(display, reg,    TEXT_REG,  MARGIN_X, y, font_px); y += lh + 2;
        if y >= H { break; }
        let lh = draw_line(display, bold,   TEXT_BOLD, MARGIN_X, y, font_px); y += lh + 2;
        if y >= H { break; }
        let lh = draw_line(display, italic, TEXT_ITAL, MARGIN_X, y, font_px); y += lh;
        if y >= H { break; }

        y += GROUP_GAP;
        if y < H { hline(display, y, 10); y += GROUP_GAP; }
    }
}

// ── Single-font view: one face, grouped by weight with all sizes ──────────────
fn render_single_font(display: &mut Display<'_>, face_idx: usize) {
    taint_all(display);
    let (name, reg, bold, italic) = FONTS[face_idx];

    let hdr = format!("{}  |  BOOT = all fonts  NEXT = next face", name);
    draw_header(display, &hdr);

    let mut y = HEADER_H + GROUP_GAP;

    let groups = [
        ("Regular", reg,    TEXT_REG),
        ("Bold",    bold,   TEXT_BOLD),
        ("Italic",  italic, TEXT_ITAL),
    ];

    for (weight_name, font_data, text) in groups {
        // Weight label — advance uses smallest size so ascenders clear the label
        let adv = draw_label(display, weight_name, MARGIN_X, y, SIZES[0]);
        y += adv;
        if y >= H { break; }

        // All four sizes, smallest to largest
        for (i, &font_px) in SIZES.iter().enumerate() {
            let lh = draw_line(display, font_data, text, MARGIN_X, y, font_px);
            let gap = if i + 1 < SIZES.len() { 2 } else { 0 };
            y += lh + gap;
            if y >= H { break; }
        }
        if y >= H { break; }

        y += GROUP_GAP;
        if y < H { hline(display, y, 10); y += GROUP_GAP; }
    }
}

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default()
        .with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 256 * 1024);
    esp_alloc::psram_allocator!(
        peripherals.PSRAM, esp_hal::psram,
        esp_hal::psram::PsramConfig { mode: esp_hal::psram::PsramMode::OctalSpi, ..Default::default() }
    );

    let mut gpio0  = peripherals.GPIO0;
    let mut gpio38 = peripherals.GPIO38;
    let boot_btn = Input::new(gpio0.reborrow(),  InputConfig::default().with_pull(Pull::Up));
    let next_btn = Input::new(gpio38.reborrow(), InputConfig::default().with_pull(Pull::Up));
    let delay = Delay::new();

    println!("font_compare: init display");
    let mut display = Display::new(
        epaper::pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
        peripherals.I2C0,
    ).expect("display init");

    display.set_rotation(DisplayRotation::Rotate90);
    delay.delay_millis(100);
    display.power_on();
    delay.delay_millis(10);

    // ── State ─────────────────────────────────────────────────────────────────
    let mut size_idx: usize = 1;   // default: 18 px
    let mut face_idx: usize = 0;   // which font in single-font view
    let mut single_mode = false;   // false = all-fonts view, true = single-font view

    // ── Initial render ────────────────────────────────────────────────────────
    display.clear().unwrap();
    show_loading(&mut display, "Rasterizing\u{2026} please wait");
    two_flush(&mut display, &mut |d| render_all_fonts(d, size_idx));
    println!("font_compare: ready");

    loop {
        let boot = boot_btn.is_low();
        let next = next_btn.is_low();

        if boot || next {
            delay.delay_millis(50);
            while boot_btn.is_low() || next_btn.is_low() {}
            delay.delay_millis(50);

            if boot {
                if single_mode {
                    // BOOT in single-font view → return to all-fonts view
                    single_mode = false;
                    println!("font_compare: all-fonts view, {}px", SIZES[size_idx] as u32);
                } else {
                    // BOOT in all-fonts view → next size
                    size_idx = (size_idx + 1) % SIZES.len();
                    println!("font_compare: size → {}px", SIZES[size_idx] as u32);
                }
            } else {
                // NEXT button
                if single_mode {
                    face_idx = (face_idx + 1) % FONTS.len();
                    println!("font_compare: face → {}", FONTS[face_idx].0);
                } else {
                    single_mode = true;
                    face_idx = 0;
                    println!("font_compare: single-font view → {}", FONTS[face_idx].0);
                }
            }

            display.clear().unwrap();
            show_loading(&mut display, "Rasterizing\u{2026} please wait");
            if single_mode {
                two_flush(&mut display, &mut |d| render_single_font(d, face_idx));
            } else {
                two_flush(&mut display, &mut |d| render_all_fonts(d, size_idx));
            }
        }

        delay.delay_millis(50);
    }
}
