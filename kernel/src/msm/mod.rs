//! The Google Pixel 2 (Qualcomm MSM8998 / Snapdragon 835), stage 1: a boot image that
//! `fastboot boot` can run and that draws on the screen (docs/pixel2.md). Built only with feature
//! `msm8998`.
//!
//! * [`stage1`]: the whole experiment, in the order a person looking at the phone sees it: colour
//!   blocks, then the boot log as a text console, then a counter that keeps going.
//! * [`fb`]: the bootloader's framebuffer (its "splash" region of RAM): finding it, drawing the
//!   pattern into it in whatever pixel order, sending the console's rows there, and the crash screen.
//! * [`mem`]: the RAM the device tree lists minus the regions it reserves.
//! * [`gicv3`]: what the interrupt controller's identification registers say (read only in stage 1).
//!
//! The pure parts (options, pixel formats, the pattern, the RAM arithmetic, the GIC register
//! decoding) are in `rpikit::msm`, with host tests.

pub mod fb;
pub mod gicv3;
pub mod mem;
pub mod stage1;
pub mod wdt;
