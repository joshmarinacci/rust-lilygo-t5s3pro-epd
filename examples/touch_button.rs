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
use epaper::driver::gt911::GT911_ADDR_PRIMARY;

esp_bootloader_esp_idf::esp_app_desc!();

// ── Button geometry ───────────────────────────────────────────────────────────

const BTN_W: u32 = 360;
const BTN_H: u32 = 160;
const BTN_X: i32 = (960 - BTN_W as i32) / 2;  // 300
const BTN_Y: i32 = (540 - BTN_H as i32) / 2;  // 190
const BTN_X2: i32 = BTN_X + BTN_W as i32;      // 660
const BTN_Y2: i32 = BTN_Y + BTN_H as i32;      // 350

#[derive(Clone, Copy, PartialEq)]
enum ButtonState { Empty, Filled }

fn draw_button(display: &mut Display, state: ButtonState) {
    let label_style = MonoTextStyle::new(&FONT_10X20, match state {
        ButtonState::Empty  => Gray4::BLACK,
        ButtonState::Filled => Gray4::WHITE,
    });

    match state {
        ButtonState::Empty => {
            Rectangle::new(Point::new(BTN_X, BTN_Y), Size::new(BTN_W, BTN_H))
                .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 3))
                .draw(display)
                .unwrap();
        }
        ButtonState::Filled => {
            Rectangle::new(Point::new(BTN_X, BTN_Y), Size::new(BTN_W, BTN_H))
                .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
                .draw(display)
                .unwrap();
        }
    }

    Text::with_alignment(
        "TAP ME",
        Point::new(480, BTN_Y + BTN_H as i32 / 2 + 7),
        label_style,
        Alignment::Center,
    )
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

    let mut display = Display::new(
        epaper::pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
        peripherals.I2C0,
    )
    .expect("display init");

    let delay = Delay::new();
    delay.delay_millis(100);
    display.power_on();
    delay.delay_millis(10);

    // ── Detect GT911 address ──────────────────────────────────────────────────
    // Probe both known addresses (0x5D primary, 0x14 alternate) and use whichever ACKs.
    let touch_addr = display.detect_touch_addr().unwrap_or_else(|| {
        println!("WARNING: GT911 not detected on I2C — defaulting to 0x{:02X}", GT911_ADDR_PRIMARY);
        GT911_ADDR_PRIMARY
    });
    println!("GT911 found at I2C 0x{:02X}", touch_addr);
    let mut gt911 = Gt911::new(touch_addr);

    // ── Initial screen ────────────────────────────────────────────────────────
    display.clear().unwrap();

    // Header
    let heading = MonoTextStyle::new(&FONT_10X20, Gray4::BLACK);
    Text::with_alignment(
        "Touch Button Latency Test",
        Point::new(480, 50),
        heading,
        Alignment::Center,
    )
    .draw(&mut display)
    .unwrap();

    let small = MonoTextStyle::new(
        &embedded_graphics::mono_font::ascii::FONT_7X13,
        Gray4::BLACK,
    );
    Text::with_alignment(
        "Tap the button to toggle filled / empty. Redraw latency is logged to serial.",
        Point::new(480, 80),
        small,
        Alignment::Center,
    )
    .draw(&mut display)
    .unwrap();

    draw_button(&mut display, ButtonState::Empty);
    display.flush(DrawMode::BlackOnWhite).unwrap();

    println!("Ready. Tap the button.");

    // ── Main loop ─────────────────────────────────────────────────────────────
    let mut state = ButtonState::Empty;
    let mut tap_count: u32 = 0;

    loop {
        if let Some((tx, ty)) = display.read_touch(&mut gt911) {
            println!("touch at ({}, {})", tx, ty);

            if within_button(tx, ty) {
                // Toggle state
                state = match state {
                    ButtonState::Empty  => ButtonState::Filled,
                    ButtonState::Filled => ButtonState::Empty,
                };
                tap_count += 1;

                // Redraw — framebuffer is white after last flush, so only the
                // button rows (190–350, ~160 rows) will be tainted.
                draw_button(&mut display, state);

                let t0 = Instant::now();
                display.flush(DrawMode::BlackOnWhite).unwrap();
                let ms = t0.elapsed().as_millis();

                println!("tap #{}: {:?} → flush {}ms", tap_count,
                    if state == ButtonState::Filled { "filled" } else { "empty" },
                    ms);

                // Wait for finger to lift before accepting another tap.
                loop {
                    delay.delay_millis(20);
                    if display.read_touch(&mut gt911).is_none() {
                        break;
                    }
                }
            }
        }

        delay.delay_millis(20); // ~50 Hz poll
    }
}
