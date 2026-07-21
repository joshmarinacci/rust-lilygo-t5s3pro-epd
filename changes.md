## 2026-07-21 (3)

**Added: `examples/clock.rs` — deep-sleep e-paper clock**
- On every boot: reads `rtc.current_time_us()` (survives deep sleep via RTC STORE2/STORE3), formats as `HH:MM:SS`, draws on display, then calls `rtc.sleep_deep()` for 10 seconds.
- On first boot (non-`CoreDeepSleep` reset reason): seeds the RTC with `INITIAL_HH/MM/SS` constants the user sets before flashing.
- Uses `Rtc::new(peripherals.LPWR)` + `TimerWakeupSource` from `esp_hal::rtc_cntl` — no new crate dependencies.
- Display is powered off after each flush; e-paper retains the image with no power.
- Set `SLEEP_SECS = 3` for faster iteration during testing.

## 2026-07-21 (2)

**Updated: `examples/ebook.rs` — two-button navigation + four-way orientation**
- BOOT (GPIO0) goes to the previous page; GPIO38 advances to the next page (confirmed on hardware).
- Both directions wrap around (page 0 back → last page; last page forward → page 0).
- Hold GPIO38 (≥500 ms) to cycle through all four orientations: 0°, 90°, 180°, 270°.
- `RotatedDisplay<'d, 'hw>` wrapper implements `DrawTarget` + `OriginDimensions` with per-orientation pixel mapping; two separate lifetimes keep the borrow scoped correctly.
- `draw_page` is generic over `DrawTarget + OriginDimensions`; selects font/margins by orientation (FONT_10X20 landscape, FONT_9X18 portrait).
- Serial monitor prints current orientation label on each change.

**Added: `examples/find_button.rs` — GPIO diagnostic**
- Polls candidate free GPIOs (excluding USB D-/D+ on GPIO19/20 and display pins) and prints which one goes low when pressed.
- Used to identify the forward button as GPIO38. Keep for future hardware debugging.

## 2026-07-21

**Fix: `examples/graphics_test.rs` — panic in triangle drawing**
- `embedded-graphics` 0.8.2 overflows `i32` in `ClosedThickSegmentIter` / `IntersectionParams::nearly_colinear_has_error` (`denominator.pow(2)`) for the triangle's large screen coordinates.
- Workaround: replaced `Triangle::new(...).into_styled(s4)` with three separate `Line` primitives (same visual result, avoids the thick-segment join code path).
- Removed unused `Triangle` import.

## 2026-07-20 (3)

**Added: `examples/battery_status.rs` — BQ27220 + BQ25896 dashboard**
- Reads all useful registers from both chips via the display's shared I2C bus and renders a two-column dashboard on the e-paper screen, refreshing every 10 s.
- Left column (BQ27220 fuel gauge): state of charge %, voltage, current + direction, remaining/full capacity, state of health, temperature.
- Right column (BQ25896 charger): USB presence, charge status, VBUS voltage, battery voltage, system voltage, charge current.
- All readings also logged to serial each cycle.

**Added: `i2c_read_u8 / i2c_read_u16 / i2c_read_i16` to `Display`**
- Generic I2C passthrough helpers on the shared bus, used by `battery_status` to reach the battery chips without opening a second I2C port.

## 2026-07-20 (2)

**Fixed: `examples/backlight.rs` — wrong GPIO**
- Was using GPIO47 (BOARD_LORA_BUSY) instead of GPIO11 (BOARD_BL_EN).
- Confirmed from official board definition in Lilygo's T5S3-4.7-e-paper-PRO repo.
- Updated README hardware table to document backlight pin, BQ25896 charger, and BQ27220 fuel gauge.

## 2026-07-20

**Added: `examples/finger_draw.rs` — touch finger-drawing demo**
- Draws a 16×16 px filled black dot at each touch position using partial refresh.
- No erasing — pixels accumulate, letting you judge the display's maximum refresh cadence.
- Each dot flush only updates the 16 dirty rows, so partial refresh completes in a fraction of a full-screen update time.
- Timing (`flush=Xms`) and dot count printed to serial for each stroke.
- Drawing area is the full screen below a thin header; header stays physically on screen without being re-sent.

**Added: `examples/backlight.rs` — frontlight PWM demo**
- New example that drives the Lilygo T5 S3 Pro frontlight (GPIO47) using the ESP32-S3 LEDC peripheral with 1 kHz 8-bit PWM.
- Draws a static label on the e-paper display, then enters a loop fading the backlight from 0% → 100% over ~2 s, holds for 1 s, then fades back to 0%.
- Prints brightness percentage to serial each step.
- Updated README examples table with the new entry.

## 2026-07-19

Rendering fixes, touch_button improvements, and waveform documentation.

**Fixed: `src/driver/display.rs` — partial refresh column bleeding**
- `draw()` was calling `self.epd.skip()` directly for non-tainted rows, bypassing `row_skip()`. This left the source drivers holding the last active row's pixel data during skip CKV pulses, causing the column range of any drawn region to bleed black top-to-bottom across the full display height over 15 waveform frames.
- Fix: use `self.row_skip()` / `self.row_write()` throughout `draw()` and reset `self.skipping = 0` at the start of each frame, so the first skip after active rows sends a blank buffer that clears the source drivers.

**Updated: `examples/touch_button.rs`**
- Shrunk button from 800×440 to 200×60 px centered on screen to measure raw partial-refresh speed.
- Removed status bar (text, constants, `update_status` helper, `Buf` struct) so only the button region is flushed.
- Replaced `clear_area()` + `BlackOnWhite` with a universal two-pass waveform approach:
  - Pass 1: `WhiteOnBlack` with all-white framebuffer drives every pixel in the button area to white for all 15 frames, establishing a known-white physical state.
  - Pass 2: `BlackOnWhite` renders the actual button content (fill or stroke+text) on that clean canvas.
  - Eliminates `clear_area()` (32 full hardware scans) for both state transitions; both filled and empty states now render correctly in both passes.

**Updated: `README.md`**
- Replaced the partial-refresh "LUT only drives toward black" note (incorrect) with a full "Waveform Engine & DrawMode" section covering: how the 15-frame LUT engine works, 2-bit waveform code meanings, DrawMode semantics, the two-pass pattern, and a latency vs quality tradeoff table.

## 2026-07-17 (gt911 byte layout fix)

Fixed GT911 touch coordinate byte offsets, inverted Y axis, and removed wrong scaling.

**Modified: `src/driver/gt911.rs`**
- Fixed `read_touch`: actual layout is Y at [0,1], X at [2,3], touch area at [4,5] (was reading X from [1,2], Y from [3,4])
- Y is physically inverted: raw y=y_max is the physical top of the screen; corrected with `y = y_max - y_raw`
- Removed incorrect 16-bit scaling (`x_raw * x_max / 65535`); the GT911 outputs coordinates directly in the configured range (0..x_max, 0..y_max) after `configure()` is called
- Removed y_raw_min/y_raw_max calibration fields and `set_y_raw_range()` — no longer needed

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

## 2026-07-16 (touch_button — GT911 Y axis calibration)

Fixed GT911 Y coordinate spanning only ~42 pixels instead of the full 540.

**Root cause**: The GT911 on this hardware outputs Y raw values in a hardware-specific sub-range (~1946–8240) rather than the full 0–65535 span used by the X axis. Dividing by `u16::MAX` (65535) produced only ~42 pixels of effective Y travel even across the full screen height.

**Fix**: Added `y_raw_min` and `y_raw_max` fields to `Gt911` (defaults 1946/8240, calibrated from observed tap data). `read_touch()` now clamps to this range and inverts in one step: `y = (y_raw_max - y_raw) * y_max / (y_raw_max - y_raw_min)`. Added `set_y_raw_range(min, max)` for future hardware-specific overrides.

**Derivation**: Observed y_raw≈7424 at button top (y≈70) and y_raw≈2307 at button bottom (y≈509). Extrapolated to screen edges: top y=0 → y_raw≈8240, bottom y=540 → y_raw≈1946.

**Files changed**:
- `src/driver/gt911.rs` — `Gt911` struct gains `y_raw_min`/`y_raw_max`; `new()` defaults to measured values; `read_touch()` uses calibrated Y range; added `set_y_raw_range()`

## 2026-07-16 (touch_button — GT911 coordinate scaling)

Fixed GT911 touch coordinates reporting raw 16-bit sensor values instead of display pixel coordinates.

**Root cause**: The GT911 outputs raw sensor coordinates in a 0–65535 range regardless of the `X_output_max`/`Y_output_max` config registers. The `read_touch()` function was returning the raw values directly.

**Fix**: Added `x_max`/`y_max` fields to `Gt911` struct (set by `configure()`). `read_touch()` now scales raw coordinates to display pixel space: `pixel = raw * max / 65535`.

**Files changed**:
- `src/driver/gt911.rs` — `Gt911` struct gains `x_max: u16, y_max: u16`; `configure()` saves them; `read_touch()` scales output when max fields are set

## 2026-07-16 (touch_button — button background clearing on toggle)

Fixed button not clearing when tapping a second time to return to the outline (Empty) state.

**Root cause**: The `BlackOnWhite` waveform is darken-only. `lut_default = 0x55` drives all pixels toward black; `update_lut` progressively changes entries for lighter target pixels from `01` (black-drive) to `00` (VCOM/neutral). White-target pixels get VCOM for all 15 frames — so previously-black pixels are left black, since VCOM produces no net drive on the panel.

**Fix**: Added `display.clear_area()` on the button bounds before `draw_button(Empty)`, same as the existing status-bar fix. This uses AC voltage cycles to physically drive the button interior back to white before the waveform renders the new outline.

**Files changed**:
- `examples/touch_button.rs` — added `clear_area()` call in the `ButtonState::Empty` arm of the tap handler

## 2026-07-16 (touch_button — status bar background clearing)

Fixed status bar text background not being cleared between touch events.

**Root cause**: The display waveform LUT uses only the target framebuffer value as its index. After each `flush()`, the framebuffer is reset to `0xFF` (white). When `update_status()` filled rows 0-59 with white via embedded-graphics, the framebuffer values were unchanged (already `0xFF`), so the waveform had no information about the previous display state (e.g. old black text pixels). The LUT cannot drive previously-black pixels to white without knowing they were black.

**Fix**: Added a `display.clear_area()` call at the start of `update_status()`. This uses AC voltage cycles (darken + lighten) to physically drive the status bar cells to white before the framebuffer-based `flush()` renders the new text. Kept the embedded-graphics white rectangle fill so that `flush()` taints and re-drives all 60 status rows consistently.

**Files changed**:
- `examples/touch_button.rs` — added `use epaper::driver::display::Rectangle as EpdRect`, added `display.clear_area(EpdRect { x: 0, y: 0, width: 960, height: STATUS_H as u16 })` at start of `update_status()`

## 2026-07-16 (touch debugging — GT911 config)

Debugged GT911 touch controller — IC communicates but digitizer not detected.

**Root cause found**: The GT911 config block had `version=0x00` (never programmed). With invalid/uninitialized config, the GT911 enters an "awaiting host configuration" state and does NOT start the scan engine (status register stays 0x00 indefinitely). Writing a valid 184-byte config block with correct checksum and 0x01 to the config-fresh register (0x8100) triggers the scan engine.

**Fix applied**: Added `Gt911::configure(i2c, x_max, y_max)` that writes a valid config with INT mode 1 (falling edge), touch threshold 0x01, 5-point max touch, and 5ms scan rate. Config readback confirms correct write: x_res=960, y_res=540, max_touch=5, int_mode=0x01.

**Outstanding hardware issue**: Even with the GT911 scanning (brief 0x80 burst observed at ~1.2s after config reload), the status register never shows count>0 during physical tapping. Tapping was confirmed during a 10s pure-poll diagnostic loop and a 2-minute main loop. This is consistent with the touch digitizer FPC cable not being connected to the GT911 module, or a board variant with GT911 populated but no digitizer attached. Hardware inspection of a second FPC connector on the board is needed.

**New files/methods added**:
- `Gt911::configure(i2c, x_max, y_max)` — writes full 184-byte config block
- `Gt911::read_config(i2c)` — reads 7 config bytes for diagnostics  
- `Gt911::read_status_raw(i2c)` — reads status without clearing (diagnostics)
- `Gt911::clear_status(i2c)` — write 0x00 to clear buffer-ready flag
- `Display::configure_touch(gt911, x_max, y_max)` — routes config write
- `Display::touch_read_config(gt911)` — routes config read
- `Display::touch_read_status_raw(gt911)` — routes raw status read
- `Display::touch_clear_status(gt911)` — routes status clear
- `Display::i2c_scan()` — scans all I2C addresses (diagnostic helper)

I2C scan reveals devices at: 0x20 (PCA9555), 0x51 (RTC), 0x55 (unknown), 0x68 (TPS65185), 0x6B (unknown). GT911 at 0x5D responds to write_read but not naked read (expected behavior).
