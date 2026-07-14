# epaper

Embedded Rust driver for the **Lilygo T5 E-Paper S3 Pro** — an ESP32-S3 board with a 4.7" ED047TC1 e-paper display (960×540, 16 grayscale levels).

## Hardware

| Component | Detail |
|-----------|--------|
| MCU | ESP32-S3 (Xtensa LX7, 240 MHz) |
| Display | ED047TC1, 960×540, 4-bit grayscale |
| Interface | Parallel I8080 via ESP32-S3 LCD_CAM + DMA + RMT |
| PMIC | TPS65185 (I2C 0x68) — controls display voltage rails and VCOM |
| I/O expander | PCA9555 (I2C 0x20, SDA=GPIO39, SCL=GPIO40) — controls OE/MODE/PWRUP/VCOM_CTRL/WAKEUP |
| PSRAM | 8 MB OctalSPI — required for the 325 KB framebuffer |

## Prerequisites

1. Install the Espressif Xtensa toolchain:
   ```
   cargo install espup
   espup install
   source ~/export-esp.sh
   ```
2. Install `espflash`:
   ```
   cargo install espflash
   ```

## Build and Flash

Connect the board via USB, then:

```
cargo run
```

This builds in dev mode and flashes via `espflash flash --monitor --chip esp32s3`.

To build release:

```
cargo build --release
espflash flash --chip esp32s3 target/xtensa-esp32s3-none-elf/release/epaper
```

## Project Structure

```
src/
  main.rs              — demo: draws shapes and text with embedded-graphics
  driver/
    mod.rs             — public re-exports and pin_config! macro
    ed047tc1.rs        — low-level panel driver (I8080, RMT, I2C power management)
    rmt.rs             — RMT pulse helper for CKV row clock (GPIO48)
    display.rs         — framebuffer, waveform engine, flush/clear logic
    graphics.rs        — embedded-graphics DrawTarget<Color=Gray4> impl
```

## API

```rust
let mut display = Display::new(
    pin_config!(peripherals),
    peripherals.DMA_CH0,
    peripherals.LCD_CAM,
    peripherals.RMT,
    peripherals.I2C0,
)?;

display.power_on();
display.clear()?;                          // hardware white clear cycle

// draw with embedded-graphics into the framebuffer …

display.flush(DrawMode::BlackOnWhite)?;    // push framebuffer to panel
display.power_off();
```

Colors are `Gray4` from `embedded-graphics`. `Gray4::BLACK` (luma 0x0) = black;
`Gray4::WHITE` (luma 0xF) = white. The framebuffer starts white after each `flush`.

## Key Implementation Notes

- **Pixel bit ordering**: the ED047TC1 reads the parallel bus MSB-first — bits 6–7 of each output byte are the leftmost pixel in a 4-pixel group. The LUT converts 4×4bpp pixels to one byte, then a 2-bit-pair reversal (`display.rs: prepare_dma_buffer`) corrects the ordering. Solid fills are unaffected (0x55/0xAA are palindromes under this transform), which is why `clear()` works correctly without this fix.
- **PSRAM allocator**: must be initialized before `Display::new()`.
- **Waveform**: 15-frame grayscale waveform via a 65536-entry LUT; supports `BlackOnWhite`, `WhiteOnWhite`, and `WhiteOnBlack` draw modes.

## License

The `src/driver/` module is derived from [lilygo-epd47](https://crates.io/crates/lilygo-epd47) (GPL-3.0).
