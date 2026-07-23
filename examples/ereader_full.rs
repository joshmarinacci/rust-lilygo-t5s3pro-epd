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
const LINE_H:        i32 = 24; // line spacing including leading (FONT_10X20 + 4px)

// Landscape (canvas 960×540)
const LAND_MARGIN:   i32   = 40;
const LAND_CHARS:    usize = 88; // floor((960-80)/10)
const LAND_LINES:    usize = 19; // floor((510-48)/24)

// Portrait (canvas 540×960)
const PORT_MARGIN:   i32   = 30;
const PORT_CHARS:    usize = 48; // floor((540-60)/10)
const PORT_LINES:    usize = 36; // floor((930-48)/24)

// ── Orientation ───────────────────────────────────────────────────────────────
#[derive(Copy, Clone, PartialEq)]
enum Orientation { Deg0, Deg90, Deg180, Deg270 }

impl Orientation {
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
fn layout(o: Orientation) -> (i32, i32, usize, usize, i32) {
    // (canvas_w, canvas_h, max_chars, max_lines, margin_x)
    if o.is_portrait() {
        (Display::HEIGHT as i32, Display::WIDTH as i32, PORT_CHARS, PORT_LINES, PORT_MARGIN)
    } else {
        (Display::WIDTH as i32, Display::HEIGHT as i32, LAND_CHARS, LAND_LINES, LAND_MARGIN)
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
fn paginate(start: usize, max_lines: usize, max_chars: usize) -> (Vec<&'static str>, usize) {
    let mut lines = Vec::with_capacity(max_lines);
    let mut pos = start;
    while lines.len() < max_lines && pos < MOBY_DICK.len() {
        let (line, next) = wrap_line(pos, max_chars);
        lines.push(line);
        pos = next;
    }
    (lines, pos)
}

fn wrap_line(pos: usize, max_chars: usize) -> (&'static str, usize) {
    let s = &MOBY_DICK[pos..];
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n == 0 { return ("", pos); }

    let mut last_space: Option<usize> = None;
    let mut char_count = 0usize;
    let mut i = 0usize;

    loop {
        if i >= n {
            return (&s[..i], pos + i);
        }
        let b = bytes[i];
        if b == b'\n' {
            return (s[..i].trim_end(), pos + i + 1);
        }
        if char_count >= max_chars {
            if let Some(sp) = last_space {
                let line = s[..sp].trim_end();
                let mut nxt = sp + 1;
                while nxt < n && bytes[nxt] == b' ' { nxt += 1; }
                return (line, pos + nxt);
            }
            return (&s[..i], pos + i);
        }
        if b == b' ' { last_space = Some(i); }
        i += 1;
        char_count += 1;
    }
}

// ── Draw: header bar (black filled, white text) ───────────────────────────────
fn draw_header<D>(target: &mut D, time: &str, soc: u16, charging: bool, bl: usize, o: Orientation)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let cw = target.size().width as i32;
    let border = PrimitiveStyle::with_stroke(Gray4::BLACK, 2);
    let black  = MonoTextStyle::new(&FONT_9X18, Gray4::BLACK);

    Rectangle::new(Point::zero(), Size::new(cw as u32, HEADER_H as u32))
        .into_styled(border).draw(target).unwrap();

    let z = cw / 4; // zone width
    let ty = HEADER_H - 14; // text baseline (leaves 14px from bottom of bar)

    // Zone 1: time
    Text::new(time, Point::new(8, ty), black).draw(target).unwrap();

    // Zone 2: battery
    let bat = if charging { format!("{soc}%[+]") } else { format!("{soc}%") };
    Text::new(&bat, Point::new(z + 4, ty), black).draw(target).unwrap();

    // Zone 3: backlight (tappable — zone x = [cw/2 .. cw*3/4])
    let bl_s = format!("BL:{}", BL_LABEL[bl]);
    Text::new(&bl_s, Point::new(z * 2 + 4, ty), black).draw(target).unwrap();

    // Zone 4: orientation (tappable — zone x = [cw*3/4 .. cw])
    let rot_s = format!("Rot:{}", o.label());
    Text::new(&rot_s, Point::new(z * 3 + 4, ty), black).draw(target).unwrap();
}

// ── Draw: content text lines ──────────────────────────────────────────────────
fn draw_content<D>(target: &mut D, lines: &[&str], margin_x: i32)
where D: DrawTarget<Color = Gray4> + OriginDimensions, D::Error: core::fmt::Debug
{
    let style = MonoTextStyle::new(&FONT_10X20, Gray4::BLACK);
    for (i, &line) in lines.iter().enumerate() {
        let y = CONTENT_TOP + i as i32 * LINE_H + 16;
        Text::new(line, Point::new(margin_x, y), style).draw(target).unwrap();
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
    page_offset:  usize,
    orientation:  Orientation,
    bl_level:     usize,
    status:       &str,
) -> usize
{
    let time = rtc_time_str(rtc);
    let soc  = read_soc(display);
    let chrg = is_charging(display);
    let (_, _, max_chars, max_lines, margin_x) = layout(orientation);

    let (lines, next_offset) = paginate(page_offset, max_lines, max_chars);

    let page_num   = page_offset / max_chars.max(1) / max_lines.max(1) + 1;
    let total_pages = MOBY_DICK.len() / max_chars.max(1) / max_lines.max(1) + 1;

    let mut rot = RotatedDisplay { inner: display, orientation };
    draw_header(&mut rot, &time, soc, chrg, bl_level, orientation);
    draw_content(&mut rot, &lines, margin_x);
    draw_footer(&mut rot, status, page_num, total_pages);

    next_offset
}

// ── Partial header update (only header rows are tainted; fast flush) ──────────
fn update_header_only(
    display:    &mut Display<'_>,
    rtc:        &Rtc<'_>,
    bl_level:   usize,
    orientation: Orientation,
) {
    let time = rtc_time_str(rtc);
    let soc  = read_soc(display);
    let chrg = is_charging(display);
    let mut rot = RotatedDisplay { inner: display, orientation };
    draw_header(&mut rot, &time, soc, chrg, bl_level, orientation);
}

// ── Partial footer update ─────────────────────────────────────────────────────
fn update_footer_only(display: &mut Display<'_>, msg: &str, orientation: Orientation) {
    let mut rot = RotatedDisplay { inner: display, orientation };
    draw_footer(&mut rot, msg, 0, 0);
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

    let (mut page_offset, mut prev_page_offset, mut bl_level, mut orientation, wake_status) =
        if is_first_boot {
            rtc.set_current_time_us((INITIAL_HH * 3600 + INITIAL_MM * 60) * 1_000_000);
            println!("ereader: first boot");
            (0usize, 0usize, 1usize, Orientation::Deg0, "")
        } else {
            let po    = rtc_store_read(0) as usize;
            let ppo   = rtc_store_read(1) as usize;
            let pack  = rtc_store_read(5);
            let bl    = (pack & 0xFF) as usize;
            let ori   = Orientation::from_u32(pack >> 8);
            let ws    = match wakeup_cause() {
                SleepSource::Ext0 => "Awake! BOOT=prev  next=fwd",
                _                 => "Awake!",
            };
            println!("ereader: woke — po={} bl={}", po, bl);
            (po, ppo, bl.min(3), ori, ws)
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

    // ── Initial render ────────────────────────────────────────────────────────
    display.clear().unwrap();
    let mut next_page_offset = render_page(
        &mut display, &rtc, page_offset, orientation, bl_level, wake_status,
    );
    display.flush(DrawMode::BlackOnWhite).unwrap();

    let mut last_interaction = Instant::now();
    let mut last_time_update = Instant::now();
    let mut redraw = false;

    // ── Main loop ─────────────────────────────────────────────────────────────
    loop {
        // ── BOOT = previous page ──────────────────────────────────────────────
        if boot_btn.is_low() {
            delay.delay_millis(50);
            while boot_btn.is_low() {}
            delay.delay_millis(50);

            if page_offset != prev_page_offset {
                page_offset = prev_page_offset;
                last_interaction = Instant::now();
                redraw = true;
            }
        }

        // ── Next button = forward page ────────────────────────────────────────
        if next_btn.is_low() {
            delay.delay_millis(50);
            while next_btn.is_low() {}
            delay.delay_millis(50);

            if next_page_offset < MOBY_DICK.len() {
                prev_page_offset = page_offset;
                page_offset = next_page_offset;
                last_interaction = Instant::now();
                redraw = true;
            }
        }

        // ── Touch: backlight or orientation ───────────────────────────────────
        if let Some((tx, ty)) = display.read_touch(&mut gt911) {
            last_interaction = Instant::now();

            let (lx, ly) = phys_to_logical(tx as i32, ty as i32, orientation);
            let cw = if orientation.is_portrait() { 540i32 } else { 960i32 };
            let bl_start  = cw / 2;
            let rot_start = cw * 3 / 4;

            if ly < HEADER_H && lx >= bl_start && lx < rot_start {
                // Backlight tap → cycle level
                bl_level = (bl_level + 1) % 4;
                bl_ch.set_duty(BL_DUTY[bl_level]).unwrap();
                println!("backlight: {}", BL_LABEL[bl_level]);
                update_header_only(&mut display, &rtc, bl_level, orientation);
                display.flush(DrawMode::BlackOnWhite).unwrap();
            } else if ly < HEADER_H && lx >= rot_start {
                // Orientation tap → cycle orientation, repaginate
                orientation = orientation.next();
                println!("orientation: {}", orientation.label());
                redraw = true;
            }

            // Wait for finger lift
            loop {
                delay.delay_millis(20);
                if display.read_touch(&mut gt911).is_none() { break; }
            }
        }

        // ── Time display update (every minute) ────────────────────────────────
        if last_time_update.elapsed().as_secs() >= TIME_UPDATE_SECS {
            update_header_only(&mut display, &rtc, bl_level, orientation);
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
            rtc_store_write(5, bl_level as u32 | (orientation.as_u32() << 8));

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
                &mut display, &rtc, page_offset, orientation, bl_level, "",
            );
            display.flush(DrawMode::BlackOnWhite).unwrap();
            redraw = false;
        }

        delay.delay_millis(50);
    }
}
