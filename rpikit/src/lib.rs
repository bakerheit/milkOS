//! The pure parts of milkOS's Raspberry Pi support (docs/raspberry-pi.md): everything that can
//! be decided from bytes alone, so it runs under host tests and the kernel's drivers stay thin.
//!
//! * [`dtb`]: the flattened device tree reader (RAM, CPUs and how to start them, the kernel
//!   command line, the root `compatible`).
//! * [`cmdline`]: `milk.<name>=<value>` options on the kernel command line.
//! * [`genet`]: Ethernet descriptor words and autonegotiation results.
//! * [`ledcode`]: the ACT LED boot progress codes (pulses per milestone, the start marker).
//! * [`mailbox`]: VideoCore property-interface messages (framebuffer, clocks, board info).
//! * [`pci`]: config space fields, BAR sizing and placement, bridge windows, a host bridge's `ranges`.
//! * [`sd`]: SD card commands and responses, and the SDHCI register values that go with them.
//! * [`usb`]: xHCI TRBs, events and contexts, USB descriptors, and the HID boot keyboard and mouse.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod cmdline;
pub mod dtb;
pub mod genet;
pub mod ledcode;
pub mod mailbox;
pub mod pci;
pub mod sd;
pub mod usb;

#[cfg(test)]
mod tests;
