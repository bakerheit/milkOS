//! The GICv3, read only. Stage 1 enables no interrupt (the idle loop polls the counter), so this
//! only looks: where the tree says the distributor and the redistributors are, what their
//! identification registers answer, and which redistributor belongs to which CPU. A wrong
//! address or a GIC in an unexpected state then shows in the log of the first run instead of as
//! a hang in the second stage.

use crate::dtb::Dtb;
use crate::kprintln;
use crate::mmu::phys_to_virt;
use core::ptr::read_volatile;
use rpikit::msm::{Redist, decode_gicd_typer, gic_arch_rev, mpidr_affinity, redistributors};

const GICD_CTLR: usize = 0x0000;
const GICD_TYPER: usize = 0x0004;
const GICD_IIDR: usize = 0x0008;
const GICD_PIDR2: usize = 0xffe8;
const GICR_IIDR: usize = 0x0004;
const GICR_TYPER: usize = 0x0008;
const GICR_PIDR2: usize = 0xffe8;

fn rd32(pa: usize) -> u32 {
    // SAFETY: a device register read inside the Device mapping of the first GiB (platform.rs);
    // the address comes from the tree and was checked to be in that range.
    unsafe { read_volatile(phys_to_virt(pa) as *const u32) }
}

fn rd64(pa: usize) -> u64 {
    // SAFETY: as `rd32`; GICR_TYPER is a 64-bit register at an 8-byte aligned offset.
    unsafe { read_volatile(phys_to_virt(pa) as *const u64) }
}

/// Is `[pa, pa+size)` inside the first GiB, which the kernel maps as Device memory?
fn mapped(pa: u64, size: u64) -> bool {
    let (lo, hi) = crate::platform::CURRENT.device.first().copied().unwrap_or((0, 0));
    size != 0 && pa >= lo as u64 && pa.checked_add(size).is_some_and(|e| e <= hi as u64)
}

/// Log what the GIC says. Never writes a register.
pub fn probe(dtb: Option<&Dtb>) {
    let Some(d) = dtb else {
        kprintln!("gic: no device tree, nothing to look at");
        return;
    };
    let mut reg = [(0u64, 0u64); 4];
    let n = d.node_reg_compat(b"arm,gic-v3", &mut reg);
    if n < 2 {
        kprintln!("gic: the tree has no arm,gic-v3 node with a reg ({n} ranges)");
        return;
    }
    let (gicd, gicd_size) = reg[0];
    let (gicr, gicr_size) = reg[1];
    kprintln!("gic: GICD {gicd:#x}+{gicd_size:#x}");
    kprintln!("gic: GICR {gicr:#x}+{gicr_size:#x}");
    if !mapped(gicd, gicd_size) || !mapped(gicr, gicr_size) {
        kprintln!("gic: outside the mapped device range, not touched");
        return;
    }
    let (gicd, gicr) = (gicd as usize, gicr as usize);
    let rev = gic_arch_rev(rd32(gicd + GICD_PIDR2));
    let typer = decode_gicd_typer(rd32(gicd + GICD_TYPER));
    kprintln!(
        "gic: arch rev {rev} (3 = GICv3), {} lines, {} id bits, A3V {}, SecurityExtn {}",
        typer.lines,
        typer.id_bits,
        typer.a3v as u8,
        typer.security_extn as u8
    );
    kprintln!(
        "gic: GICD_CTLR {:#x}, IIDR {:#x}",
        rd32(gicd + GICD_CTLR),
        rd32(gicd + GICD_IIDR)
    );
    let mut frames = [Redist::default(); 16];
    let found = redistributors(gicr_size as usize, |off| rd64(gicr + off + GICR_TYPER), &mut frames);
    kprintln!(
        "gic: {found} redistributors, first PIDR2 rev {}, IIDR {:#x}",
        gic_arch_rev(rd32(gicr + GICR_PIDR2)),
        rd32(gicr + GICR_IIDR)
    );
    let mpidr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack)) };
    let mine = mpidr_affinity(mpidr);
    for (i, f) in frames[..found].iter().enumerate() {
        kprintln!(
            "gic:  [{i}] aff {:#010x} cpu# {}{}{}",
            f.affinity,
            f.processor,
            if f.last { " last" } else { "" },
            if f.affinity == mine { " <- this cpu" } else { "" }
        );
    }
    if !frames[..found].iter().any(|f| f.affinity == mine) {
        kprintln!("gic: no redistributor has this cpu's affinity {mine:#x}");
    }
}
