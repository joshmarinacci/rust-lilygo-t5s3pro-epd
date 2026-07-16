#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::{delay::Delay, main, time::Instant};
use esp_println::println;

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Gray4,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};

use epaper::driver::{Display, DrawMode, Gt911};
use epaper::driver::display::Rectangle as EpdRect;
use epaper::driver::gt911::GT911_ADDR_PRIMARY;

esp_bootloader_esp_idf::esp_app_desc!();

// ── Layout ────────────────────────────────────────────────────────────────────
// Status bar: rows 0-59  (shows last touch coordinates, FONT_10X20)
// Button:     rows 70-509 (large target for reliable hit-testing)

const STATUS_Y: i32 = 45;       // text baseline (FONT_10X20 is 20px tall, top at y=25)
const STATUS_H: u32 = 60;       // height of status area to clear/flush

const BTN_X: i32 = 80;
const BTN_Y: i32 = 70;
const BTN_W: u32 = 800;
const BTN_H: u32 = 440;
const BTN_X2: i32 = BTN_X + BTN_W as i32;  // 880
const BTN_Y2: i32 = BTN_Y + BTN_H as i32;  // 510

// A small number-formatting buffer for no_std
struct Buf<const N: usize>([u8; N], usize);
impl<const N: usize> Buf<N> {
    fn new() -> Self { Self([0u8; N], 0) }
    fn push_str(&mut self, s: &str) { for b in s.bytes() { if self.1 < N { self.0[self.1] = b; self.1 += 1; } } }
    fn push_u16(&mut self, mut n: u16) {
        if n == 0 { self.push_str("0"); return; }
        let mut tmp = [0u8; 5]; let mut i = 5usize;
        while n > 0 && i > 0 { i -= 1; tmp[i] = b'0' + (n % 10) as u8; n /= 10; }
        self.push_str(core::str::from_utf8(&tmp[i..]).unwrap_or("?"));
    }
    fn as_str(&self) -> &str { core::str::from_utf8(&self.0[..self.1]).unwrap_or("") }
}

// ── Drawing helpers ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum ButtonState { Empty, Filled }

fn draw_button(display: &mut Display, state: ButtonState) {
    let (fill, stroke, text_color) = match state {
        ButtonState::Empty  => (None,              Some(Gray4::BLACK), Gray4::BLACK),
        ButtonState::Filled => (Some(Gray4::BLACK), None,             Gray4::WHITE),
    };

    let mut style = PrimitiveStyle::default();
    if let Some(c) = fill   { style = PrimitiveStyle::with_fill(c); }
    if let Some(c) = stroke { style = PrimitiveStyle::with_stroke(c, 3); }

    Rectangle::new(Point::new(BTN_X, BTN_Y), Size::new(BTN_W, BTN_H))
        .into_styled(style)
        .draw(display)
        .unwrap();

    Text::with_alignment(
        "TAP ME",
        Point::new(480, BTN_Y + BTN_H as i32 / 2 + 7),
        MonoTextStyle::new(&FONT_10X20, text_color),
        Alignment::Center,
    )
    .draw(display)
    .unwrap();
}

/// Redraw the status bar with the current touch coordinates.
fn update_status(display: &mut Display, tx: u16, ty: u16, tap_count: u32, flush_ms: u64) {
    let style = MonoTextStyle::new(&FONT_10X20, Gray4::BLACK);

    // Physically drive the status cells to white using AC voltage cycles.
    // A framebuffer-only white fill isn't enough: the waveform LUT uses only the
    // target framebuffer value and has no knowledge of the previous display state,
    // so old black pixels won't be driven to white without explicit voltage pulses.
    display.clear_area(EpdRect { x: 0, y: 0, width: 960, height: STATUS_H as u16 }).unwrap();

    // Also write white into the framebuffer so flush() re-drives all status rows.
    Rectangle::new(Point::new(0, 0), Size::new(960, STATUS_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .unwrap();

    let mut buf = Buf::<64>::new();
    buf.push_str("touch (");
    buf.push_u16(tx);
    buf.push_str(", ");
    buf.push_u16(ty);
    buf.push_str(")   tap #");
    // push tap count (u32)
    let mut n = tap_count; let mut tmp = [0u8;10]; let mut i = 10usize;
    if n == 0 { tmp[9] = b'0'; i = 9; } else { while n > 0 && i > 0 { i -= 1; tmp[i] = b'0' + (n % 10) as u8; n /= 10; } }
    buf.push_str(core::str::from_utf8(&tmp[i..]).unwrap_or("?"));
    buf.push_str("   flush ");
    // push flush_ms (u64)
    let mut m = flush_ms; let mut tm = [0u8;20]; let mut mi = 20usize;
    if m == 0 { tm[19] = b'0'; mi = 19; } else { while m > 0 && mi > 0 { mi -= 1; tm[mi] = b'0' + (m % 10) as u8; m /= 10; } }
    buf.push_str(core::str::from_utf8(&tm[mi..]).unwrap_or("?"));
    buf.push_str("ms");

    Text::new(buf.as_str(), Point::new(10, STATUS_Y), style)
        .draw(display)
        .unwrap();
}

fn within_button(x: u16, y: u16) -> bool {
    let xi = x as i32;
    let yi = y as i32;
    xi >= BTN_X && xi < BTN_X2 && yi >= BTN_Y && yi < BTN_Y2
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default()
        .with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    let psram_config = esp_hal::psram::PsramConfig {
        mode: esp_hal::psram::PsramMode::OctalSpi,
        ..Default::default()
    };
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);

    let delay = Delay::new();

    let mut display = Display::new(
        epaper::pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
        peripherals.I2C0,
    )
    .expect("display init");

    delay.delay_millis(100);
    display.power_on();
    delay.delay_millis(10);

    // ── Detect GT911 ─────────────────────────────────────────────────────────
    let touch_addr = display.detect_touch_addr().unwrap_or_else(|| {
        println!("WARNING: GT911 not detected — defaulting to 0x{:02X}", GT911_ADDR_PRIMARY);
        GT911_ADDR_PRIMARY
    });
    println!("GT911 at I2C 0x{:02X}", touch_addr);
    let mut gt911 = Gt911::new(touch_addr);

    // Read product ID — should be "911\0" (0x39 0x31 0x31 0x00) for a real GT911
    let pid = display.touch_product_id(&mut gt911);
    println!("GT911 product ID: {:?} (\"{}{}{}\")",
        pid, pid[0] as char, pid[1] as char, pid[2] as char);

    // Write valid configuration block so the GT911 starts scanning.
    // version=0x00 means it was never programmed; without valid config the chip idles.
    display.configure_touch(&mut gt911, 960, 540);
    delay.delay_millis(200); // allow GT911 to reload config and start scanning
    // Verify config readback.
    let cfg = display.touch_read_config(&mut gt911);
    let x_res = u16::from_le_bytes([cfg[1], cfg[2]]);
    let y_res = u16::from_le_bytes([cfg[3], cfg[4]]);
    println!("GT911 config readback: x_res={} y_res={} max_touch={} int_mode=0x{:02X}",
        x_res, y_res, cfg[5], cfg[6]);
    // Clear stale buffer flag and set coordinate-output mode.
    display.init_touch(&mut gt911);

    println!("GT911 ready");

    // ── Initial render ────────────────────────────────────────────────────────
    display.clear().unwrap();

    // Status bar header (static, shown before first tap)
    Text::new(
        "Tap the button. Coordinates appear here.",
        Point::new(10, STATUS_Y),
        MonoTextStyle::new(&FONT_10X20, Gray4::BLACK),
    )
    .draw(&mut display)
    .unwrap();

    draw_button(&mut display, ButtonState::Empty);
    display.flush(DrawMode::BlackOnWhite).unwrap();

    println!("Ready. Button: x={}..{} y={}..{}", BTN_X, BTN_X2, BTN_Y, BTN_Y2);

    // ── Main loop ─────────────────────────────────────────────────────────────
    let mut state = ButtonState::Empty;
    let mut tap_count: u32 = 0;
    let mut last_flush_ms: u64 = 0;

    loop {
        if let Some((tx, ty)) = display.read_touch(&mut gt911) {
            println!("touch ({}, {})", tx, ty);

            let in_btn = within_button(tx, ty);

            if in_btn {
                state = match state {
                    ButtonState::Empty  => ButtonState::Filled,
                    ButtonState::Filled => ButtonState::Empty,
                };
                tap_count += 1;

                // When returning to Empty, the interior is physically black from the
                // previous Filled flush.  The BlackOnWhite waveform is darken-only
                // (drive-black then VCOM); it cannot drive black pixels back to white.
                // Explicit AC cycles are required, same as the status-bar fix.
                if state == ButtonState::Empty {
                    display.clear_area(EpdRect {
                        x: BTN_X as u16,
                        y: BTN_Y as u16,
                        width: BTN_W as u16,
                        height: BTN_H as u16,
                    }).unwrap();
                }

                // Draw button in new state (taints button rows)
                draw_button(&mut display, state);
            }

            // Always update status bar (taints rows 0-49)
            update_status(&mut display, tx, ty, tap_count, last_flush_ms);

            // Flush both areas in one pass
            let t0 = Instant::now();
            display.flush(DrawMode::BlackOnWhite).unwrap();
            last_flush_ms = t0.elapsed().as_millis();

            println!("flush {}ms  in_btn={}", last_flush_ms, in_btn);

            // Debounce: wait for lift
            loop {
                delay.delay_millis(20);
                if display.read_touch(&mut gt911).is_none() { break; }
            }
        }

        delay.delay_millis(20);
    }
}
