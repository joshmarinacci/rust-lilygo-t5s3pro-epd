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
