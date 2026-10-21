//! virtio devices over the virtio-mmio transport.
//!
//! [`init`] walks the 32 virtio-mmio slots of qemu `virt` and hands each device it finds to
//! its driver. Drivers so far: [`input`] (keyboard, tablet),
//! [`blk`] (disks, registered with `fs::blockdev`), [`net`] (eth0 for `crate::net`), [`sound`] (PCM output for `crate::audio`), [`rng`] (entropy for `crate::crypto::rng`).

pub mod blk;
pub mod input;
pub mod mmio;
pub mod net;
pub mod p9;
pub mod queue;
pub mod rng;
pub mod sound;

use crate::kprintln;
use mmio::{Error, Transport, device_id};

fn name(id: u32) -> &'static str {
    match id {
        device_id::NET => "net",
        device_id::P9 => "9p",
        device_id::BLOCK => "block",
        device_id::CONSOLE => "console",
        device_id::RNG => "rng",
        device_id::GPU => "gpu",
        device_id::INPUT => "input",
        device_id::SOUND => "sound",
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
        let (slot, irq, id) = (t.slot(), t.irq(), t.device_id());
        let r = match t.device_id() {
            device_id::INPUT => input::attach(t),
            device_id::BLOCK => blk::attach(t),
            device_id::NET => net::attach(t),
            device_id::SOUND => sound::attach(t),
            device_id::P9 => p9::attach(t),
            device_id::RNG => rng::attach(t),
            _ => Ok(()),
        };
        match r {
            // Devices a user-space driver may take over (docs/drivers.md).
            Ok(()) if id == device_id::RNG && rng::present() => crate::udrv::register(
                slot,
                irq,
                id,
                crate::udrv::Class::Rng,
                crate::udrv::Hooks {
                    detach: rng::detach,
                    attach: rng::attach,
                },
            ),
            Ok(()) if id == device_id::SOUND && sound::present() => crate::udrv::register(
                slot,
                irq,
                id,
                crate::udrv::Class::Sound,
                crate::udrv::Hooks {
                    detach: sound::detach,
                    attach: sound::attach,
                },
            ),
            Ok(()) => {}
            Err(e) => kprintln!("[virtio] slot {slot}: driver failed: {e:?}"),
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
