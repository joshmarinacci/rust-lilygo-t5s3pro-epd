#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Pull},
    main,
};
use esp_println::println;

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Gray4,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::Text,
};

use epaper::driver::{Display, DrawMode};

esp_bootloader_esp_idf::esp_app_desc!();

// Three pages of ebook content, ~65 chars per line with FONT_10X20 (10px/char)
const PAGES: [&[&str]; 3] = [
    &[
        "Part One: The Quiet Display",
        "",
        "Electronic paper had a peculiar patience. Unlike",
        "the frantic refresh of liquid crystal panels, an",
        "e-paper display held its image with no power at",
        "all — content to sit quietly for days, weeks,",
        "months, waiting for the next command.",
        "",
        "The Lilygo T5 E-Paper S3 Pro brought together an",
        "ESP32-S3 microcontroller running at 240 MHz and a",
        "4.7-inch ED047TC1 panel with 960 by 540 pixels.",
        "Sixteen shades of grey were available, rendered",
        "through a fifteen-frame waveform that coaxed ink",
        "particles from black to white and back again,",
        "one row at a time, in silent microsecond pulses.",
        "",
        "Press the BOOT button (GPIO 0) to turn the page.",
    ],
    &[
        "Part Two: Memory Without Power",
        "",
        "The framebuffer lived in PSRAM — 325 kilobytes",
        "of four-bit pixels packed into a 960 by 540",
        "grid. Each nibble held a gray level from zero",
        "(pure black) to fifteen (pure white). The flush",
        "operation drove fifteen waveform frames across",
        "every tainted row, applying contrast cycles of",
        "30, 30, 20, 20, 30, 30, 30, 40, 40, 50, 50,",
        "50, 100, 200, and 300 microseconds in sequence.",
        "",
        "Between frames the panel held its state. Between",
        "pages it held its state. Even when power was cut",
        "entirely the ink particles stayed put, holding",
        "the last image indefinitely — no backlight, no",
        "refresh, no energy required to remember.",
        "",
        "Page 2 of 3  —  press BOOT to continue.",
    ],
    &[
        "Part Three: The Cost of Patience",
        "",
        "Refresh time was the great trade-off. Where an",
        "LCD redraws sixty frames per second without",
        "complaint, the e-paper panel needed several",
        "hundred milliseconds to complete its waveform.",
        "",
        "This was not a bug. It was the price of zero",
        "standby power — of a display readable in bright",
        "sunlight, of an image that persisted without a",
        "continuous supply of electrons. For a reader,",
        "a moment between pages was not a hardship.",
        "",
        "The time you just waited was the refresh time.",
        "Watch the serial monitor to see it measured.",
        "",
        "Page 3 of 3  —  press BOOT to return to page 1.",
    ],
];

const MARGIN_X: i32 = 50;
const MARGIN_Y: i32 = 40;
const LINE_HEIGHT: i32 = 27;

fn draw_page(display: &mut Display, page: usize) {
    let style = MonoTextStyle::new(&FONT_10X20, Gray4::BLACK);

    // Thin border
    Rectangle::new(Point::new(16, 16), Size::new(928, 508))
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
        .draw(display)
        .unwrap();

    // Page indicator dots at the bottom
    let dots_y = 530i32;
    for i in 0..3usize {
        let cx = 460 + (i as i32 - 1) * 24;
        let r = 6u32;
        if i == page {
            Rectangle::new(
                Point::new(cx - r as i32, dots_y - r as i32),
                Size::new(r * 2, r * 2),
            )
            .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
            .draw(display)
            .unwrap();
        } else {
            Rectangle::new(
                Point::new(cx - r as i32, dots_y - r as i32),
                Size::new(r * 2, r * 2),
            )
            .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
            .draw(display)
            .unwrap();
        }
    }

    // Text lines
    for (i, line) in PAGES[page].iter().enumerate() {
        Text::new(
            line,
            Point::new(MARGIN_X, MARGIN_Y + 20 + i as i32 * LINE_HEIGHT),
            style,
        )
        .draw(display)
        .unwrap();

        // Underline separator after the title
        if i == 0 {
            Line::new(
                Point::new(MARGIN_X, MARGIN_Y + 26 + LINE_HEIGHT),
                Point::new(960 - MARGIN_X, MARGIN_Y + 26 + LINE_HEIGHT),
            )
            .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 1))
            .draw(display)
            .unwrap();
        }
    }
}

fn wait_for_button(button: &Input, delay: &Delay) {
    while button.is_high() {}
    delay.delay_millis(50);
    while button.is_low() {}
    delay.delay_millis(50);
}

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

    // GPIO0 is the BOOT button — active-low with internal pull-up
    let button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

    let mut display = Display::new(
        epaper::pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
        peripherals.I2C0,
    )
    .expect("to initialize display");

    let delay = Delay::new();
    delay.delay_millis(100);
    display.power_on();
    delay.delay_millis(10);

    println!("Ebook demo — 3 pages. Press BOOT (GPIO0) to advance.");

    // Hardware white clear once at startup
    display.clear().unwrap();

    let mut page = 0usize;

    loop {
        // Hardware clear before every page: the waveform LUT only drives pixels
        // toward black and leaves "white" pixels with no-drive (0x00). Without a
        // physical clear, previously black pixels stay black regardless of what the
        // framebuffer says. clear() unconditionally drives all rows black then white
        // via push_pixels, returning the panel to a known white state first.
        display.clear().unwrap();

        draw_page(&mut display, page);

        println!("--- page {} ---", page + 1);
        println!("flushing...");
        display.flush(DrawMode::BlackOnWhite).unwrap();
        println!("flush complete. Press BOOT for next page.");

        wait_for_button(&button, &delay);

        page = (page + 1) % PAGES.len();
    }
}
