//! USB: an xHCI host controller driver with the HID boot protocol on top (keyboards and mice),
//! polled, behind PCI(e). On qemu virt the controller is `-device qemu-xhci` on the generic PCI
//! host bridge; on the Raspberry Pi 4 it is the VL805 behind the BCM2711's PCIe root complex
//! (`rpi/pcie.rs`). Everything above the registers is the same code, so it is tested on qemu
//! (`scripts/usb-test.sh`: keys and pointer moves typed through qemu's `usb-kbd` / `usb-mouse`).
//!
//! * [`xhci`]: the controller (rings, slots, enumeration of root ports).
//! * [`hid`]: boot keyboards and mice feeding `crate::input`.
//!
//! Not yet: hubs (a keyboard behind a hub), interrupts (a thread polls the event ring every few
//! milliseconds), mass storage, report-protocol devices (only what a BIOS would drive), LEDs.
//! Everything a device sends is checked (descriptor lengths, report sizes); every wait on the
//! controller is bounded.

pub mod hid;
pub mod xhci;

use alloc::vec::Vec;

use crate::kprintln;
use crate::pci;

/// Find the host controllers and start the polling thread. Called once, after the scheduler is up.
pub fn start() {
    crate::sched::spawn("usb", run);
}

fn run() {
    let Some((devices, host)) = find_controllers() else {
        return;
    };
    let mut ctrls: Vec<xhci::Controller> = Vec::new();
    for dev in &devices {
        #[cfg(feature = "raspi")]
        if rpikit::pci::vendor(dev.id) == 0x1106 {
            // The VIA VL805: the firmware may have to load its code first.
            crate::rpi::pcie::notify_vl805(dev.bdf.bus, dev.bdf.dev, dev.bdf.func);
        }
        let Some(bar) = dev.bars.first().filter(|b| b.size != 0) else {
            continue;
        };
        let Some(va) = pci::mmio(host.cpu_addr(bar.pci_addr), bar.size as usize) else {
            kprintln!("[usb] cannot map the controller's registers");
            continue;
        };
        match xhci::Controller::new(va, bar.size as usize) {
            Some(c) => ctrls.push(c),
            None => kprintln!(
                "[usb] the controller at {:02x}:{:02x}.{} did not start",
                dev.bdf.bus,
                dev.bdf.dev,
                dev.bdf.func
            ),
        }
    }
    if ctrls.is_empty() {
        return;
    }
    // The polling loop: events, new and gone devices, key repeat. A few ms is below what a person notices.
    loop {
        for c in ctrls.iter_mut() {
            c.poll();
        }
        crate::sched::sleep_ms(4);
    }
}

/// The board's host bridge and its xHCI controllers.
fn find_controllers() -> Option<(Vec<pci::Device>, pci::Translate)> {
    let host = host_bridge()?;
    let devs: Vec<pci::Device> = pci::enumerate(&host)
        .into_iter()
        .filter(|d| d.is_xhci())
        .collect();
    if devs.is_empty() {
        if crate::platform::RASPI {
            kprintln!("[usb] no xHCI controller found behind the PCIe root complex");
        }
        return None;
    }
    Some((devs, host.translate()))
}

#[cfg(feature = "raspi")]
fn host_bridge() -> Option<pci::Host> {
    crate::rpi::pcie::host()
}

#[cfg(not(feature = "raspi"))]
fn host_bridge() -> Option<pci::Host> {
    pci::virt_host()
}
