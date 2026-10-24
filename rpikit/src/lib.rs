//! The pure parts of milkOS's Raspberry Pi support (docs/raspberry-pi.md): everything that can
//! be decided from bytes alone, so it runs under host tests and the kernel's drivers stay thin.
//!
//! * [`dtb`]: the flattened device tree reader (RAM, CPUs and how to start them, the kernel
//!   command line, the root `compatible`).
//! * [`cmdline`]: `milk.<name>=<value>` options on the kernel command line.
//! * [`mailbox`]: VideoCore property-interface messages (framebuffer, clocks, board info).
//! * [`sd`]: SD card commands and responses, and the SDHCI register values that go with them.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod cmdline;
pub mod dtb;
pub mod mailbox;
pub mod sd;

#[cfg(test)]
mod tests;
