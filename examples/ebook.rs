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
    draw_target::DrawTarget,
    geometry::OriginDimensions,
    mono_font::{
        ascii::{FONT_10X20, FONT_9X18},
        MonoFont, MonoTextStyle,
    },
    pixelcolor::Gray4,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::Text,
};

use epaper::driver::{Display, DrawMode};

esp_bootloader_esp_idf::esp_app_desc!();

// Forward button on GPIO38 (confirmed via find_button diagnostic).

// ── Orientation ───────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq)]
enum Orientation {
    Deg0,   // landscape, normal
    Deg90,  // portrait,  90° CW content (hold device with left edge up)
    Deg180, // landscape, upside-down
    Deg270, // portrait,  90° CCW content (hold device with right edge up)
}

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
        match self {
            Self::Deg0   => "landscape 0°",
            Self::Deg90  => "portrait 90°",
            Self::Deg180 => "landscape 180°",
            Self::Deg270 => "portrait 270°",
        }
    }
}

// ── Rotated display wrapper ───────────────────────────────────────────────────
//
// Logical canvas size and pixel mapping per orientation (W=960, H=540):
//   Deg0:   size 960×540,  (x,y) → (x,       y      )
//   Deg90:  size 540×960,  (x,y) → (W-1-y,   x      )
//   Deg180: size 960×540,  (x,y) → (W-1-x,   H-1-y  )
//   Deg270: size 540×960,  (x,y) → (y,        H-1-x  )
//
// 'd = borrow lifetime, 'hw = Display's hardware peripheral lifetime.
// Keeping them separate lets the borrow end at the closing brace of the
// block where RotatedDisplay is used, freeing `display` for the flush call.
struct RotatedDisplay<'d, 'hw> {
    inner: &'d mut Display<'hw>,
    orientation: Orientation,
}

impl<'d, 'hw> DrawTarget for RotatedDisplay<'d, 'hw> {
    type Color = Gray4;
    type Error = <Display<'hw> as DrawTarget>::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        const W: i32 = Display::WIDTH as i32;  // 960
        const H: i32 = Display::HEIGHT as i32; // 540
        let orientation = self.orientation;
        self.inner.draw_iter(pixels.into_iter().map(|Pixel(Point { x, y }, c)| {
            let p = match orientation {
                Orientation::Deg0   => Point::new(x,       y      ),
                Orientation::Deg90  => Point::new(W-1-y,   x      ),
                Orientation::Deg180 => Point::new(W-1-x,   H-1-y  ),
                Orientation::Deg270 => Point::new(y,       H-1-x  ),
            };
            Pixel(p, c)
        }))
    }
}

impl<'d, 'hw> OriginDimensions for RotatedDisplay<'d, 'hw> {
    fn size(&self) -> Size {
        if self.orientation.is_portrait() {
            Size::new(Display::HEIGHT as u32, Display::WIDTH as u32) // 540×960
        } else {
            Size::new(Display::WIDTH as u32, Display::HEIGHT as u32) // 960×540
        }
    }
}

// ── Pages ─────────────────────────────────────────────────────────────────────
//
// Lines are kept to ≤52 chars so they fit in both landscape (FONT_10X20) and
// portrait (FONT_9X18, 30 px margin → 480 px usable → 53 chars max).

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
        "BOOT = back   next = forward   hold next = rotate",
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
        "BOOT = back   next = forward   hold next = rotate",
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
        "BOOT = back   next = forward   hold next = rotate",
    ],
];

// ── Drawing ───────────────────────────────────────────────────────────────────

fn draw_page<D>(target: &mut D, page: usize, orientation: Orientation)
where
    D: DrawTarget<Color = Gray4> + OriginDimensions,
    D::Error: core::fmt::Debug,
{
    let w = target.size().width as i32;
    let h = target.size().height as i32;

    let (font, margin_x, line_height): (&MonoFont, i32, i32) = if orientation.is_portrait() {
        (&FONT_9X18, 30, 24)
    } else {
        (&FONT_10X20, 50, 27)
    };

    let style  = MonoTextStyle::new(font, Gray4::BLACK);
    let border = PrimitiveStyle::with_stroke(Gray4::BLACK, 2);
    let rule   = PrimitiveStyle::with_stroke(Gray4::BLACK, 1);

    // Border
    Rectangle::new(Point::new(16, 16), Size::new((w - 32) as u32, (h - 32) as u32))
        .into_styled(border)
        .draw(target).unwrap();

    // Page indicator dots at the bottom
    let dots_y = h - 10;
    for i in 0..PAGES.len() {
        let cx = w / 2 + (i as i32 - 1) * 24;
        let r = 6u32;
        let dot_style = if i == page {
            PrimitiveStyle::with_fill(Gray4::BLACK)
        } else {
            PrimitiveStyle::with_stroke(Gray4::BLACK, 2)
        };
        Rectangle::new(
            Point::new(cx - r as i32, dots_y - r as i32),
            Size::new(r * 2, r * 2),
        )
        .into_styled(dot_style)
        .draw(target).unwrap();
    }

    // Text lines
    for (i, line) in PAGES[page].iter().enumerate() {
        Text::new(
            line,
            Point::new(margin_x, 60 + i as i32 * line_height),
            style,
        )
        .draw(target).unwrap();

        // Underline separator after the title
        if i == 0 {
            Line::new(
                Point::new(margin_x, 66 + line_height),
                Point::new(w - margin_x, 66 + line_height),
            )
            .into_styled(rule)
            .draw(target).unwrap();
        }
    }
}

// ── Input ─────────────────────────────────────────────────────────────────────

enum Action { PrevPage, NextPage, ToggleOrientation }

// Long press threshold for the next button (in ms).
const LONG_PRESS_MS: u32 = 500;

fn wait_for_action(boot: &Input, next: &Input, delay: &Delay) -> Action {
    loop {
        if boot.is_low() {
            delay.delay_millis(50);
            while boot.is_low() {}
            delay.delay_millis(50);
            return Action::PrevPage;
        }
        if next.is_low() {
            delay.delay_millis(50);
            let mut held_ms = 50u32;
            while next.is_low() {
                delay.delay_millis(50);
                held_ms += 50;
            }
            delay.delay_millis(50);
            return if held_ms >= LONG_PRESS_MS {
                Action::ToggleOrientation
            } else {
                Action::NextPage
            };
        }
    }
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

    // GPIO0 = BOOT button (previous page), GPIO38 = next/rotate button
    let boot_btn = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );
    let next_btn = Input::new(
        peripherals.GPIO38,
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

    println!("Ebook demo — BOOT=back, next=forward, hold next=cycle orientation");

    display.clear().unwrap();

    let mut page = 0usize;
    let mut orientation = Orientation::Deg0;

    loop {
        // Hardware clear before every page so previously-black pixels are
        // driven back to white before the new frame is written.
        display.clear().unwrap();

        {
            // RotatedDisplay is dropped at the end of this block,
            // releasing the borrow on `display` before the flush below.
            let mut rot = RotatedDisplay { inner: &mut display, orientation };
            draw_page(&mut rot, page, orientation);
        }

        println!("page {} | {}", page + 1, orientation.label());
        display.flush(DrawMode::BlackOnWhite).unwrap();

        match wait_for_action(&boot_btn, &next_btn, &delay) {
            Action::PrevPage => {
                page = if page == 0 { PAGES.len() - 1 } else { page - 1 };
            }
            Action::NextPage => {
                page = (page + 1) % PAGES.len();
            }
            Action::ToggleOrientation => {
                orientation = orientation.next();
                println!("orientation: {}", orientation.label());
            }
        }
    }
}
