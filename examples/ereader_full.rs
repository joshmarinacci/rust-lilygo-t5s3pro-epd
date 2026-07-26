#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};

use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{DriveMode, Input, InputConfig, Pull},
    ledc::{
        channel::{self, ChannelIFace},
        timer::{self, TimerIFace},
        LSGlobalClkSource, Ledc, LowSpeed,
    },
    main,
    rtc_cntl::{
        reset_reason, wakeup_cause, Rtc, SocResetReason,
        sleep::{Ext0WakeupSource, WakeupLevel},
    },
    system::{Cpu, SleepSource},
    time::{Instant, Rate},
};
use esp_println::println;

use embedded_graphics::{
    draw_target::DrawTarget,
    geometry::OriginDimensions,
    mono_font::{
        ascii::{FONT_7X13, FONT_9X18, FONT_10X20},
        MonoTextStyle,
    },
    pixelcolor::Gray4,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};

use epaper::driver::{Display, DrawMode, Gt911};
use epaper::driver::gt911::GT911_ADDR_PRIMARY;
use epaper::epub::EpubArchive;
use epaper::font::TextRenderer;
use esp_storage::FlashStorage;
use sequential_storage::{cache::NoCache, map};

esp_bootloader_esp_idf::esp_app_desc!();

// ── Book EPUB (embedded in flash at compile time) ─────────────────────────────
const EPUB_DATA: &[u8] = include_bytes!("moby_dick.epub");

// ── I2C addresses ─────────────────────────────────────────────────────────────
const BQ27220_ADDR: u8 = 0x55;
const BQ25896_ADDR: u8 = 0x6B;

// ── Initial time (set before flashing; RTC persists across deep sleep) ────────
const INITIAL_HH: u64 = 12;
const INITIAL_MM: u64 = 0;

// ── Timeouts ─────────────────────────────────────────────────────────────────
const SLEEP_AFTER_SECS: u64 = 60;
const TIME_UPDATE_SECS: u64 = 60;

// ── Backlight ─────────────────────────────────────────────────────────────────
const BL_DUTY:  [u8; 4]   = [0, 25, 60, 100];
const BL_LABEL: [&str; 4] = ["Off", "Low", "Med", "Hi"];

// Chapters with fewer non-whitespace characters than this threshold are skipped
// (e.g. cover pages that only contain an <img> tag).
const MIN_CHAPTER_CHARS: usize = 50;

// ── Layout constants (physical display is always 960×540) ─────────────────────
const HEADER_H:      i32 = 52;
const FOOTER_H:      i32 = 30;
const CONTENT_TOP:   i32 = HEADER_H + 4;
const LEADING:       i32 = 4;    // extra spacing between lines

// Landscape (canvas 960×540)
const LAND_MARGIN:   i32 = 40;

// Portrait (canvas 540×960)
const PORT_MARGIN:   i32 = 30;

// ── Font sizes ────────────────────────────────────────────────────────────────
// Each entry is (landscape_px, portrait_px). Index 1 is the default.
const FONT_SIZES:  [(f32, f32); 4] = [(15.0, 13.0), (18.0, 16.0), (22.0, 20.0), (28.0, 26.0)];
const FONT_LABELS: [&str; 4]       = ["Sm", "Md", "Lg", "XL"];
const DEFAULT_FONT_SIZE: usize     = 1;

// ── Dropdown panel constants ──────────────────────────────────────────────────
const ITEM_H:     i32       = 40;  // height of each dropdown item row
const DROP_W:     i32       = 200; // width of option dropdowns
const BATT_W:     i32       = 320; // width of battery info panel
const ROT_LABELS: [&str; 4] = ["Landscape", "Portrait", "Inverted", "CCW"];

// ── Orientation ───────────────────────────────────────────────────────────────
#[derive(Copy, Clone, PartialEq)]
enum Orientation { Deg0, Deg90, Deg180, Deg270 }

impl Orientation {
    #[allow(dead_code)]
    fn next(self) -> Self {
        match self {
            Self::Deg0   => Self::Deg90,
            Self::Deg90  => Self::Deg180,
            Self::Deg180 => Self::Deg270,
            Self::Deg270 => Self::Deg0,
        }
    }
    fn is_portrait(self) -> bool {
        matches!(self, Self::Deg90 | Self::Deg270)
    }
    fn label(self) -> &'static str {
        match self { Self::Deg0 => "Land", Self::Deg90 => "Port", Self::Deg180 => "Inv", Self::Deg270 => "CCW" }
    }
    fn as_u32(self) -> u32 {
        match self { Self::Deg0 => 0, Self::Deg90 => 1, Self::Deg180 => 2, Self::Deg270 => 3 }
    }
    fn from_u32(v: u32) -> Self {
        match v & 3 { 1 => Self::Deg90, 2 => Self::Deg180, 3 => Self::Deg270, _ => Self::Deg0 }
    }
}

// ── Dropdown state ────────────────────────────────────────────────────────────
#[derive(Copy, Clone, PartialEq)]
enum Dropdown { Backlight, Battery, FontSize, Rotation }

// ── RotatedDisplay (mirrors ebook.rs) ────────────────────────────────────────
struct RotatedDisplay<'d, 'hw> {
    inner:       &'d mut Display<'hw>,
    orientation: Orientation,
}

impl<'d, 'hw> DrawTarget for RotatedDisplay<'d, 'hw> {
    type Color = Gray4;
    type Error = <Display<'hw> as DrawTarget>::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where I: IntoIterator<Item = Pixel<Self::Color>>
    {
        const W: i32 = Display::WIDTH  as i32; // 960
        const H: i32 = Display::HEIGHT as i32; // 540
        let o = self.orientation;
        self.inner.draw_iter(pixels.into_iter().map(|Pixel(Point { x, y }, c)| {
            let p = match o {
                Orientation::Deg0   => Point::new(x,     y    ),
                Orientation::Deg90  => Point::new(W-1-y, x    ),
                Orientation::Deg180 => Point::new(W-1-x, H-1-y),
                Orientation::Deg270 => Point::new(y,     H-1-x),
            };
            Pixel(p, c)
        }))
    }
}

impl<'d, 'hw> OriginDimensions for RotatedDisplay<'d, 'hw> {
    fn size(&self) -> Size {
        if self.orientation.is_portrait() {
            Size::new(Display::HEIGHT as u32, Display::WIDTH as u32)
        } else {
            Size::new(Display::WIDTH as u32, Display::HEIGHT as u32)
        }
    }
}

// ── RTC STORE register helpers ────────────────────────────────────────────────
// STORE0 = page_offset within chapter
// STORE1 = prev_page_offset within chapter
// STORE5 = packed bl/orientation/font_sz
// STORE6 = chapter_idx
fn rtc_store_read(idx: u8) -> u32 {
    let r = esp_hal::peripherals::LPWR::regs();
    match idx {
        0 => r.store0().read().data().bits(),
        1 => r.store1().read().data().bits(),
        5 => r.store5().read().data().bits(),
        6 => r.store6().read().data().bits(),
        _ => 0,
    }
}

fn rtc_store_write(idx: u8, val: u32) {
    let r = esp_hal::peripherals::LPWR::regs();
    match idx {
        0 => { r.store0().write(|w| unsafe { w.data().bits(val) }); }
        1 => { r.store1().write(|w| unsafe { w.data().bits(val) }); }
        5 => { r.store5().write(|w| unsafe { w.data().bits(val) }); }
        6 => { r.store6().write(|w| unsafe { w.data().bits(val) }); }
        _ => {}
    }
}

// ── Battery / charger helpers ─────────────────────────────────────────────────
fn read_soc(display: &mut Display<'_>) -> u16 {
    display.i2c_read_u16(BQ27220_ADDR, 0x2C).min(100)
}

fn is_charging(display: &mut Display<'_>) -> bool {
    let reg = display.i2c_read_u8(BQ25896_ADDR, 0x0B);
    reg & (1 << 2) != 0
}

// ── Time string from RTC ──────────────────────────────────────────────────────
fn rtc_time_str(rtc: &Rtc<'_>) -> String {
    let secs = (rtc.current_time_us() / 1_000_000) as u32;
    format!("{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60)
}

// ── Layout params for orientation ────────────────────────────────────────────
fn layout(o: Orientation, font_sz_idx: usize) -> (i32, i32, i32, f32, i32) {
    // (canvas_w, canvas_h, max_px, font_px, margin_x)
    let (land_px, port_px) = FONT_SIZES[font_sz_idx];
    if o.is_portrait() {
        let cw = Display::HEIGHT as i32;
        (cw, Display::WIDTH as i32, cw - PORT_MARGIN * 2, port_px, PORT_MARGIN)
    } else {
        let cw = Display::WIDTH as i32;
        (cw, Display::HEIGHT as i32, cw - LAND_MARGIN * 2, land_px, LAND_MARGIN)
    }
}

// ── Touch coordinate transform: physical → logical ────────────────────────────
fn phys_to_logical(tx: i32, ty: i32, o: Orientation) -> (i32, i32) {
    const W: i32 = 960;
    const H: i32 = 540;
    match o {
        Orientation::Deg0   => (tx,     ty    ),
        Orientation::Deg90  => (ty,     W-1-tx),
        Orientation::Deg180 => (W-1-tx, H-1-ty),
        Orientation::Deg270 => (H-1-ty, tx    ),
    }
}

// ── Paginator ─────────────────────────────────────────────────────────────────
// Breaks `text[start..]` into display lines; returns (lines, next_byte_offset).
fn paginate<'a>(
    renderer: &TextRenderer,
    text: &'a str,
    start: usize,
    content_h: i32,
    max_px: i32,
    font_px: f32,
) -> (Vec<&'a str>, usize) {
    let line_h = renderer.line_height(font_px) + LEADING;
    let max_lines = (content_h / line_h.max(1)) as usize;
    let mut lines = Vec::with_capacity(max_lines);
    let mut pos = start;
    while lines.len() < max_lines && pos < text.len() {
        let (line, next) = wrap_line_px(renderer, text, pos, max_px, font_px);
        lines.push(line);
        pos = next;
    }
    (lines, pos)
}

fn wrap_line_px<'a>(
    renderer: &TextRenderer,
    text: &'a str,
    pos: usize,
    max_px: i32,
    font_px: f32,
) -> (&'a str, usize) {
    let s = &text[pos..];
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n == 0 { return ("", pos); }

    let mut last_space: Option<usize> = None;
    let mut line_px = 0.0f32;
    let mut i = 0usize;

    loop {
        if i >= n {
            return (&s[..i], pos + i);
        }
        let b = bytes[i];
        if b == b'\n' {
            return (s[..i].trim_end(), pos + i + 1);
        }
        let advance = renderer.char_advance(b as char, font_px);
        if line_px + advance > max_px as f32 {
            if let Some(sp) = last_space {
                let line = s[..sp].trim_end();
                let mut nxt = sp + 1;
                while nxt < n && bytes[nxt] == b' ' { nxt += 1; }
                return (line, pos + nxt);
            }
            return (&s[..i], pos + i);
        }
        if b == b' ' { last_space = Some(i); }
        line_px += advance;
        i += 1;
    }
}

// ── Dropdown helpers ──────────────────────────────────────────────────────────

fn dropdown_x_and_w(kind: Dropdown, z: i32, cw: i32) -> (i32, i32) {
    let (x, w) = match kind {
        Dropdown::Battery   => (z,     BATT_W),
        Dropdown::Backlight => (z * 2, DROP_W),
        Dropdown::FontSize  => (z * 3, DROP_W),
        Dropdown::Rotation  => (z * 4, DROP_W),
    };
    (x.min(cw - w).max(0), w)
}

fn draw_option_dropdown<D>(
    target: &mut D,
    drop_x: i32,
    drop_w: i32,
    items: &[&str],
    selected: usize,
)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let style = MonoTextStyle::new(&FONT_9X18, Gray4::BLACK);
    // Clear the full dropdown area before drawing so page text doesn't bleed through.
    let total_h = items.len() as i32 * ITEM_H;
    Rectangle::new(Point::new(drop_x, HEADER_H), Size::new(drop_w as u32, total_h as u32))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(target).unwrap();
    for (i, &label) in items.iter().enumerate() {
        let row_y = HEADER_H + i as i32 * ITEM_H;
        let fill = if i == selected { Gray4::new(11) } else { Gray4::WHITE };
        Rectangle::new(Point::new(drop_x, row_y), Size::new(drop_w as u32, ITEM_H as u32))
            .into_styled(PrimitiveStyle::with_fill(fill))
            .draw(target).unwrap();
        Text::new(label, Point::new(drop_x + 10, row_y + ITEM_H - 12), style)
            .draw(target).unwrap();
    }
    let total_h = items.len() as i32 * ITEM_H;
    Rectangle::new(Point::new(drop_x, HEADER_H), Size::new(drop_w as u32, total_h as u32))
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 1))
        .draw(target).unwrap();
}

fn draw_battery_panel<D>(
    target: &mut D,
    drop_x: i32,
    soc: u16,
    charging: bool,
    voltage_mv: u16,
    current_ma: i16,
    remaining_mah: u16,
    full_mah: u16,
)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    const BATT_LINE_H: i32 = 24;
    const BATT_LINES:  i32 = 5;
    const PAD:         i32 = 10;
    let panel_h = BATT_LINES * BATT_LINE_H + PAD * 2;
    let style = MonoTextStyle::new(&FONT_9X18, Gray4::BLACK);
    let tx = drop_x + PAD;

    Rectangle::new(Point::new(drop_x, HEADER_H), Size::new(BATT_W as u32, panel_h as u32))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(target).unwrap();
    Rectangle::new(Point::new(drop_x, HEADER_H), Size::new(BATT_W as u32, panel_h as u32))
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 1))
        .draw(target).unwrap();

    let baseline = |row: i32| HEADER_H + PAD + (row + 1) * BATT_LINE_H - 5;
    Text::new(&format!("Battery:  {}%", soc),
        Point::new(tx, baseline(0)), style).draw(target).unwrap();
    Text::new(&format!("Charging: {}", if charging { "Yes" } else { "No" }),
        Point::new(tx, baseline(1)), style).draw(target).unwrap();
    Text::new(&format!("Voltage:  {} mV", voltage_mv),
        Point::new(tx, baseline(2)), style).draw(target).unwrap();
    Text::new(&format!("Current:  {} mA", current_ma),
        Point::new(tx, baseline(3)), style).draw(target).unwrap();
    Text::new(&format!("Capacity: {}/{} mAh", remaining_mah, full_mah),
        Point::new(tx, baseline(4)), style).draw(target).unwrap();
}

// ── Draw: header bar ──────────────────────────────────────────────────────────
fn draw_header<D>(
    target: &mut D,
    time: &str,
    soc: u16,
    charging: bool,
    bl: usize,
    font_sz_idx: usize,
    o: Orientation,
)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let cw = target.size().width as i32;
    let border = PrimitiveStyle::with_stroke(Gray4::BLACK, 2);
    let black  = MonoTextStyle::new(&FONT_10X20, Gray4::BLACK);

    Rectangle::new(Point::zero(), Size::new(cw as u32, HEADER_H as u32))
        .into_styled(border).draw(target).unwrap();

    let z = cw / 5; // zone width
    let ty = HEADER_H - 16; // text baseline

    Text::new(time, Point::new(8, ty), black).draw(target).unwrap();

    let bat = if charging { format!("{soc}%[+]") } else { format!("{soc}%") };
    Text::new(&bat, Point::new(z + 4, ty), black).draw(target).unwrap();

    let bl_s = format!("BL:{}", BL_LABEL[bl]);
    Text::new(&bl_s, Point::new(z * 2 + 4, ty), black).draw(target).unwrap();

    let sz_s = format!("Sz:{}", FONT_LABELS[font_sz_idx]);
    Text::new(&sz_s, Point::new(z * 3 + 4, ty), black).draw(target).unwrap();

    let rot_s = format!("Rot:{}", o.label());
    Text::new(&rot_s, Point::new(z * 4 + 4, ty), black).draw(target).unwrap();
}

// ── Draw: content text lines ──────────────────────────────────────────────────
fn draw_content(
    display: &mut Display<'_>,
    orientation: Orientation,
    renderer: &TextRenderer,
    lines: &[&str],
    margin_x: i32,
    font_px: f32,
) {
    const W: i32 = Display::WIDTH as i32;
    const H: i32 = Display::HEIGHT as i32;
    let line_h = renderer.line_height(font_px) + LEADING;
    for (i, &line) in lines.iter().enumerate() {
        let baseline_y = CONTENT_TOP + renderer.line_height(font_px) + i as i32 * line_h;
        renderer.draw_str(line, margin_x, baseline_y, font_px, 15, &mut |lx, ly, g4| {
            let (px, py) = match orientation {
                Orientation::Deg0   => (lx,     ly    ),
                Orientation::Deg90  => (W-1-ly, lx    ),
                Orientation::Deg180 => (W-1-lx, H-1-ly),
                Orientation::Deg270 => (ly,     H-1-lx),
            };
            if px >= 0 && px < W && py >= 0 && py < H {
                let _ = display.set_pixel(px as u16, py as u16, g4);
            }
        });
    }
}

// ── Draw: footer bar ──────────────────────────────────────────────────────────
fn draw_footer<D>(
    target: &mut D,
    status: &str,
    chapter: usize,
    chapter_count: usize,
    page: usize,
    total_pages: usize,
)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let cw = target.size().width  as i32;
    let ch = target.size().height as i32;
    let fy = ch - FOOTER_H;

    Rectangle::new(
        Point::new(0, fy),
        Size::new(cw as u32, FOOTER_H as u32),
    ).into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
     .draw(target).unwrap();

    Line::new(Point::new(0, fy), Point::new(cw, fy))
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 1))
        .draw(target).unwrap();

    let small = MonoTextStyle::new(&FONT_7X13, Gray4::BLACK);
    let ty = fy + FOOTER_H - 8;

    if !status.is_empty() {
        Text::with_alignment(status, Point::new(cw / 2, ty), small, Alignment::Center)
            .draw(target).unwrap();
    } else {
        if chapter_count > 0 {
            let s = format!("Ch.{}/{} p.{}/{}", chapter + 1, chapter_count, page, total_pages);
            Text::new(&s, Point::new(8, ty), small).draw(target).unwrap();
        }
        Text::with_alignment(
            "BOOT=prev  next=fwd",
            Point::new(cw - 8, ty), small, Alignment::Right,
        ).draw(target).unwrap();
    }
}

// ── Full page render; returns next_page_offset within chapter_text ────────────
fn render_page(
    display:       &mut Display<'_>,
    rtc:           &Rtc<'_>,
    renderer:      &TextRenderer,
    chapter_text:  &str,
    chapter_idx:   usize,
    chapter_count: usize,
    page_offset:   usize,
    orientation:   Orientation,
    bl_level:      usize,
    font_sz_idx:   usize,
    status:        &str,
) -> usize
{
    let time = rtc_time_str(rtc);
    let soc  = read_soc(display);
    let chrg = is_charging(display);
    let (_canvas_w, canvas_h, max_px, font_px, margin_x) = layout(orientation, font_sz_idx);
    let content_h = canvas_h - CONTENT_TOP - FOOTER_H;

    let line_h = renderer.line_height(font_px) + LEADING;
    let (lines, next_offset) = paginate(renderer, chapter_text, page_offset, content_h, max_px, font_px);

    // Rough page-within-chapter estimate from byte offset.
    let avg_char_px = (font_px * 0.55) as usize;
    let line_chars  = (max_px as usize / avg_char_px.max(1)).max(1);
    let lines_per_page = (content_h / line_h.max(1)) as usize;
    let chars_per_page = (line_chars * lines_per_page).max(1);
    let page_num    = page_offset / chars_per_page + 1;
    let total_pages = (chapter_text.len() / chars_per_page + 1).max(page_num);

    {
        let mut rot = RotatedDisplay { inner: display, orientation };
        draw_header(&mut rot, &time, soc, chrg, bl_level, font_sz_idx, orientation);
        draw_footer(&mut rot, status, chapter_idx, chapter_count, page_num, total_pages);
    }
    draw_content(display, orientation, renderer, &lines, margin_x, font_px);

    next_offset
}

// ── Partial header update ─────────────────────────────────────────────────────
fn update_header_only(
    display:     &mut Display<'_>,
    rtc:         &Rtc<'_>,
    bl_level:    usize,
    font_sz_idx: usize,
    orientation: Orientation,
) {
    let time = rtc_time_str(rtc);
    let soc  = read_soc(display);
    let chrg = is_charging(display);
    let mut rot = RotatedDisplay { inner: display, orientation };
    draw_header(&mut rot, &time, soc, chrg, bl_level, font_sz_idx, orientation);
}

// ── Partial footer update ─────────────────────────────────────────────────────
fn update_footer_only(display: &mut Display<'_>, msg: &str, orientation: Orientation) {
    let mut rot = RotatedDisplay { inner: display, orientation };
    draw_footer(&mut rot, msg, 0, 0, 0, 0);
}

// ── Flash adapter: wraps blocking FlashStorage for sequential-storage's async API ─
struct FlashAdapter(FlashStorage);

impl embedded_storage::nor_flash::ErrorType for FlashAdapter {
    type Error = esp_storage::FlashStorageError;
}

impl embedded_storage_async::nor_flash::ReadNorFlash for FlashAdapter {
    const READ_SIZE: usize = FlashStorage::WORD_SIZE as usize;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        embedded_storage::nor_flash::ReadNorFlash::read(&mut self.0, offset, bytes)
    }

    fn capacity(&self) -> usize {
        embedded_storage::nor_flash::ReadNorFlash::capacity(&self.0)
    }
}

impl embedded_storage_async::nor_flash::NorFlash for FlashAdapter {
    const WRITE_SIZE: usize = FlashStorage::WORD_SIZE as usize;
    const ERASE_SIZE: usize = FlashStorage::SECTOR_SIZE as usize;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        embedded_storage::nor_flash::NorFlash::erase(&mut self.0, from, to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        embedded_storage::nor_flash::NorFlash::write(&mut self.0, offset, bytes)
    }
}

// ── Minimal no_std executor ────────────────────────────────────────────────────
fn block_on<F: core::future::Future>(mut f: F) -> F::Output {
    use core::{
        pin::Pin,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };
    static VTABLE: RawWakerVTable =
        RawWakerVTable::new(|p| RawWaker::new(p, &VTABLE), |_| {}, |_| {}, |_| {});
    let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    loop {
        match unsafe { Pin::new_unchecked(&mut f) }.poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => {}
        }
    }
}

// ── Persistent reading position ────────────────────────────────────────────────
// Key 0: page_offset (u32) within current chapter
// Key 1: chapter_idx (u32)
const NVS_FLASH_RANGE: core::ops::Range<u32> = 0x9000..0xF000;

// Returns (page_offset, chapter_idx, font_sz_idx, orientation_u32, bl_level).
// Keys: 0=pos, 1=chapter, 2=font_sz, 3=orientation, 4=bl_level
fn flash_load_position() -> (usize, usize, usize, u32, usize) {
    let mut flash = FlashAdapter(FlashStorage::new());
    let mut cache = NoCache::new();
    let mut buf   = [0u8; 64];

    let mut load = |key: u8, default: u32| -> u32 {
        match block_on(map::fetch_item::<u8, u32, _>(
            &mut flash, NVS_FLASH_RANGE, &mut cache, &mut buf, &key,
        )) {
            Ok(Some(v)) => v,
            _           => default,
        }
    };

    let pos     = load(0, 0) as usize;
    let chapter = load(1, 0) as usize;
    let font_sz = load(2, DEFAULT_FONT_SIZE as u32) as usize;
    let ori     = load(3, 0);
    let bl      = load(4, 1) as usize;

    println!("flash: ch={} pos={} font={} ori={} bl={}", chapter, pos, font_sz, ori, bl);
    (pos, chapter, font_sz, ori, bl)
}

fn flash_save_position(
    pos:         usize,
    chapter_idx: usize,
    font_sz_idx: usize,
    orientation: Orientation,
    bl_level:    usize,
) {
    let mut flash = FlashAdapter(FlashStorage::new());
    let mut cache = NoCache::new();
    let mut buf   = [0u8; 64];

    let mut save = |key: u8, val: u32| {
        if let Err(e) = block_on(map::store_item::<u8, u32, _>(
            &mut flash, NVS_FLASH_RANGE, &mut cache, &mut buf, &key, &val,
        )) {
            println!("flash: save key {} error {:?}", key, e);
        }
    };

    save(0, pos as u32);
    save(1, chapter_idx as u32);
    save(2, font_sz_idx as u32);
    save(3, orientation.as_u32());
    save(4, bl_level as u32);
}

// ── Two-pass dropdown close ────────────────────────────────────────────────────
fn restore_after_dropdown(
    display:       &mut Display<'_>,
    rtc:           &Rtc<'_>,
    renderer:      &TextRenderer,
    chapter_text:  &str,
    chapter_idx:   usize,
    chapter_count: usize,
    page_offset:   usize,
    orientation:   Orientation,
    bl_level:      usize,
    font_sz_idx:   usize,
) -> usize {
    display.fill(0xF).unwrap();
    display.flush(DrawMode::WhiteOnBlack).unwrap();
    let next = render_page(display, rtc, renderer, chapter_text, chapter_idx, chapter_count,
                           page_offset, orientation, bl_level, font_sz_idx, "");
    display.flush(DrawMode::BlackOnWhite).unwrap();
    next
}

// ── Fast-scroll helpers ───────────────────────────────────────────────────────

fn compute_chars_per_page(renderer: &TextRenderer, orientation: Orientation, font_sz_idx: usize) -> usize {
    let (_, canvas_h, max_px, font_px, _) = layout(orientation, font_sz_idx);
    let content_h = canvas_h - CONTENT_TOP - FOOTER_H;
    let line_h = renderer.line_height(font_px) + LEADING;
    let avg_char_px = (font_px * 0.55) as usize;
    let line_chars = (max_px as usize / avg_char_px.max(1)).max(1);
    let lines_per_page = (content_h / line_h.max(1)) as usize;
    (line_chars * lines_per_page).max(1)
}

fn draw_fast_scroll_dialog<D>(target: &mut D, page_num: usize, total_pages: usize)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let cw = target.size().width as i32;
    let ch = target.size().height as i32;
    // In portrait (ch > cw, canvas 540×960 logical), logical-X maps to physical rows via the
    // Deg90/Deg270 transform. Swap to a narrow logical-X dialog so both orientations taint
    // exactly the same ~70 physical rows and flush at the same speed.
    let is_portrait = ch > cw;
    let (dlg_w, dlg_h): (u32, u32) = if is_portrait { (70, 300) } else { (300, 70) };
    let dlg_x = (cw - dlg_w as i32) / 2;
    let dlg_y = (ch - dlg_h as i32) / 2;
    let rect = Rectangle::new(Point::new(dlg_x, dlg_y), Size::new(dlg_w, dlg_h));
    rect.into_styled(PrimitiveStyle::with_fill(Gray4::WHITE)).draw(target).unwrap();
    rect.into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2)).draw(target).unwrap();
    // Portrait: abbreviated label fits the narrow 70px logical-X dimension.
    // Landscape: full label fits the wide 300px logical-X dimension.
    if is_portrait {
        let label = format!("{}/{}", page_num, total_pages);
        Text::with_alignment(
            &label,
            Point::new(dlg_x + dlg_w as i32 / 2, dlg_y + dlg_h as i32 / 2 + 4),
            MonoTextStyle::new(&FONT_7X13, Gray4::BLACK),
            Alignment::Center,
        ).draw(target).unwrap();
    } else {
        let label = format!("p. {} / {}", page_num, total_pages);
        Text::with_alignment(
            &label,
            Point::new(dlg_x + dlg_w as i32 / 2, dlg_y + dlg_h as i32 / 2 + 9),
            MonoTextStyle::new(&FONT_10X20, Gray4::BLACK),
            Alignment::Center,
        ).draw(target).unwrap();
    }
}

// Advances exactly one page forward from `start` without allocating a Vec.
fn advance_page_offset(
    renderer: &TextRenderer,
    text: &str,
    start: usize,
    content_h: i32,
    max_px: i32,
    font_px: f32,
) -> usize {
    let line_h = renderer.line_height(font_px) + LEADING;
    let max_lines = (content_h / line_h.max(1)) as usize;
    let mut pos = start;
    for _ in 0..max_lines {
        if pos >= text.len() { break; }
        let (_, next) = wrap_line_px(renderer, text, pos, max_px, font_px);
        if next == pos { break; }
        pos = next;
    }
    pos
}

// Held-button fast-scroll: shows a centered page-number dialog, advancing one page
// per EPD refresh cycle. Returns the new page_offset (unchanged if already at boundary).
fn fast_scroll(
    forward: bool,
    display: &mut Display<'_>,
    renderer: &TextRenderer,
    chapter_text: &str,
    start_offset: usize,
    orientation: Orientation,
    font_sz_idx: usize,
    is_held: &mut dyn FnMut() -> bool,
) -> usize {
    let (_, canvas_h, max_px, font_px, _) = layout(orientation, font_sz_idx);
    let content_h = canvas_h - CONTENT_TOP - FOOTER_H;
    let chars_per_page = compute_chars_per_page(renderer, orientation, font_sz_idx);
    let total_pages = (chapter_text.len() / chars_per_page + 1).max(1);
    let mut scroll_offset = start_offset;

    // Two-flush pattern: WhiteOnBlack clears the dialog rows on the EPD panel,
    // then BlackOnWhite renders the dialog content. Draw through RotatedDisplay
    // so the dialog is centered and oriented to match the current reading layout.
    let pn = scroll_offset / chars_per_page + 1;
    { let mut rot = RotatedDisplay { inner: display, orientation };
      draw_fast_scroll_dialog(&mut rot, pn, total_pages); }
    display.flush(DrawMode::WhiteOnBlack).unwrap();
    { let mut rot = RotatedDisplay { inner: display, orientation };
      draw_fast_scroll_dialog(&mut rot, pn, total_pages); }
    display.flush(DrawMode::BlackOnWhite).unwrap();

    // Advance as fast as the EPD can redraw — no artificial delay.
    loop {
        if !is_held() { break; }
        let new_offset = if forward {
            let next = advance_page_offset(renderer, chapter_text, scroll_offset,
                                           content_h, max_px, font_px);
            if next > scroll_offset { next } else { scroll_offset }
        } else {
            scroll_offset.saturating_sub(chars_per_page)
        };
        scroll_offset = new_offset;

        let pn = scroll_offset / chars_per_page + 1;
        { let mut rot = RotatedDisplay { inner: display, orientation };
          draw_fast_scroll_dialog(&mut rot, pn, total_pages); }
        display.flush(DrawMode::WhiteOnBlack).unwrap();
        if !is_held() { break; }
        { let mut rot = RotatedDisplay { inner: display, orientation };
          draw_fast_scroll_dialog(&mut rot, pn, total_pages); }
        display.flush(DrawMode::BlackOnWhite).unwrap();
    }

    scroll_offset
}

// ── Main ──────────────────────────────────────────────────────────────────────
#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default()
        .with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::psram_allocator!(
        peripherals.PSRAM, esp_hal::psram,
        esp_hal::psram::PsramConfig { mode: esp_hal::psram::PsramMode::OctalSpi, ..Default::default() }
    );

    let mut gpio0 = peripherals.GPIO0;
    let mut rtc = Rtc::new(peripherals.LPWR);

    // ── Parse EPUB once at startup ────────────────────────────────────────────
    let archive = EpubArchive::new(EPUB_DATA).expect("epub: parse failed");
    let spine = archive.spine().expect("epub: spine failed");
    let chapter_count = spine.len();
    println!("epub: {} chapters", chapter_count);

    // ── Boot type and persisted state ─────────────────────────────────────────
    let is_first_boot = reset_reason(Cpu::ProCpu) != Some(SocResetReason::CoreDeepSleep);

    let (mut page_offset, mut prev_page_offset, mut bl_level, mut orientation,
         mut font_sz_idx, mut chapter_idx, wake_status) =
        if is_first_boot {
            rtc.set_current_time_us((INITIAL_HH * 3600 + INITIAL_MM * 60) * 1_000_000);
            let (saved_pos, saved_ch, saved_font, saved_ori, saved_bl) = flash_load_position();
            let ch = saved_ch.min(chapter_count.saturating_sub(1));
            let ws = if saved_pos > 0 || saved_ch > 0 { "Resumed" } else { "" };
            println!("ereader: first boot, ch={} pos={}", ch, saved_pos);
            (saved_pos, saved_pos,
             saved_bl.min(BL_DUTY.len() - 1),
             Orientation::from_u32(saved_ori),
             saved_font.min(FONT_SIZES.len() - 1),
             ch, ws)
        } else {
            let po   = rtc_store_read(0) as usize;
            let ppo  = rtc_store_read(1) as usize;
            let pack = rtc_store_read(5);
            let ch   = (rtc_store_read(6) as usize).min(chapter_count.saturating_sub(1));
            let bl   = (pack & 0xFF) as usize;
            let ori  = Orientation::from_u32(pack >> 8);
            let sz   = ((pack >> 10) & 0x3) as usize;
            let ws   = match wakeup_cause() {
                SleepSource::Ext0 => "Awake! BOOT=prev  next=fwd",
                _                 => "Awake!",
            };
            println!("ereader: woke — ch={} pos={} bl={} sz={}", ch, po, bl, sz);
            (po, ppo, bl.min(3), ori, sz.min(FONT_SIZES.len() - 1), ch, ws)
        };

    // ── Load current chapter text (skip image-only/empty chapters) ───────────
    let mut chapter_text: String = archive
        .chapter_text(&spine[chapter_idx])
        .expect("epub: chapter load failed");
    while chapter_text.trim().len() < MIN_CHAPTER_CHARS && chapter_idx + 1 < chapter_count {
        chapter_idx += 1;
        chapter_text = archive.chapter_text(&spine[chapter_idx]).expect("epub: chapter load");
    }

    // ── Buttons ───────────────────────────────────────────────────────────────
    let boot_btn = Input::new(gpio0.reborrow(), InputConfig::default().with_pull(Pull::Up));
    let next_btn = Input::new(peripherals.GPIO38, InputConfig::default().with_pull(Pull::Up));

    let delay = Delay::new();

    // ── Display ───────────────────────────────────────────────────────────────
    let mut display = Display::new(
        epaper::pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
        peripherals.I2C0,
    ).expect("display init");

    delay.delay_millis(100);
    display.power_on();
    delay.delay_millis(10);

    // ── Touch ─────────────────────────────────────────────────────────────────
    let touch_addr = display.detect_touch_addr().unwrap_or_else(|| {
        println!("GT911 not found; defaulting to 0x{:02X}", GT911_ADDR_PRIMARY);
        GT911_ADDR_PRIMARY
    });
    let mut gt911 = Gt911::new(touch_addr);
    display.configure_touch(&mut gt911, 960, 540);
    delay.delay_millis(200);
    display.init_touch(&mut gt911);

    // ── Backlight (LEDC, GPIO11) ──────────────────────────────────────────────
    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    let mut lstimer0 = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    lstimer0.configure(timer::config::Config {
        duty:         timer::config::Duty::Duty8Bit,
        clock_source: timer::LSClockSource::APBClk,
        frequency:    Rate::from_khz(1),
    }).unwrap();

    let mut bl_ch = ledc.channel(channel::Number::Channel0, peripherals.GPIO11);
    bl_ch.configure(channel::config::Config {
        timer:      &lstimer0,
        duty_pct:   0,
        drive_mode: DriveMode::PushPull,
    }).unwrap();
    bl_ch.set_duty(BL_DUTY[bl_level]).unwrap();

    // ── Font renderer ─────────────────────────────────────────────────────────
    let renderer = TextRenderer::new();

    // ── Initial render ────────────────────────────────────────────────────────
    display.clear().unwrap();
    let mut next_page_offset = render_page(
        &mut display, &rtc, &renderer, &chapter_text, chapter_idx, chapter_count,
        page_offset, orientation, bl_level, font_sz_idx, wake_status,
    );
    display.flush(DrawMode::BlackOnWhite).unwrap();

    let mut last_interaction = Instant::now();
    let mut last_time_update = Instant::now();
    let mut redraw = false;
    let mut open_dropdown: Option<Dropdown> = None;

    // ── Main loop ─────────────────────────────────────────────────────────────
    loop {
        // ── BOOT = previous page (or dismiss dropdown) ───────────────────────
        if boot_btn.is_low() {
            delay.delay_millis(50);

            if open_dropdown.is_some() {
                while boot_btn.is_low() { delay.delay_millis(10); }
                delay.delay_millis(50);
                open_dropdown = None;
                next_page_offset = restore_after_dropdown(
                    &mut display, &rtc, &renderer, &chapter_text,
                    chapter_idx, chapter_count, page_offset, orientation, bl_level, font_sz_idx,
                );
            } else {
                let hold_start = Instant::now();
                while boot_btn.is_low() && hold_start.elapsed().as_millis() < 1000 {
                    delay.delay_millis(10);
                }
                if boot_btn.is_low() {
                    // Long press: fast-scroll backward.
                    let new_off = fast_scroll(
                        false, &mut display, &renderer, &chapter_text,
                        page_offset, orientation, font_sz_idx,
                        &mut || boot_btn.is_low(),
                    );
                    while boot_btn.is_low() { delay.delay_millis(10); }
                    if new_off != page_offset {
                        prev_page_offset = page_offset;
                        page_offset = new_off;
                        flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                        last_interaction = Instant::now();
                        redraw = true;
                    }
                } else {
                    // Short press: go back one page or to previous chapter.
                    if page_offset != prev_page_offset {
                        page_offset = prev_page_offset;
                        flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                        last_interaction = Instant::now();
                        redraw = true;
                    } else if chapter_idx > 0 {
                        chapter_idx -= 1;
                        chapter_text = archive.chapter_text(&spine[chapter_idx])
                            .expect("epub: chapter load");
                        while chapter_text.trim().len() < MIN_CHAPTER_CHARS && chapter_idx > 0 {
                            chapter_idx -= 1;
                            chapter_text = archive.chapter_text(&spine[chapter_idx]).expect("epub: chapter load");
                        }
                        page_offset = 0;
                        prev_page_offset = 0;
                        next_page_offset = 0;
                        flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                        last_interaction = Instant::now();
                        redraw = true;
                    }
                }
                delay.delay_millis(50);
            }
        }

        // ── Next button = forward page (or dismiss dropdown) ─────────────────
        if next_btn.is_low() {
            delay.delay_millis(50);

            if open_dropdown.is_some() {
                while next_btn.is_low() { delay.delay_millis(10); }
                delay.delay_millis(50);
                open_dropdown = None;
                next_page_offset = restore_after_dropdown(
                    &mut display, &rtc, &renderer, &chapter_text,
                    chapter_idx, chapter_count, page_offset, orientation, bl_level, font_sz_idx,
                );
            } else {
                let hold_start = Instant::now();
                while next_btn.is_low() && hold_start.elapsed().as_millis() < 1000 {
                    delay.delay_millis(10);
                }
                if next_btn.is_low() {
                    // Long press: fast-scroll forward.
                    let new_off = fast_scroll(
                        true, &mut display, &renderer, &chapter_text,
                        page_offset, orientation, font_sz_idx,
                        &mut || next_btn.is_low(),
                    );
                    while next_btn.is_low() { delay.delay_millis(10); }
                    if new_off != page_offset {
                        prev_page_offset = page_offset;
                        page_offset = new_off;
                        next_page_offset = new_off;
                        flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                        last_interaction = Instant::now();
                        redraw = true;
                    }
                } else {
                    // Short press: advance one page or to next chapter.
                    if next_page_offset < chapter_text.len() {
                        prev_page_offset = page_offset;
                        page_offset = next_page_offset;
                        flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                        last_interaction = Instant::now();
                        redraw = true;
                    } else if chapter_idx + 1 < chapter_count {
                        chapter_idx += 1;
                        chapter_text = archive.chapter_text(&spine[chapter_idx])
                            .expect("epub: chapter load");
                        while chapter_text.trim().len() < MIN_CHAPTER_CHARS && chapter_idx + 1 < chapter_count {
                            chapter_idx += 1;
                            chapter_text = archive.chapter_text(&spine[chapter_idx]).expect("epub: chapter load");
                        }
                        page_offset = 0;
                        prev_page_offset = 0;
                        next_page_offset = 0;
                        flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                        last_interaction = Instant::now();
                        redraw = true;
                    }
                }
                delay.delay_millis(50);
            }
        }

        // ── Touch: open/close dropdown panels ────────────────────────────────
        if let Some((tx, ty)) = display.read_touch(&mut gt911) {
            last_interaction = Instant::now();

            let (lx, ly) = phys_to_logical(tx as i32, ty as i32, orientation);
            let cw = if orientation.is_portrait() { 540i32 } else { 960i32 };
            let z  = cw / 5;

            if let Some(kind) = open_dropdown {
                let (drop_x, drop_w) = dropdown_x_and_w(kind, z, cw);
                let n_items = match kind {
                    Dropdown::Backlight => BL_LABEL.len() as i32,
                    Dropdown::FontSize  => FONT_SIZES.len() as i32,
                    Dropdown::Rotation  => ROT_LABELS.len() as i32,
                    Dropdown::Battery   => 0,
                };
                let in_panel = n_items > 0
                    && lx >= drop_x && lx < drop_x + drop_w
                    && ly >= HEADER_H && ly < HEADER_H + n_items * ITEM_H;

                if in_panel {
                    let idx = ((ly - HEADER_H) / ITEM_H) as usize;
                    match kind {
                        Dropdown::Backlight => {
                            bl_level = idx;
                            bl_ch.set_duty(BL_DUTY[bl_level]).unwrap();
                            println!("backlight: {}", BL_LABEL[bl_level]);
                        }
                        Dropdown::FontSize => {
                            font_sz_idx = idx;
                            println!("font size: {}", FONT_LABELS[font_sz_idx]);
                        }
                        Dropdown::Rotation => {
                            orientation = Orientation::from_u32(idx as u32);
                            println!("orientation: {}", orientation.label());
                        }
                        Dropdown::Battery => {}
                    }
                    flash_save_position(page_offset, chapter_idx, font_sz_idx, orientation, bl_level);
                }
                open_dropdown = None;
                next_page_offset = restore_after_dropdown(
                    &mut display, &rtc, &renderer, &chapter_text,
                    chapter_idx, chapter_count, page_offset, orientation, bl_level, font_sz_idx,
                );

            } else if ly < HEADER_H {
                let new_kind = match lx / z {
                    1 => Some(Dropdown::Battery),
                    2 => Some(Dropdown::Backlight),
                    3 => Some(Dropdown::FontSize),
                    4 => Some(Dropdown::Rotation),
                    _ => None,
                };
                if let Some(kind) = new_kind {
                    open_dropdown = Some(kind);
                    let (drop_x, drop_w) = dropdown_x_and_w(kind, z, cw);

                    // Clear the full screen to white, re-render the page behind the dropdown,
                    // then draw the dropdown on top — same pattern as restore_after_dropdown.
                    // This guarantees no bleed-through in any orientation without needing to
                    // compute orientation-specific physical rects for a partial clear.
                    display.fill(0xF).unwrap();
                    display.flush(DrawMode::WhiteOnBlack).unwrap();
                    render_page(&mut display, &rtc, &renderer, &chapter_text,
                                chapter_idx, chapter_count, page_offset,
                                orientation, bl_level, font_sz_idx, "");

                    if kind == Dropdown::Battery {
                        let soc    = read_soc(&mut display);
                        let chrg   = is_charging(&mut display);
                        let volt   = display.i2c_read_u16(BQ27220_ADDR, 0x08);
                        let curr   = display.i2c_read_i16(BQ27220_ADDR, 0x0C);
                        let remain = display.i2c_read_u16(BQ27220_ADDR, 0x10);
                        let full   = display.i2c_read_u16(BQ27220_ADDR, 0x12);
                        let mut rot = RotatedDisplay { inner: &mut display, orientation };
                        draw_battery_panel(&mut rot, drop_x, soc, chrg, volt, curr, remain, full);
                    } else {
                        let mut rot = RotatedDisplay { inner: &mut display, orientation };
                        match kind {
                            Dropdown::Backlight => {
                                draw_option_dropdown(&mut rot, drop_x, drop_w, &BL_LABEL, bl_level);
                            }
                            Dropdown::FontSize => {
                                draw_option_dropdown(&mut rot, drop_x, drop_w, &FONT_LABELS, font_sz_idx);
                            }
                            Dropdown::Rotation => {
                                draw_option_dropdown(&mut rot, drop_x, drop_w, &ROT_LABELS, orientation.as_u32() as usize);
                            }
                            Dropdown::Battery => unreachable!(),
                        }
                    }
                    display.flush(DrawMode::BlackOnWhite).unwrap();
                }
            }

            loop {
                delay.delay_millis(20);
                if display.read_touch(&mut gt911).is_none() { break; }
            }
        }

        // ── Time display update (every minute) ────────────────────────────────
        if last_time_update.elapsed().as_secs() >= TIME_UPDATE_SECS {
            update_header_only(&mut display, &rtc, bl_level, font_sz_idx, orientation);
            display.flush(DrawMode::BlackOnWhite).unwrap();
            last_time_update = Instant::now();
        }

        // ── Inactivity → deep sleep ───────────────────────────────────────────
        if last_interaction.elapsed().as_secs() >= SLEEP_AFTER_SECS {
            println!("ereader: sleeping (ch={} pos={})", chapter_idx, page_offset);

            update_footer_only(&mut display, "Sleeping... Press BOOT to wake", orientation);
            display.flush(DrawMode::BlackOnWhite).unwrap();
            display.power_off();

            bl_ch.set_duty(0).unwrap();

            rtc_store_write(0, page_offset as u32);
            rtc_store_write(1, prev_page_offset as u32);
            rtc_store_write(5, bl_level as u32 | (orientation.as_u32() << 8) | ((font_sz_idx as u32) << 10));
            rtc_store_write(6, chapter_idx as u32);

            let wakeup_pin = unsafe { esp_hal::gpio::AnyPin::steal(0) };
            let boot_src = Ext0WakeupSource::new(wakeup_pin, WakeupLevel::Low);
            rtc.sleep_deep(&[&boot_src]);
        }

        // ── Full page redraw ──────────────────────────────────────────────────
        if redraw {
            display.clear().unwrap();
            next_page_offset = render_page(
                &mut display, &rtc, &renderer, &chapter_text, chapter_idx, chapter_count,
                page_offset, orientation, bl_level, font_sz_idx, "",
            );
            display.flush(DrawMode::BlackOnWhite).unwrap();
            redraw = false;
        }

        delay.delay_millis(50);
    }
}
