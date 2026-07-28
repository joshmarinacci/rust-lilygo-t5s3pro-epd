// On xtensa (ESP32) targets we run no_std; on the host (simulator) we use std.
#![cfg_attr(target_arch = "xtensa", no_std)]

#[cfg(target_arch = "xtensa")]
extern crate alloc;

#[cfg(target_arch = "xtensa")]
pub mod driver;
