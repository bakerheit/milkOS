//! virtio devices over the virtio-mmio transport.
//!
//! [`init`] walks the 32 virtio-mmio slots of qemu `virt` and hands each device it finds to
//! its driver. Drivers so far: [`input`] (keyboard, tablet),
//! [`blk`] (disks, registered with `fs::blockdev`), [`net`] (eth0 for `crate::net`).

pub mod blk;
pub mod input;
pub mod mmio;
pub mod net;
pub mod queue;

use crate::kprintln;
use mmio::{Error, Transport, device_id};

fn name(id: u32) -> &'static str {
    match id {
        device_id::NET => "net",
        device_id::BLOCK => "block",
        device_id::CONSOLE => "console",
        device_id::RNG => "rng",
        device_id::GPU => "gpu",
        device_id::INPUT => "input",
        _ => "unknown",
    }
}

/// Probe every slot and start the drivers we have. The GIC must be initialised; IRQs may
/// still be masked (drivers only register and enable their interrupt here).
pub fn init() {
    let mut found = 0;
    let mut legacy = 0;
    for slot in 0..mmio::MMIO_SLOTS {
        let t = match Transport::probe(slot) {
            Ok(t) => t,
            Err(Error::Legacy) => {
                legacy += 1;
                continue;
            }
            Err(_) => continue,
        };
        found += 1;
        kprintln!(
            "[virtio] slot {:2} @ {:#x}: {} (id {}, vendor {:#x}), INTID {}",
            slot,
            t.base(),
            name(t.device_id()),
            t.device_id(),
            t.vendor_id(),
            t.irq()
        );
        let r = match t.device_id() {
            device_id::INPUT => input::attach(t),
            device_id::BLOCK => blk::attach(t),
            device_id::NET => net::attach(t),
            _ => Ok(()),
        };
        if let Err(e) = r {
            kprintln!("[virtio] slot {slot}: driver failed: {e:?}");
        }
    }
    if legacy > 0 {
        kprintln!(
            "[virtio] {legacy} legacy (v1) device(s) ignored: run qemu with \
             -global virtio-mmio.force-legacy=false"
        );
    }
    if found == 0 && legacy == 0 {
        kprintln!("[virtio] no devices");
    }
}
