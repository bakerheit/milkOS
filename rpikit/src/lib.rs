//! The pure parts of milkOS's Raspberry Pi support (docs/raspberry-pi.md): everything that can
//! be decided from bytes alone, so it runs under host tests and the kernel's drivers stay thin.
//!
//! * [`dtb`]: the flattened device tree reader (RAM, CPUs and how to start them, the kernel
//!   command line, the root `compatible`).
//! * [`cmdline`]: `milk.<name>=<value>` options on the kernel command line.
//! * [`genet`]: Ethernet descriptor words and autonegotiation results.
//! * [`mailbox`]: VideoCore property-interface messages (framebuffer, clocks, board info).
//! * [`pci`]: config space fields, BAR sizing and placement, bridge windows, a host bridge's `ranges`.
//! * [`sd`]: SD card commands and responses, and the SDHCI register values that go with them.
//! * [`lcd`]: the 3.5-inch SPI display on the GPIO header (ILI9486 / ILI9488 / ST7796 command streams,
//!   pixel packing, dirty rectangles, scaling the desktop down to the panel).
//! * [`touch`]: the XPT2046 touch chip beside it (commands, filter, calibration, pointer events).
//! * [`usb`]: xHCI TRBs, events and contexts, USB descriptors, and the HID boot keyboard and mouse.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod cmdline;
pub mod dtb;
pub mod genet;
pub mod lcd;
pub mod mailbox;
pub mod pci;
pub mod sd;
pub mod touch;
pub mod usb;

#[cfg(test)]
mod lcdsim;
#[cfg(test)]
mod lcdtests;
#[cfg(test)]
mod testpng;
#[cfg(test)]
mod tests;
