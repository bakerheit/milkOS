//! USB: an xHCI host controller driver with the HID boot protocol on top (keyboards and mice),
//! polled, behind PCI(e). On qemu virt the controller is `-device qemu-xhci` on the generic PCI
//! host bridge; on the Raspberry Pi 4 it is the VL805 behind the BCM2711's PCIe root complex
//! (`rpi/pcie.rs`). Everything above the registers is the same code, so it is tested on qemu
//! (`scripts/usb-test.sh`: keys and pointer moves typed through qemu's `usb-kbd` / `usb-mouse`;
//! `scripts/usb-hub-test.sh`: the same behind a `usb-hub`, nested hubs, hot plug).
//!
//! * [`xhci`]: the controller (rings, slots, enumeration of root ports), and hubs (`xhci/hub.rs`: the Pi 4's
//!   sockets are behind one inside the VL805).
//! * [`hid`]: boot keyboards and mice feeding `crate::input`.
//!
//! Not yet: interrupts (a thread polls the event ring every few milliseconds), SuperSpeed hubs, mass storage,
//! report-protocol devices (only what a BIOS would drive), LEDs.
//! Everything a device sends is checked (descriptor lengths, report sizes); every wait on the
//! controller is bounded.

pub mod hid;
pub mod xhci;

use alloc::vec::Vec;

use crate::kprintln;
use crate::pci;

/// Find the host controllers and start the polling thread. Called once, after the scheduler is up.
pub fn start() {
    #[cfg(feature = "raspi")]
    if crate::rpi::disabled("usb") {
        return;
    }
    // The phone's USB is a DWC3 behind the SoC's own glue, not an xHCI on a PCIe host bridge: nothing here
    // applies, and nothing may probe addresses the SoC owns.
    if crate::platform::PHONE {
        kprintln!("[usb] none yet on the Pixel 2");
        return;
    }
    crate::sched::spawn("usb", run);
}

fn run() {
    let Some((devices, host)) = find_controllers() else {
        return;
    };
    let mut ctrls: Vec<xhci::Controller> = Vec::new();
    for dev in &devices {
        let Some(bar) = dev.bars.first().filter(|b| b.size != 0) else {
            continue;
        };
        let Some(va) = pci::mmio(host.cpu_addr(bar.pci_addr), bar.size as usize) else {
            kprintln!("[usb] cannot map the controller's registers");
            continue;
        };
        let seen = crate::serror::absorbed();
        let started = xhci::Controller::new(va, bar.size as usize);
        // A rejected access during start-up is taken here and survived (checkpoint labels starting `pcie-`).
        crate::serror::checkpoint("pcie-xhci-start");
        if crate::serror::absorbed() != seen {
            kprintln!("[usb] the controller rejected an access while starting (an SError, survived): USB is off");
            // The controller may still be doing DMA into its rings: leave them allocated.
            core::mem::forget(started);
            core::mem::forget(ctrls);
            return;
        }
        match started {
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
        let seen = crate::serror::absorbed();
        for c in ctrls.iter_mut() {
            c.poll();
        }
        crate::serror::checkpoint("pcie-xhci-poll");
        if crate::serror::absorbed() != seen {
            kprintln!("[usb] a controller rejected an access (an SError, survived): USB is off");
            core::mem::forget(ctrls);
            return;
        }
        crate::sched::sleep_ms(4);
    }
}

/// The board's host bridge and its xHCI controllers.
fn find_controllers() -> Option<(Vec<pci::Device>, pci::Translate)> {
    let (devs, translate) = controllers()?;
    if devs.is_empty() {
        if crate::platform::RASPI {
            kprintln!("[usb] no xHCI controller found behind the PCIe root complex");
        }
        return None;
    }
    Some((devs, translate))
}

/// The Pi: the root complex is brought up, the VL805 told about and enabled in one go (`rpi/pcie.rs`).
#[cfg(feature = "raspi")]
fn controllers() -> Option<(Vec<pci::Device>, pci::Translate)> {
    crate::rpi::pcie::bring_up()
}

#[cfg(not(feature = "raspi"))]
fn controllers() -> Option<(Vec<pci::Device>, pci::Translate)> {
    let host = pci::virt_host()?;
    let devs: Vec<pci::Device> = pci::enumerate(&host)
        .into_iter()
        .filter(|d| d.is_xhci())
        .collect();
    Some((devs, host.translate()))
}
