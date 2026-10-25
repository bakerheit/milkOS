//! The pure parts of milkOS's Raspberry Pi support (docs/raspberry-pi.md): everything that can
//! be decided from bytes alone, so it runs under host tests and the kernel's drivers stay thin.
//!
//! * [`dtb`]: the flattened device tree reader (RAM, CPUs and how to start them, the kernel
//!   command line, the root `compatible`).
//! * [`cmdline`]: `milk.<name>=<value>` options on the kernel command line.
//! * [`esr`]: ESR_EL1 decoded for the exception dump (exception class, abort and SError syndromes).
//! * [`genet`]: Ethernet descriptor words and autonegotiation results.
//! * [`fat`]: finding one pre-created file on a FAT32 boot partition and its sectors (the boot log file).
//! * [`milklog`]: the SD card log file's layout (format 2: a header and a ring of sectors holding many boots), how a
//!   flush builds its sectors, and how the end of the text is found again after a power cut.
//! * [`logintake`]: what goes into the log: line assembly, the rate cap, the uptime stamp.
//! * [`ledcode`]: the ACT LED boot progress codes (pulses per milestone, the start marker).
//! * [`ledcrash`]: the ACT LED crash cycle (boot stage, exception class, PC, fault address as counted pulses).
//! * [`mailbox`]: VideoCore property-interface messages (framebuffer, clocks, board info).
//! * [`pci`]: config space fields, BAR sizing and placement, bridge windows, a host bridge's `ranges`.
//! * [`pcie`]: the root complex bring-up as an ordered step table, the VL805 handshake orders, window register maths.
//! * [`soc`]: the BCM2711's blocks by address (the crash dump names the last device writes).
//! * [`sd`]: SD card commands and responses, and the SDHCI register values that go with them.
//! * [`lcd`]: the 3.5-inch SPI display on the GPIO header (ILI9486 / ILI9488 / ST7796 command streams,
//!   pixel packing, dirty rectangles, scaling the desktop down to the panel).
//! * [`lcdbudget`]: the time limits that keep the display from ever delaying the boot (the early bring-up's budget, the print path's, when `milk.lcd=probe` may start).
//! * [`lcdprobe`]: `milk.lcd=probe`, the card shown on each candidate set-up of a display whose controller is unknown.
//! * [`touch`]: the XPT2046 touch chip beside it (commands, filter, calibration, pointer events).
//! * [`usb`]: xHCI TRBs, events and contexts, USB descriptors, and the HID boot keyboard and mouse.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod cmdline;
pub mod dtb;
pub mod esr;
pub mod fat;
pub mod genet;
pub mod ledcode;
pub mod ledcrash;
pub mod lcd;
pub mod lcdbudget;
pub mod lcdprobe;
pub mod logintake;
pub mod milklog;
pub mod mailbox;
pub mod pci;
pub mod pcie;
pub mod sd;
pub mod soc;
pub mod touch;
pub mod usb;

#[cfg(test)]
mod fattests;
#[cfg(test)]
mod lcdsim;
#[cfg(test)]
mod lcdtests;
#[cfg(test)]
mod logtests;
#[cfg(test)]
mod pcietests;
#[cfg(test)]
mod testpng;
#[cfg(test)]
mod tests;
