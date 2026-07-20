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
| Battery charger | BQ25896 (I2C) — single-cell LiPo charging with USB power-path |
| Fuel gauge | BQ27220 (I2C) — state of charge, voltage, current, runtime estimate |
| Backlight | GPIO11 (BOARD_BL_EN) — PWM-controllable frontlight |

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

## Running Examples

Flash and monitor an example with:

```
cargo run --example <name>
```

| Example | Description |
|---------|-------------|
| `ebook` | 3-page e-book demo; press the BOOT button (GPIO0) to advance pages |
| `graphics_test` | 7-screen graphics test: shapes, typography, grayscale, images, animation, timing |
| `touch_button` | Capacitive touch demo; tap the button to toggle fill, coordinates shown in status bar |
| `backlight` | Frontlight demo; fades the LED frontlight in and out using LEDC PWM on GPIO11 |
| `finger_draw` | Touch drawing demo; paint 16×16 px dots wherever your finger moves; partial-refresh timing printed to serial |

**Example:**
```
cargo run --example touch_button
```

The `--monitor` flag is included automatically via `.cargo/config.toml`, so serial output appears in the terminal after flashing.

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

## Partial Refresh

`flush()` only sends rows that have been touched since the last flush — the driver tracks a per-row dirty bitmap (1 bit per row, 68 bytes total). Any `set_pixel` call marks that row dirty; `flush()` sends exactly those rows through the full 15-frame waveform and skips the rest with a fast CKV clock pulse.

**What you can control**
- Any subset of the 540 rows, in any combination — including non-contiguous rows (e.g., row 10, row 200, and row 500 all in one flush)
- As few as 1 row or as many as all 540

**What you cannot control**
- Columns — a dirty row always sends all 960 pixels across that row; there is no column masking
- Sub-row granularity — touching any pixel in a row marks the entire row dirty

**Performance characteristics**
The 15-frame waveform runs in full regardless of how many rows are updated. Each frame iterates all 540 rows; dirty rows get the full I8080 data transfer (~240 bytes), clean rows get only a CKV pulse (microseconds). Speedup is roughly proportional to the fraction of rows updated, minus a fixed per-frame overhead. In practice, updating ~10% of rows (54 rows) takes roughly 10–15% of the time of a full flush.

## Waveform Engine & DrawMode

### How the waveform engine works

`flush()` drives the panel through 15 sequential frames. Each frame uses a 65 536-entry lookup table (LUT) indexed by a `u16` value encoding 4 consecutive framebuffer pixels (4 × 4bpp = 16 bits). The LUT output is one byte containing four 2-bit waveform codes — one per pixel — that set the source-driver voltage for that pixel during the frame's CKV gate pulse. The gate pulse duration comes from `contrast_cycles[k]`, which increases from ~8–30 µs in early frames up to 300 µs in the final frame.

The LUT starts at a uniform default (all pixels get the same 2-bit code) and is progressively modified across frames to drive pixels of each brightness level for a calibrated number of frame-cycles, then switch them to VCOM (no drive). The bistable nature of the e-paper panel holds each pixel at the voltage it was last actively driven to.

**2-bit waveform codes used in this driver:**

| Bits | Meaning |
|------|---------|
| `01` | Source positive — drives particles toward the black electrode (darkens pixel) |
| `10` | Source negative — drives particles toward the white electrode (lightens pixel) |
| `00` | VCOM — no drive; pixel holds its last electrically-set state |

### DrawMode semantics

Three modes are available, differing in their LUT default and update direction:

| Mode | LUT default | Frame-k direction | Best used when… |
|------|-------------|-------------------|-----------------|
| `BlackOnWhite` | `0x55` (all `01`, drive dark) | k = 15−frame (high→low) | …the area is physically white; drives target-black pixels for all 15 frames, floats target-white pixels from frame 0 onward. |
| `WhiteOnBlack` | `0xAA` (all `10`, drive light) | k = frame (low→high) | …you need to physically clear black pixels to white; drives target-white pixels (brightness 0xF) for all 15 frames (they're never cleared since k only reaches 14), floats target-black pixels from frame 0. |
| `WhiteOnWhite` | `0xAA` (all `10`, drive light) | k = 15−frame (high→low) | …same update order as `BlackOnWhite` but starting from drive-light; mainly used for full-panel resets. |

**Critical constraint:** `BlackOnWhite` reliably renders white pixels as white only when those pixels are *already physically white* on the panel. A pixel that is physically black receives only the frame-0 short pulse before being floated — not enough to move the particles. If pixels may be physically black, always run a `WhiteOnBlack` clear pass first.

### The two-pass pattern for reliable rendering

Any time a region may contain physically-black pixels that need to appear white in the new image (including white text on a black fill, or clearing a filled button), use two passes:

```rust
// Pass 1 — reset region to known-white physical state.
// WhiteOnBlack with all-white framebuffer pixels drives 0xAA (source-negative)
// for the full 15 frames on every pixel, because brightness-0xF pixels are never
// cleared (k only reaches 14). This moves all particles to white regardless of
// their prior state.
Rectangle::new(origin, size)
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(&mut display)?;
display.flush(DrawMode::WhiteOnBlack)?;

// Pass 2 — render actual content onto the clean white canvas.
// BlackOnWhite drives black-target pixels for all 15 frames; white-target pixels
// float from their now-confirmed-white physical state and hold.
draw_your_content(&mut display);
display.flush(DrawMode::BlackOnWhite)?;
```

This costs two flush passes but eliminates the need for `clear_area()`, which runs 32 full hardware frame scans (~4× slower on a small region).

### Latency vs quality tradeoffs

The table below lists the levers available, from lowest-impact to most aggressive:

| Technique | Latency gain | Quality cost |
|-----------|-------------|--------------|
| **Partial refresh** — draw only the rows you need | Large; proportional to row count | None for untouched rows |
| **Skip pass 1** when you know the area is already white | ~50 % off two-pass time | Faded text / ghosting if area was not white |
| **Single `BlackOnWhite` pass only** | ~50 % off two-pass time | White-on-black text will appear faded or missing if previously black |
| **Reduce `DRAW_IMAGE_FRAME_COUNT`** (default 15, `display.rs:304`) | Proportional to frames cut | Reduced contrast, lighter blacks, more inter-frame ghosting |
| **Shorten `CONTRAST_CYCLES_4BPP`** (default sum ≈ 1020, `display.rs:8`) | Proportional to cycle-time reduction | Incomplete particle drive; lighter blacks, grayer whites |
| **Use `CONTRAST_CYCLES_4BPP_WHITE`** for `BlackOnWhite` | ~40 % (sum ≈ 280 vs 1020) | Much weaker black drive; acceptable for text at larger font sizes |
| **Accept ghosting** — skip `clear()` / `clear_area()` for updates | Large for full-screen changes | Ghost of previous image visible in unchanged regions |

**Practical guidance:**

- For a **clock or counter** updating a small region every second: use partial refresh + single `BlackOnWhite` pass. If the digits are always black on a pre-cleared white background, one pass is sufficient and ghosting is minimal.
- For a **toggle button or icon** that flips between states: the two-pass pattern is needed; partial refresh keeps it fast. 60 rows × 2 passes typically completes in under 300 ms.
- For **animations** at the cost of quality: reduce `DRAW_IMAGE_FRAME_COUNT` to 8–10 and cut the last two high-contrast entries from `CONTRAST_CYCLES_4BPP` (the 200 and 300 µs entries account for ~50 % of total frame time).
- For **full-page reflows** (e-reader page turn): a full `display.clear()` followed by a single `BlackOnWhite` flush is the cleanest approach; the clear dominates the time budget so the waveform cost is secondary.

## Key Implementation Notes

- **Pixel bit ordering**: the ED047TC1 reads the parallel bus MSB-first — bits 6–7 of each output byte are the leftmost pixel in a 4-pixel group. The LUT converts 4×4bpp pixels to one byte, then a 2-bit-pair reversal (`display.rs: prepare_dma_buffer`) corrects the ordering. Solid fills are unaffected (0x55/0xAA are palindromes under this transform), which is why `clear()` works correctly without this fix.
- **PSRAM allocator**: must be initialized before `Display::new()`.
- **Waveform**: 15-frame grayscale waveform via a 65536-entry LUT; supports `BlackOnWhite`, `WhiteOnWhite`, and `WhiteOnBlack` draw modes.

## License

The `src/driver/` module is derived from [lilygo-epd47](https://crates.io/crates/lilygo-epd47) (GPL-3.0).
