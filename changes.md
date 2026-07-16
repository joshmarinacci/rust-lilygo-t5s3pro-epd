## 2026-07-15 (touch_button)

Added GT911 touch controller support and `examples/touch_button.rs`.

**New file: `src/driver/gt911.rs`**
- Minimal GT911 capacitive touch driver (polling, no INT pin required)
- `Gt911::new(addr)` — construct with I2C address (0x5D primary, 0x14 alternate)
- `Gt911::read_touch(i2c)` — reads status register 0x814E, returns first touch point coordinates from 0x8150, clears buffer-ready flag after each read
- `Gt911::detect(i2c)` — probes both addresses and returns the one that ACKs

**Modified: `src/driver/ed047tc1.rs`**
- Added `i2c()` method exposing `&mut I2c<'_, Blocking>` so the Display layer can pass the bus to touch reads

**Modified: `src/driver/display.rs`**
- Added `read_touch(&mut self, gt911: &mut Gt911) -> Option<(u16, u16)>` — polls GT911 via the driver's internal I2C
- Added `detect_touch_addr(&mut self) -> Option<u8>` — finds the active GT911 address at startup

**Modified: `src/driver/mod.rs`**
- Added `pub mod gt911` and re-exported `Gt911`

**New file: `examples/touch_button.rs`**
- Detects GT911 address on boot; warns if not found
- Draws a 360×160 px button centered on screen (rows 190–350)
- Toggle between outline-only and filled-black on each tap
- Uses partial refresh (only button rows flushed) for low-latency redraws
- Prints `touch at (x, y)` and `flush Nms` per tap to serial monitor
- Debounces: waits for finger-lift before accepting next tap

Flash and run: `cargo run --example touch_button`

## 2026-07-15 (graphics_test)

Added `examples/graphics_test.rs` — comprehensive 7-screen graphics test.

**New file: `examples/graphics_test.rs`**
- Screen 0: Title page listing all screens, navigation hint
- Screen 1: Shapes — 8 radiating lines, 5 concentric circles (filled + stroked), 4 stroke-width rectangles, triangle, grey-level line swatch
- Screen 2: Typography — all 9 built-in fonts (`FONT_4X6` through `FONT_10X20`), underline via `underline_with_color`, strikethrough via `strikethrough_with_color`, left/centre/right alignment demo
- Screen 3: Grayscale — 16 labelled bars (luma 0→15), 960×50 smooth gradient strip via `ImageRaw<Gray4, BigEndian>` embedded from `OUT_DIR/strip.bin`
- Screen 4: Image — 960×270 four-quadrant test card (gradient / checkerboard / solid bands / Chebyshev rings) via `ImageRaw` from `OUT_DIR/card.bin`
- Screen 5: Animation — 20-frame ball animation in a 120-row partial-refresh band; measures full-flush time (540 rows via `fill()`) and per-frame partial-flush time
- Screen 6: Timing summary — `clear_ms`, `full_flush_ms`, `partial_avg_ms` with computed speedup ratio

**Bug fix: `src/driver/display.rs`** — tainted-row dirty bitmap (`set_pixel` and `is_tainted`) divided by `TAINTED_ROWS_SIZE` (68) instead of 8, causing row-index collisions and preventing true partial refresh. Fixed to divide by 8; `1 << (row % 8)` correctly indexes the bit within each byte.

**Updated: `build.rs`** — generates two synthetic image assets at compile time for the graphics_test example:
- `OUT_DIR/card.bin` — 960×270 four-quadrant test card (129,600 bytes), 4-bit BigEndian Gray4
- `OUT_DIR/strip.bin` — 960×50 horizontal gradient (24,000 bytes), 4-bit BigEndian Gray4

Flash and run: `cargo run --example graphics_test`

## 2026-07-15 (ebook)

Added 3-page ebook demo as an example binary; `src/main.rs` is unchanged.

**New files**
- `src/lib.rs` — minimal library root (`pub mod driver`) so examples can reference the driver
- `examples/ebook.rs` — ebook page-turn demo

**Changes to `src/driver/mod.rs`**
- Re-exported `ed047tc1::PinConfig` as `driver::PinConfig` so the `pin_config!` macro works from outside the crate
- Updated macro body to use `$crate::driver::PinConfig` (was `$crate::driver::ed047tc1::PinConfig`)

**Ebook demo details**
- Three pages of text using `FONT_10X20`, ~65 chars per line, ~17 lines per page
- Chapter title + underline separator, body text, page-indicator dots (filled = current page)
- Page navigation via GPIO0 (BOOT button, active-low, pull-up with `InputConfig`): press to advance, wraps back to page 1
- `display.clear()` before every page: the waveform LUT only drives pixels toward black and leaves "white" pixels with no-drive (`0x00`), so previously-black pixels from the prior page would ghost unless the panel is unconditionally reset to white first via `push_pixels`
- Serial monitor logs `flushing...` / `flush complete` around each `flush()` call for timing observation

Flash and run: `cargo run --example ebook`

## 2026-07-14 18:50

Fixed pixel ordering in `prepare_dma_buffer` (`src/driver/display.rs`):

- The ED047TC1 panel reads the parallel bus MSB-first: bits 6–7 of each byte are the leftmost pixel in a 4-pixel group, not bits 0–1
- The LUT produced LSB-first output, causing every 4-pixel group to render right-to-left (blurry circle edges, garbled text)
- Fix: reverse the 2-bit pixel-pair order within each output byte after LUT conversion
- Uniform solid fills (0x55 / 0xAA / 0x00 / 0xFF) are palindromes under this transform, which is why `display.clear()` always worked correctly
- Verified on hardware: sharp shape edges and readable text

## 2026-07-14 18:30

Added embedded-graphics demo to `src/main.rs`:

- Added `embedded-graphics = "0.8"` dependency to `Cargo.toml`
- Draws a 6px border, filled circle, stroked rectangle, stroked triangle, and two centred text lines using `FONT_10X20`
- Uses `Gray4::BLACK` for all primitives on a white background (`display.clear()`)
- Flushes to hardware via `display.flush(DrawMode::BlackOnWhite)`
- Verified on device: serial output shows "drawing shapes... flushing... done." with no panics

## 2026-07-14

Replaced `lilygo-epd47` crate with a local `src/driver/` module forked for the T5 E-Paper S3 Pro hardware (V7 / ESP32-S3):

- **Correct GPIO wiring**: Data bus D0–D7 → GPIO5–8,15–18; CKH→GPIO4; STH→GPIO41; LEH→GPIO42; STV→GPIO45; CKV→GPIO48
- **I2C power management**: PCA9555 I/O expander (addr 0x20, SDA=GPIO39, SCL=GPIO40) for OE/MODE/PWRUP/VCOM_CTRL/WAKEUP signals; TPS65185 PMIC (addr 0x68) for voltage rail enable and VCOM=1600mV
- **Pro-specific `pin_config!` macro** wired to the correct GPIOs
- **`Display::new()` takes `peripherals.I2C0`** as an additional parameter
- Verified: display fills solid black end-to-end on hardware

## 2026-07-13

Initial project scaffold for Lilygo T5 E-Paper S3 Pro embedded Rust driver.

- Created `Cargo.toml` with `lilygo-epd47 1.1.0` as the primary display driver (ED047TC1 parallel e-paper via ESP32-S3 LCD_CAM + DMA + RMT)
- Created `.cargo/config.toml` targeting `xtensa-esp32s3-none-elf` with the `esp` toolchain
- Created `rust-toolchain.toml` pinning to the Espressif `esp` Xtensa toolchain
- Created `build.rs` linking `linkall.x` (standard ESP32 pattern)
- Created `src/main.rs` that initializes PSRAM, powers on the display, and performs a hardware clear to white
- Build compiles cleanly with `cargo build`
