#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec::Vec};

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
        ascii::{FONT_7X13, FONT_9X18},
        MonoTextStyle,
    },
    pixelcolor::Gray4,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};

use epaper::driver::{Display, DrawMode, Gt911};
use epaper::driver::gt911::GT911_ADDR_PRIMARY;
use epaper::font::TextRenderer;

esp_bootloader_esp_idf::esp_app_desc!();

// ── Book text (embedded in flash at compile time) ─────────────────────────────
const MOBY_DICK: &str = include_str!("moby_dick.txt");

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

// ── Layout constants (physical display is always 960×540) ─────────────────────
const HEADER_H:      i32 = 44;
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
// Base 0x6000_8000; STORE0@+0x50, STORE1@+0x54, STORE5@+0xC4, STORE6@+0xC8
// STORE2/3 used by esp-hal for time. STORE4 used by ROM for boot messages.
fn rtc_store_read(idx: u8) -> u32 {
    let r = esp_hal::peripherals::LPWR::regs();
    match idx {
        0 => r.store0().read().data().bits(),
        1 => r.store1().read().data().bits(),
        5 => r.store5().read().data().bits(),
        _ => 0,
    }
}

fn rtc_store_write(idx: u8, val: u32) {
    let r = esp_hal::peripherals::LPWR::regs();
    match idx {
        0 => { r.store0().write(|w| unsafe { w.data().bits(val) }); }
        1 => { r.store1().write(|w| unsafe { w.data().bits(val) }); }
        5 => { r.store5().write(|w| unsafe { w.data().bits(val) }); }
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
fn rtc_time_str(rtc: &Rtc<'_>) -> alloc::string::String {
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
// Returns (lines, next_byte_offset) — all slices reference into MOBY_DICK.
fn paginate(
    renderer: &TextRenderer,
    start: usize,
    content_h: i32,
    max_px: i32,
    font_px: f32,
) -> (Vec<&'static str>, usize) {
    let line_h = renderer.line_height(font_px) + LEADING;
    let max_lines = (content_h / line_h.max(1)) as usize;
    let mut lines = Vec::with_capacity(max_lines);
    let mut pos = start;
    while lines.len() < max_lines && pos < MOBY_DICK.len() {
        let (line, next) = wrap_line_px(renderer, pos, max_px, font_px);
        lines.push(line);
        pos = next;
    }
    (lines, pos)
}

fn wrap_line_px(
    renderer: &TextRenderer,
    pos: usize,
    max_px: i32,
    font_px: f32,
) -> (&'static str, usize) {
    let s = &MOBY_DICK[pos..];
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
// Five equal zones; zones 3-5 are tappable.
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
    let black  = MonoTextStyle::new(&FONT_9X18, Gray4::BLACK);

    Rectangle::new(Point::zero(), Size::new(cw as u32, HEADER_H as u32))
        .into_styled(border).draw(target).unwrap();

    let z = cw / 5; // zone width
    let ty = HEADER_H - 14; // text baseline

    // Zone 1: time
    Text::new(time, Point::new(8, ty), black).draw(target).unwrap();

    // Zone 2: battery
    let bat = if charging { format!("{soc}%[+]") } else { format!("{soc}%") };
    Text::new(&bat, Point::new(z + 4, ty), black).draw(target).unwrap();

    // Zone 3: backlight (tappable — [2z..3z])
    let bl_s = format!("BL:{}", BL_LABEL[bl]);
    Text::new(&bl_s, Point::new(z * 2 + 4, ty), black).draw(target).unwrap();

    // Zone 4: font size (tappable — [3z..4z])
    let sz_s = format!("Sz:{}", FONT_LABELS[font_sz_idx]);
    Text::new(&sz_s, Point::new(z * 3 + 4, ty), black).draw(target).unwrap();

    // Zone 5: orientation (tappable — [4z..5z])
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
// status: non-empty → shown centred; empty → page number + button hint shown.
fn draw_footer<D>(target: &mut D, status: &str, page: usize, total: usize)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let cw = target.size().width  as i32;
    let ch = target.size().height as i32;
    let fy = ch - FOOTER_H;

    // White background for footer (ensures clean render after partial update)
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
        if page > 0 {
            let s = format!("p.{page}/{total}");
            Text::new(&s, Point::new(8, ty), small).draw(target).unwrap();
        }
        Text::with_alignment(
            "BOOT=prev  next=fwd",
            Point::new(cw - 8, ty), small, Alignment::Right,
        ).draw(target).unwrap();
    }
}

// ── Full page render; returns next_page_offset ────────────────────────────────
fn render_page(
    display:      &mut Display<'_>,
    rtc:          &Rtc<'_>,
    renderer:     &TextRenderer,
    page_offset:  usize,
    orientation:  Orientation,
    bl_level:     usize,
    font_sz_idx:  usize,
    status:       &str,
) -> usize
{
    let time = rtc_time_str(rtc);
    let soc  = read_soc(display);
    let chrg = is_charging(display);
    let (_canvas_w, canvas_h, max_px, font_px, margin_x) = layout(orientation, font_sz_idx);
    let content_h = canvas_h - CONTENT_TOP - FOOTER_H;

    let (lines, next_offset) = paginate(renderer, page_offset, content_h, max_px, font_px);

    let line_h = (renderer.line_height(font_px) + LEADING).max(1);
    let max_lines_est = (content_h / line_h).max(1) as usize;
    let avg_line_chars = (max_px / 11).max(1) as usize;
    let chars_per_page = (avg_line_chars * max_lines_est).max(1);
    let page_num    = page_offset / chars_per_page + 1;
    let total_pages = MOBY_DICK.len() / chars_per_page + 1;

    {
        let mut rot = RotatedDisplay { inner: display, orientation };
        draw_header(&mut rot, &time, soc, chrg, bl_level, font_sz_idx, orientation);
        draw_footer(&mut rot, status, page_num, total_pages);
    }
    draw_content(display, orientation, renderer, &lines, margin_x, font_px);

    next_offset
}

// ── Partial header update (only header rows are tainted; fast flush) ──────────
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
    draw_footer(&mut rot, msg, 0, 0);
}

// ── Two-pass dropdown close: WhiteOnBlack clear then full re-render ───────────
fn restore_after_dropdown(
    display:     &mut Display<'_>,
    rtc:         &Rtc<'_>,
    renderer:    &TextRenderer,
    page_offset: usize,
    orientation: Orientation,
    bl_level:    usize,
    font_sz_idx: usize,
) -> usize {
    display.fill(0xF).unwrap();
    display.flush(DrawMode::WhiteOnBlack).unwrap();
    let next = render_page(display, rtc, renderer, page_offset, orientation, bl_level, font_sz_idx, "");
    display.flush(DrawMode::BlackOnWhite).unwrap();
    next
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

    // GPIO0 must be bound before pin_config! and before Rtc::new().
    // We keep it as a mutable peripheral so we can reborrow it for the Input
    // driver and later pass it via unsafe steal to Ext0WakeupSource at sleep time.
    let mut gpio0 = peripherals.GPIO0;

    let mut rtc = Rtc::new(peripherals.LPWR);

    // ── Boot type and persisted state ─────────────────────────────────────────
    let is_first_boot = reset_reason(Cpu::ProCpu) != Some(SocResetReason::CoreDeepSleep);

    let (mut page_offset, mut prev_page_offset, mut bl_level, mut orientation,
         mut font_sz_idx, wake_status) =
        if is_first_boot {
            rtc.set_current_time_us((INITIAL_HH * 3600 + INITIAL_MM * 60) * 1_000_000);
            println!("ereader: first boot");
            (0usize, 0usize, 1usize, Orientation::Deg0, DEFAULT_FONT_SIZE, "")
        } else {
            let po    = rtc_store_read(0) as usize;
            let ppo   = rtc_store_read(1) as usize;
            let pack  = rtc_store_read(5);
            let bl    = (pack & 0xFF) as usize;
            let ori   = Orientation::from_u32(pack >> 8);
            let sz    = ((pack >> 10) & 0x3) as usize;
            let ws    = match wakeup_cause() {
                SleepSource::Ext0 => "Awake! BOOT=prev  next=fwd",
                _                 => "Awake!",
            };
            println!("ereader: woke — po={} bl={} sz={}", po, bl, sz);
            (po, ppo, bl.min(3), ori, sz.min(FONT_SIZES.len() - 1), ws)
        };

    // ── Buttons ───────────────────────────────────────────────────────────────
    // gpio0 is reborrowed for the Input so the owned gpio0 stays available for
    // Ext0WakeupSource::new (via unsafe AnyPin::steal) at deep-sleep time.
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

    // ── Font renderer (loads Georgia.ttf from flash into PSRAM) ──────────────
    let renderer = TextRenderer::new();

    // ── Initial render ────────────────────────────────────────────────────────
    display.clear().unwrap();
    let mut next_page_offset = render_page(
        &mut display, &rtc, &renderer, page_offset, orientation, bl_level, font_sz_idx, wake_status,
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
            while boot_btn.is_low() {}
            delay.delay_millis(50);

            if open_dropdown.is_some() {
                open_dropdown = None;
                next_page_offset = restore_after_dropdown(
                    &mut display, &rtc, &renderer,
                    page_offset, orientation, bl_level, font_sz_idx,
                );
            } else if page_offset != prev_page_offset {
                page_offset = prev_page_offset;
                last_interaction = Instant::now();
                redraw = true;
            }
        }

        // ── Next button = forward page (or dismiss dropdown) ─────────────────
        if next_btn.is_low() {
            delay.delay_millis(50);
            while next_btn.is_low() {}
            delay.delay_millis(50);

            if open_dropdown.is_some() {
                open_dropdown = None;
                next_page_offset = restore_after_dropdown(
                    &mut display, &rtc, &renderer,
                    page_offset, orientation, bl_level, font_sz_idx,
                );
            } else if next_page_offset < MOBY_DICK.len() {
                prev_page_offset = page_offset;
                page_offset = next_page_offset;
                last_interaction = Instant::now();
                redraw = true;
            }
        }

        // ── Touch: open/close dropdown panels ────────────────────────────────
        if let Some((tx, ty)) = display.read_touch(&mut gt911) {
            last_interaction = Instant::now();

            let (lx, ly) = phys_to_logical(tx as i32, ty as i32, orientation);
            let cw = if orientation.is_portrait() { 540i32 } else { 960i32 };
            let z  = cw / 5;

            if let Some(kind) = open_dropdown {
                // ── Dropdown open: select an item or dismiss ──────────────────
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
                }
                open_dropdown = None;
                next_page_offset = restore_after_dropdown(
                    &mut display, &rtc, &renderer,
                    page_offset, orientation, bl_level, font_sz_idx,
                );

            } else if ly < HEADER_H {
                // ── No dropdown open: open one for the tapped zone ────────────
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
                    if kind == Dropdown::Battery {
                        let soc     = read_soc(&mut display);
                        let chrg    = is_charging(&mut display);
                        let volt    = display.i2c_read_u16(BQ27220_ADDR, 0x08);
                        let curr    = display.i2c_read_i16(BQ27220_ADDR, 0x0C);
                        let remain  = display.i2c_read_u16(BQ27220_ADDR, 0x10);
                        let full    = display.i2c_read_u16(BQ27220_ADDR, 0x12);
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

            // Wait for finger lift
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
            println!("ereader: sleeping");

            update_footer_only(&mut display, "Sleeping... Press BOOT to wake", orientation);
            display.flush(DrawMode::BlackOnWhite).unwrap();
            display.power_off();

            bl_ch.set_duty(0).unwrap();

            rtc_store_write(0, page_offset as u32);
            rtc_store_write(1, prev_page_offset as u32);
            rtc_store_write(5, bl_level as u32 | (orientation.as_u32() << 8) | ((font_sz_idx as u32) << 10));

            // GPIO38 is not RTC-capable on ESP32-S3 and cannot wake from deep
            // sleep. Only GPIO0 (BOOT) is used as the wakeup source.
            let wakeup_pin = unsafe { esp_hal::gpio::AnyPin::steal(0) };
            let boot_src = Ext0WakeupSource::new(wakeup_pin, WakeupLevel::Low);
            rtc.sleep_deep(&[&boot_src]);
        }

        // ── Full page redraw ──────────────────────────────────────────────────
        if redraw {
            display.clear().unwrap();
            next_page_offset = render_page(
                &mut display, &rtc, &renderer, page_offset, orientation, bl_level, font_sz_idx, "",
            );
            display.flush(DrawMode::BlackOnWhite).unwrap();
            redraw = false;
        }

        delay.delay_millis(50);
    }
}
