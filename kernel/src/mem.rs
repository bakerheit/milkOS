#![allow(dead_code)]
//! Memory bring-up: device tree -> MMU -> frame allocator -> heap, plus the
//! boot-time self-test.

use crate::dtb::{Dtb, MAX_BANKS, MemRange};
use crate::mmu::{self, PAGE_SIZE, PhysAddr};
use crate::{heap, kprintln, pmm};
use core::ptr::addr_of;

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static __boot_dtb: u64;
}

/// The board's default RAM (qemu virt: 512 MiB at 1 GiB), used only if the device tree can't be
/// parsed.
const FALLBACK_RAM: MemRange = MemRange {
    base: crate::platform::CURRENT.ram_fallback.0,
    size: crate::platform::CURRENT.ram_fallback.1,
};

/// The RAM banks to use (rpikit's [`usable_banks`](crate::dtb::usable_banks)) and where they came
/// from, for the log. Without a usable bank holding the kernel: the board's default.
pub fn ram_banks(
    tree: &[MemRange],
    kstart: PhysAddr,
) -> ([MemRange; MAX_BANKS], usize, &'static str) {
    let limit = crate::platform::CURRENT.ram_limit;
    let (banks, n) = crate::dtb::usable_banks(tree, kstart, limit, PAGE_SIZE);
    if n > 0 {
        return (banks, n, "device tree");
    }
    let f = FALLBACK_RAM;
    let mut banks = banks;
    banks[0] = MemRange {
        base: f.base.next_multiple_of(PAGE_SIZE),
        size: f.size & !(PAGE_SIZE - 1),
    };
    (banks, 1, "FALLBACK default, no usable device tree")
}

/// Physical range of the kernel image (incl. .bss and the boot stack).
pub fn kernel_range() -> (PhysAddr, PhysAddr) {
    (
        mmu::virt_to_phys(addr_of!(__kernel_start) as usize),
        mmu::virt_to_phys(addr_of!(__kernel_end) as usize),
    )
}

/// Physical address of the device tree blob qemu passed in (0 if none).
pub fn boot_dtb() -> PhysAddr {
    // SAFETY: plain word written once by boot.S.
    unsafe { addr_of!(__boot_dtb).read() as usize }
}

/// Kernel virtual address of the boot device tree (0 if none), for reading it.
pub fn boot_dtb_ptr() -> usize {
    match boot_dtb() {
        0 => 0,
        pa => mmu::phys_to_virt(pa),
    }
}

/// Bring up the MMU, physical allocator and heap. Call once from `kmain`,
/// before anything allocates or takes a lock.
pub fn init() {
    let (kstart, kend) = kernel_range();

    // 1. Where is RAM? Ask the device tree, else assume the board's default.
    // qemu virt only passes a DTB (in x0) when it boots an arm64 Image, i.e. the flat
    // binary scripts/run.sh makes; a plain ELF boot gets x0 = 0 and lands on the
    // 512 MiB default (which is what run.sh passes with -m). The Pi firmware always passes one.
    let dtb_addr = boot_dtb();
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { Dtb::from_addr(boot_dtb_ptr()) };
    let mut tree = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    let ntree = dtb.as_ref().map_or(0, |d| d.memory(&mut tree));
    // The Pixel 2: what the secure world, the modem and the DSPs own is not ours to map, not even
    // to read; the tree's reserved regions come out of the RAM list (msm/mem.rs).
    #[cfg(feature = "msm8998")]
    let ntree = match &dtb {
        Some(d) => crate::msm::mem::carve_tree(d, &mut tree, ntree),
        None => ntree,
    };
    let (banks, nbanks, source) = ram_banks(&tree[..ntree], kstart);
    let primary = banks[..nbanks]
        .iter()
        .find(|b| (b.base..b.base + b.size).contains(&kstart))
        .copied()
        .unwrap_or(MemRange { base: 0, size: 0 });
    // The allocator covers the first bank and everything above it as one range; the gaps
    // between banks (the Pi has one for the GPU's memory) are reserved below.
    let span_end = banks[..nbanks]
        .iter()
        .map(|b| b.base + b.size)
        .max()
        .unwrap_or(0);
    let ram = MemRange {
        base: primary.base,
        size: span_end - primary.base,
    };
    let ram_end = ram.base + ram.size;

    // 2. The bitmap (and the frame tags) go right after the kernel image. If the device tree sits
    // there (a loader that puts it next to the kernel), start the bitmap after it.
    let mut low_end = kend;
    if let Some(d) = &dtb {
        let bitmap_len = pmm::metadata_bytes(ram.size) + PAGE_SIZE;
        let dbase = mmu::virt_to_phys(d.base());
        let dend = dbase + d.total_size();
        if dbase < kend + bitmap_len && dend > kend {
            low_end = dend;
        }
    }

    // 3. The kernel's own page tables (boot.S turned the MMU on with temporary ones), then
    // PMM, then heap.
    // SAFETY: first and only call, core 0, before the PMM.
    unsafe { mmu::init(&banks[..nbanks]) };
    // SAFETY: MMU is on, RAM is mapped, nothing else owns RAM above the kernel image.
    unsafe { pmm::init(ram.base, ram.size, low_end) };
    let mut prev_end = ram.base;
    for b in banks[..nbanks].iter().filter(|b| b.base >= ram.base) {
        if b.base > prev_end {
            pmm::reserve(prev_end, b.base - prev_end);
        }
        prev_end = prev_end.max(b.base + b.size);
    }
    if let Some(d) = &dtb {
        pmm::reserve(mmu::virt_to_phys(d.base()), d.total_size());
    }
    // The bootloader's framebuffer is RAM we map but must not hand out.
    #[cfg(feature = "msm8998")]
    crate::msm::fb::reserve_frames();
    heap::init();

    let s = pmm::stats();
    let h = heap::stats();
    let total: usize = banks[..nbanks].iter().map(|b| b.size).sum();
    kprintln!(
        "mem: RAM {:#x}..{:#x} ({} MiB, {})",
        ram.base,
        ram_end,
        total >> 20,
        source
    );
    if nbanks > 1 {
        for b in &banks[..nbanks] {
            kprintln!(
                "mem:   bank {:#x}..{:#x} ({} MiB)",
                b.base,
                b.base + b.size,
                b.size >> 20
            );
        }
    }
    match &dtb {
        Some(d) => kprintln!(
            "mem: dtb at {:#x}, {} bytes",
            mmu::virt_to_phys(d.base()),
            d.total_size()
        ),
        None => kprintln!("mem: no device tree (x0 = {dtb_addr:#x})"),
    }
    kprintln!(
        "mem: kernel image {:#x}..{:#x} ({} KiB), reserved below {:#x}",
        kstart,
        kend,
        (kend - kstart) >> 10,
        pmm::reserved_end()
    );
    let dev = crate::platform::CURRENT.device;
    kprintln!(
        "mem: MMU+caches on (4K granule, 48-bit VA, higher half at {:#x}: device {:#x}..{:#x}, RAM cacheable; TTBR0 user only)",
        mmu::KERNEL_BASE,
        dev.first().map_or(0, |r| r.0),
        dev.last().map_or(0, |r| r.1)
    );
    kprintln!(
        "mem: pmm {} frames total, {} reserved, {} free ({} MiB)",
        s.total_frames,
        s.reserved_frames,
        s.free_frames,
        (s.free_frames * PAGE_SIZE) >> 20
    );
    kprintln!(
        "mem: heap {} KiB ({} KiB free)",
        h.total >> 10,
        h.free >> 10
    );
}
