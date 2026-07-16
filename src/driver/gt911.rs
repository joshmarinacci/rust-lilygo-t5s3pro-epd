use embedded_hal::i2c::I2c;

pub const GT911_ADDR_PRIMARY: u8 = 0x5D;
pub const GT911_ADDR_ALT: u8 = 0x14;

const REG_STATUS: [u8; 2] = [0x81, 0x4E];
const REG_TOUCH0: [u8; 2] = [0x81, 0x50];

/// Minimal GT911 capacitive touch controller driver (polling, no INT pin).
pub struct Gt911 {
    pub addr: u8,
}

impl Gt911 {
    pub fn new(addr: u8) -> Self {
        Self { addr }
    }

    /// Returns the (x, y) coordinates of the first active touch point, or None.
    ///
    /// Clears the GT911 buffer flag after reading so repeated polls don't
    /// return the same touch event.
    pub fn read_touch<I: I2c>(&mut self, i2c: &mut I) -> Option<(u16, u16)> {
        let mut status = [0u8; 1];
        i2c.write_read(self.addr, &REG_STATUS, &mut status).ok()?;

        let count = status[0] & 0x0F;
        // Bit 7 (buffer-ready flag) must also be set for valid data.
        if count == 0 || status[0] & 0x80 == 0 {
            return None;
        }

        let mut pt = [0u8; 8];
        i2c.write_read(self.addr, &REG_TOUCH0, &mut pt).ok()?;

        // Clear the buffer-ready flag so the next poll sees fresh data.
        let _ = i2c.write(self.addr, &[0x81, 0x4E, 0x00]);

        let x = u16::from_le_bytes([pt[1], pt[2]]);
        let y = u16::from_le_bytes([pt[3], pt[4]]);
        Some((x, y))
    }

    /// Probe both known GT911 addresses and return the one that responds.
    /// Returns None if neither address acknowledges.
    pub fn detect<I: I2c>(i2c: &mut I) -> Option<u8> {
        for &addr in &[GT911_ADDR_PRIMARY, GT911_ADDR_ALT] {
            let mut buf = [0u8; 1];
            if i2c.write_read(addr, &REG_STATUS, &mut buf).is_ok() {
                return Some(addr);
            }
        }
        None
    }
}
