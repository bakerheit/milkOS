# Memory management

Files: `kernel/src/{dtb,mmu,pmm,heap,mem,memtest,spin}.rs`, header + DTB hand-off in `boot.S`.

## Boot sequence (`mem::init`, called first thing in `kmain`)
1. `boot.S` saves `x0` (device tree pointer) in `__boot_dtb`.
2. `dtb.rs` finds `/memory` in the blob. No DTB -> assume 512 MiB at 0x4000_0000.
3. `mmu::init` builds the boot page tables in a static pool (no allocator, no
   locks, MMU still off) and turns on MMU + D-cache + I-cache.
4. `pmm::init` puts a bitmap right after the kernel image (`__kernel_end`), marks
   everything up to there used, and `pmm::reserve`s the DTB.
5. `heap::init` takes 4 MiB of contiguous frames. The heap grows on demand.
6. `memtest::run` is the boot self-test (panics on failure).

### Why a flat Image, not the ELF
qemu passes a DTB in `x0` only when it boots an arm64 *Image* (a flat binary
with the 64-byte Linux header). A plain ELF gets `x0 = 0` and no DTB anywhere
in RAM (checked with `info roms`). So `boot.S` starts with an Image header and
`scripts/run.sh` runs `llvm-objcopy -O binary` and boots the `.bin`. The ELF
still boots (entry is the same first instruction, a branch over the header); it
just lands on the 512 MiB default. The DTB sits at RAM + 128 MiB (1 MiB window).

## Address map today
| range | type | notes |
|---|---|---|
| `0x0000_0000..0x4000_0000` | Device-nGnRE, RW, XN | UART, GIC, fw_cfg, virtio-mmio, PCIe low MMIO |
| `0x4000_0000..RAM end` | Normal WB cacheable, inner shareable, RW+X | 2 MiB blocks (1 GiB blocks if RAM >= 1 GiB and aligned) |
| everything else | unmapped | |

Identity mapped: VA == PA. Always go through `mmu::phys_to_virt` /
`mmu::virt_to_phys` (and the `PhysAddr` / `VirtAddr` aliases) so the identity
assumption lives in one place. Anything handed to qemu devices (fw_cfg, ramfb,
virtqueues) needs a *physical* address: use `virt_to_phys`.

TCR: 4 KiB granule, 48-bit VA (4 levels), WB/WA walks, inner shareable, IPS from
the CPU. TTBR1 walks are disabled (`EPD1`) for now. MAIR slots: 0 Device-nGnRE,
1 Normal WB, 2 Normal non-cacheable, 3 Device-nGnRnE. SCTLR: M, C, I, SA on;
WXN and alignment checking off (RAM is currently RWX; see "Not done").

## APIs
- `pmm::alloc_frame()`, `alloc_frame_zeroed()`, `free_frame(pa)`.
- `pmm::alloc_contiguous(count, align_frames)` / `_zeroed`, `free_contiguous(pa, count)`.
  Alignment is in frames and relative to physical address 0 (so
  `align_frames = 512` gives a 2 MiB-aligned run). Double free panics.
- `pmm::reserve(pa, len)`, `pmm::stats()`.
- `alloc::{Box, Vec, String, ...}` just work (`heap.rs` is the `#[global_allocator]`).
  Address-ordered free list, 16-byte granularity, coalescing, grows in >= 1 MiB
  contiguous chunks from the PMM, never shrinks. Don't allocate in IRQ context.
- `mmu::AddressSpace`: `new`, `map` (blocks + pages), `translate`, `unmap_page`,
  `destroy`. `mmu::with_kernel_space`, `mmu::map_device(pa, len)`.

### DMA buffers (virtio, ramfb, ...)
```rust
let pa = pmm::alloc_contiguous_zeroed(npages, 1).unwrap();   // physically contiguous
let va = mmu::phys_to_virt(pa) as *mut u8;                   // CPU pointer
// give `pa` to the device. free with pmm::free_contiguous(pa, npages)
```
Use `align_frames` for rings with alignment needs. qemu's emulated devices are
cache-coherent with the guest, so no cache maintenance is needed in qemu. On real
hardware you would map the buffer `MemType::NormalNc` or clean/invalidate.
The 3 MiB ramfb static in `.bss` can move to `alloc_contiguous_zeroed(768, 1)`.

## Going to per-process address spaces and a higher-half kernel
The page-table code is already independent of the kernel's own tables:
`AddressSpace` owns a root table, `map`/`translate`/`unmap_page` work on any
space, and table pages come from a `TableAlloc` (boot pool or PMM). Steps:

1. **Processes**: `AddressSpace::new(&mut PmmTables)` per process; map user pages
   with `MapAttrs { user: true, .. }` (sets AP[1], nG, PXN) in the low half; on a
   context switch write the space's root plus an ASID (TCR.A1 = 0 -> ASID in
   TTBR0[63:48]) to `TTBR0_EL1`, no TLB flush needed when ASIDs are distinct.
   Free with `destroy()` then free the leaf frames. Add `unmap_range`, and
   copy-on-write via the AP bits + a data-abort handler.
2. **Higher half**: add `KERNEL_BASE = 0xffff_0000_0000_0000` and make
   `PHYS_OFFSET` in `mmu.rs` equal to it. Build the kernel map in a second
   `AddressSpace` whose root goes in `TTBR1_EL1`, clear `TCR.EPD1`, link the
   kernel at `KERNEL_BASE + 0x4008_0000` (VMA) with LMA unchanged, and keep the
   current identity map in TTBR0 only during early boot (boot.S runs position
   independent until the switch). After the jump, TTBR0 holds just the current
   user process. Drop the TTBR0 identity map; MMIO goes through
   `map_device`, which already returns a VA.
3. **Everything that passes addresses to devices** uses `virt_to_phys` (already
   the rule), so step 2 doesn't touch drivers.
4. Per-section permissions (W^X) need 4 KiB pages for the image and
   page-aligned section boundaries in `linker.ld`; `map` already picks pages
   when alignment demands.

## Not done / hazards
- Kernel RAM is mapped RWX (no W^X, no stack guard pages).
- Single bank of RAM (the one containing the kernel); other DTB banks are ignored.
- Atomics and the spinlock (`spin.rs`) must not be used before `mmu::init`:
  exclusives to Device memory (MMU off) hang.
- TLB maintenance is inner-shareable but there is only one core running.
