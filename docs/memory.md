# Memory management

Files: `kernel/src/{dtb,mmu,pmm,heap,mem,memtest,spin}.rs`, header + DTB hand-off in `boot.S`.

## Boot sequence (`mem::init`, called first thing in `kmain`)
1. `boot.S` (at the physical load address, MMU off, position independent) zeroes
   .bss, fills two static boot tables (`boot_l0`/`boot_l1`: 1 GiB of MMIO + the first
   1 GiB of RAM, used as both TTBR0 = identity and TTBR1 = `KERNEL_BASE + pa`), turns on
   MMU + D-cache + I-cache, branches to the higher half, sets sp and saves `x0` (device
   tree pointer) in `__boot_dtb`.
2. `dtb.rs` finds `/memory` in the blob. No DTB -> assume 512 MiB at 0x4000_0000.
3. `mmu::init` builds the kernel's real tables (linear map of MMIO and *all* of RAM) in
   a static pool (no allocator yet), switches TTBR1 to them (through a 6-instruction
   trampoline called at its physical address) and points TTBR0 at an empty table.
4. `pmm::init` puts a bitmap right after the kernel image (`__kernel_end`), marks
   everything up to there used, and `pmm::reserve`s the DTB.
5. `heap::init` takes 4 MiB of contiguous frames. The heap grows on demand.
6. `memtest::run` is the boot self-test (panics on failure).

### Why a flat Image, not the ELF
qemu passes a DTB in `x0` only when it boots an arm64 *Image* (a flat binary
with the 64-byte Linux header). A plain ELF gets `x0 = 0` and no DTB anywhere
in RAM (checked with `info roms`). So `boot.S` starts with an Image header and
`scripts/run.sh` runs `llvm-objcopy -O binary` and boots the `.bin`. The ELF
is linked at the higher-half VMA (its LMA is the physical load address), so only
the flat Image boots. The DTB sits at RAM + 128 MiB (1 MiB window).

## Address map: higher-half kernel
`KERNEL_BASE = 0xffff_0000_0000_0000`; physical `pa` is at kernel VA `KERNEL_BASE + pa`.

| VA range | TTBR | type | notes |
|---|---|---|---|
| `0x1000..0x1_0000_0000_0000` | 0 | user (EL0, nG, ASID) | `mmu::USER_BASE..USER_END`; page 0 unmapped |
| `0xffff_0000_0000_0000..+0x4000_0000` | 1 | Device-nGnRE, RW, XN | UART, GIC, fw_cfg, virtio-mmio, PCIe low MMIO |
| `0xffff_0000_4000_0000..+RAM size` | 1 | Normal WB, inner shareable, RW+X | all RAM; the kernel image at `0xffff_0000_4008_0000` |
| `0xffff_8000_0000_0000..+2 GiB` | 1 | Normal WB, RW, XN | `VMAP_BASE`: kernel stacks with guard pages |
| everything else | | unmapped | |

TTBR0 is only ever user space: a user thread's table, or an empty table for
kernel-only threads (`mmu::kernel_ttbr0()`). Kernel mappings are global and live only
in TTBR1 (`mmu::kernel_ttbr1()`), so user tables have nothing of the kernel in them.

Always go through `mmu::phys_to_virt` / `mmu::virt_to_phys` (and the `PhysAddr` /
`VirtAddr` aliases). MMIO bases are `phys_to_virt(0x0900_0000)` etc. (`const fn`).
Anything handed to qemu devices (fw_cfg DMA descriptors, ramfb, virtqueues, buffers)
needs a *physical* address: use `virt_to_phys`, which also handles kernel-stack
addresses (a stack is physically contiguous). It panics on a non-kernel address, so a
stray identity-era pointer shows up loudly instead of DMA-ing to the wrong place.

`AddressSpace` knows its half (`Half::Lower` for TTBR0 trees, `Half::Upper` for the
kernel's): `map`/`translate`/`unmap_page` refuse VAs of the other half.

TCR: 4 KiB granule, 48-bit VA in both halves (4 levels), WB/WA walks, inner shareable,
IPS from the CPU (capped at 48 bits). MAIR slots: 0 Device-nGnRE,
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
- `mmu::AddressSpace`: `new(half, alloc)`, `new_user`, `map` (blocks + pages),
  `translate`, `unmap_page`, `destroy`. `mmu::with_kernel_space`,
  `mmu::map_device(pa, len)` (for MMIO above 1 GiB; returns the kernel VA).

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

## Secondary CPUs
`smp.S` gets the boot identity tables (`mmu::boot_ttbr0()`) for TTBR0 and the kernel's
tables for TTBR1 in its `BootBlock`, turns the MMU on, branches high, then swaps TTBR0
for the empty table before running Rust.

## W^X (not done)
Per-section permissions need 4 KiB pages for the image and page-aligned section
boundaries in `linker.ld`; `map` already picks pages when alignment demands.

## Not done / hazards
- Kernel RAM is mapped RWX (no W^X, no stack guard pages).
- Single bank of RAM (the one containing the kernel); other DTB banks are ignored.
- The boot tables cover only the first 1 GiB of RAM; everything up to `mmu::init`
  (DTB parsing) must stay inside it. qemu puts the image and DTB there.
