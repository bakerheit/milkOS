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
qemu passes a DTB in `x0` only when it boots an arm64 *Image* (a flat file
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
1 Normal WB, 2 Normal non-cacheable, 3 Device-nGnRnE. SCTLR: M, C, I, SA on, WXN on
after the boot tables (see "W^X"); alignment checking off.

## APIs
- `pmm::alloc_frame()`, `alloc_frame_zeroed()`, `free_frame(pa)`.
- `pmm::alloc_contiguous(count, align_frames)` / `_zeroed`, `free_contiguous(pa, count)`.
  Alignment is in frames and relative to physical address 0 (so
  `align_frames = 512` gives a 2 MiB-aligned run). Double free panics.
- `pmm::reserve(pa, len)`, `pmm::stats()`.
- `pmm::alloc_frame_as(Use)`, `alloc_frame_zeroed_as`, `alloc_contiguous_as(Use, count, align)`,
  `alloc_contiguous_zeroed_as`, `set_use(pa, count, Use)`: the same, saying what the frames are
  for (see "Where the memory goes"). The plain ones count as `Use::Other`.
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

## Where the memory goes
Every allocatable frame has a one-byte `pmm::Use` tag (an array after the bitmap, 1 byte per
4 KiB). Allocations say what they are for (`*_as` allocators, `set_use`); a free resets the
tag, so a count can't drift when an owner forgets to update it. `/proc/meminfo` prints them
after `Shmem`, in kB:

| Line | What |
| --- | --- |
| `KernelImage` | the kernel image (text, rodata, data, bss, boot stack) without the built-in files |
| `BuiltinFiles` | the initramfs archive, inside the image's rodata |
| `BootReserved` | RAM below the image, the PMM's bitmap and tags, the device tree blob |
| `KernelHeap` (above) | the heap's chunks, used or free (the heap never shrinks) |
| `PageTables` | page-table pages of user spaces and the kernel stacks' mappings |
| `KernelStacks` | kernel thread stacks |
| `AppPages` | user processes' private pages, each counted once (COW-shared ones too) |
| `RootFiles` | tmpfs file data that is not in the image (written or changed files) |
| `PageCache` | the disk page cache (`fs/pagecache.rs`) |
| `Display` | the framebuffer (virtio-gpu backing or ramfb) and the compositor's back buffer |
| `Wallpaper` | the wallpaper and its two blurred glass copies |
| `WindowBuffers` | window surfaces, userland windows' shared and front buffers |
| `PanelCaches` | cached panels (menu bar, dock, toasts), Spaces thumbnails, the lock screen |
| `Devices` | virtqueues, DMA buffers, RAM disks |
| `Other` | the rest of MemTotal - MemFree: frames allocated without a `Use` |

They add up to `MemTotal - MemFree` (the fs self-test checks it). Window buffers under
128 KiB come from the heap (`wm/buffer.rs`) and are counted there. Activity Monitor's Memory
tab draws them as Apps, Kernel heap, Kernel image, Files in RAM, Screen (display, wallpaper,
panel caches), Windows, Other kernel (stacks, page tables, devices, boot, unaccounted), Page
cache and Free.

## Secondary CPUs
`smp.S` gets the boot identity tables (`mmu::boot_ttbr0()`) for TTBR0 and the kernel's
tables for TTBR1 in its `BootBlock`, turns the MMU on, branches high, then swaps TTBR0
for the empty table before running Rust.

## W^X
* **Kernel:** linker.ld page-aligns `.text`, `.rodata` and `.data`; `mmu::init` maps the
  image by section in 4 KiB pages: text read + execute, rodata read only, data / bss /
  boot stack read/write + execute-never. All other RAM (heap, page tables, user frames in
  the linear map), the kernel stacks and MMIO are read/write + execute-never.
  `SCTLR_EL1.WXN` (writable implies execute-never) is set once the CPU runs on these
  tables (boot CPU in `mmu::init`, secondaries in smp.S): the boot tables are a single
  writable + executable block. The boot self-test (`memtest::test_wx`) writes to text and
  rodata and jumps into the heap and a stack through fault probes
  (`exceptions::probe_write` / `probe_exec`); each must be a permission fault.
* **User:** stacks, brk and anonymous memory are read/write, never executable; ELF
  segments get exact permissions and where a page is shared by text and data, writable
  wins (like Linux's later mapping). `mmap` / `mprotect` with PROT_WRITE | PROT_EXEC is
  refused (EACCES): nothing on milkOS needs a JIT. `utest wx` checks that jumping into the
  heap or stack and writing to text are SIGSEGV.

## Not done / hazards
- Single bank of RAM (the one containing the kernel); other DTB banks are ignored.
- The boot tables cover only the first 1 GiB of RAM; everything up to `mmu::init`
  (DTB parsing) must stay inside it. qemu puts the image and DTB there.
