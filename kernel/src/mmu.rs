#![allow(dead_code)]
//! Page tables and the MMU (4 KiB granule, 4 levels, 48-bit VA).
//!
//! * [`AddressSpace`] is a page-table tree that can map/unmap/translate. It is
//!   not tied to the kernel's own tables, so per-process spaces are just more
//!   `AddressSpace::new` calls (see docs/memory.md).
//! * boot.S turns the MMU on with two static boot tables (identity in TTBR0, the same
//!   1 GiB of RAM + 1 GiB of MMIO at KERNEL_BASE in TTBR1) and branches to the higher half.
//! * [`init`] builds the kernel's real tables (a linear map of MMIO and all of RAM) in a
//!   small static pool (no allocator yet), switches TTBR1 to them and empties TTBR0.
//! * Every physical/virtual conversion goes through [`phys_to_virt`] and
//!   [`virt_to_phys`].

use crate::platform;
use crate::pmm;
use crate::sync::SpinLock;
use core::arch::global_asm;
use core::cell::UnsafeCell;

pub type PhysAddr = usize;
pub type VirtAddr = usize;

pub const PAGE_SIZE: usize = 4096;
const ENTRIES: usize = 512;
/// Highest VA bit we use: 48-bit address space per TTBR.
const VA_BITS: u32 = 48;

/// Base of the kernel's linear map in the TTBR1 (upper) half: physical address `pa` is at
/// virtual `KERNEL_BASE + pa`. The kernel image itself is linked inside it (linker.ld:
/// VMA = KERNEL_BASE + load address).
pub const KERNEL_BASE: usize = 0xffff_0000_0000_0000;
const PHYS_OFFSET: usize = KERNEL_BASE;

pub const fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    pa + PHYS_OFFSET
}

/// Kernel virtual address to physical. Handles the linear map and the kernel-stack window
/// ([`VMAP_BASE`]..[`VMAP_END`], see `sched/kstack.rs`): a kernel stack is physically
/// contiguous, so a buffer on one is DMA-able as long as it doesn't straddle the stack's end.
pub fn virt_to_phys(va: VirtAddr) -> PhysAddr {
    if (VMAP_BASE..VMAP_END).contains(&va) {
        return crate::sched::kstack::virt_to_phys(va);
    }
    assert!(
        va >= PHYS_OFFSET,
        "virt_to_phys({va:#x}): not a kernel address (user or identity pointer?)"
    );
    va - PHYS_OFFSET
}

// ---- layout -----------------------------------------------------------------
//
// Higher-half kernel (see docs/memory.md):
//   TTBR1 (0xffff_0000_0000_0000 ..): the kernel. A linear map of MMIO (first 1 GiB of
//          PA) and all of RAM at KERNEL_BASE + pa (the image lives inside it), plus the
//          kernel-stack window. Global, EL1-only.
//   TTBR0 (0 .. 2^48): user space only. Kernel-only threads run with an empty TTBR0.

/// First user virtual address. Page 0 stays unmapped so a null pointer faults. Linux's
/// static binaries load at 0x400000; milkOS's own programs at 0x10000400000. Both fit.
pub const USER_BASE: VirtAddr = 0x1000;
/// One past the last user virtual address (top of the 48-bit TTBR0 range).
pub const USER_END: VirtAddr = 1 << VA_BITS;
/// Kernel-virtual window for things that need guard pages (kernel stacks), in TTBR1 above
/// the linear map.
pub const VMAP_BASE: VirtAddr = 0xffff_8000_0000_0000;
pub const VMAP_END: VirtAddr = 0xffff_8000_8000_0000;

/// Which TTBR an [`AddressSpace`] is for: lower (TTBR0, user, top 16 VA bits 0) or upper
/// (TTBR1, kernel, top 16 bits 1). A tree only answers for VAs of its own half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Half {
    Lower,
    Upper,
}

impl Half {
    /// The 48-bit table index part of `va` if it belongs to this half, else None.
    fn index(self, va: VirtAddr) -> Option<usize> {
        match (self, va >> VA_BITS) {
            (Half::Lower, 0) => Some(va),
            (Half::Upper, 0xffff) => Some(va & ((1 << VA_BITS) - 1)),
            _ => None,
        }
    }
}

// ---- MAIR attribute slots (referenced by AttrIdx in descriptors) ----------
const MAIR_DEVICE_NGNRE: u64 = 0x04;
const MAIR_NORMAL_WB: u64 = 0xff; // inner+outer write-back, read+write allocate
const MAIR_NORMAL_NC: u64 = 0x44; // inner+outer non-cacheable
const MAIR_DEVICE_NGNRNE: u64 = 0x00;
const MAIR_VALUE: u64 =
    MAIR_DEVICE_NGNRE | (MAIR_NORMAL_WB << 8) | (MAIR_NORMAL_NC << 16) | (MAIR_DEVICE_NGNRNE << 24);

/// What kind of memory a mapping is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemType {
    /// MMIO: Device-nGnRE (no gathering, no reordering, early write ack).
    Device,
    /// RAM: Normal, write-back cacheable, inner shareable.
    Normal,
    /// RAM that must bypass the caches (for a non-coherent DMA device).
    NormalNc,
    /// Strictest device type, for registers that need non-posted writes.
    DeviceStrict,
}

#[derive(Clone, Copy, Debug)]
pub struct MapAttrs {
    pub mem: MemType,
    pub write: bool,
    pub exec: bool,
    /// Accessible from EL0 (userspace). Such mappings are never executable at EL1.
    pub user: bool,
}

impl MapAttrs {
    pub const KERNEL_DEVICE: MapAttrs = MapAttrs {
        mem: MemType::Device,
        write: true,
        exec: false,
        user: false,
    };
    pub const KERNEL_RAM: MapAttrs = MapAttrs {
        mem: MemType::Normal,
        write: true,
        exec: true,
        user: false,
    };
    pub const KERNEL_DATA: MapAttrs = MapAttrs {
        mem: MemType::Normal,
        write: true,
        exec: false,
        user: false,
    };
    /// Kernel code: read + execute, never writable (W^X).
    pub const KERNEL_TEXT: MapAttrs = MapAttrs {
        mem: MemType::Normal,
        write: false,
        exec: true,
        user: false,
    };
    /// Kernel constants: read only, execute never.
    pub const KERNEL_RODATA: MapAttrs = MapAttrs {
        mem: MemType::Normal,
        write: false,
        exec: false,
        user: false,
    };

    fn bits(self) -> u64 {
        let attr_idx = match self.mem {
            MemType::Device => 0,
            MemType::Normal => 1,
            MemType::NormalNc => 2,
            MemType::DeviceStrict => 3,
        };
        let mut d = attr_idx << 2;
        // AP[2:1]: 00 EL1 rw, 01 EL0+EL1 rw, 10 EL1 ro, 11 EL0+EL1 ro.
        d |= match (self.write, self.user) {
            (true, false) => 0b00,
            (true, true) => 0b01,
            (false, false) => 0b10,
            (false, true) => 0b11,
        } << 6;
        // Shareability: inner for RAM, outer for device (ignored for device).
        d |= match self.mem {
            MemType::Normal | MemType::NormalNc => 0b11,
            _ => 0b10,
        } << 8;
        d |= DESC_AF;
        if self.user {
            d |= DESC_NG;
        }
        // Privileged-execute-never unless exec; user-execute-never unless a
        // user mapping asks for exec.
        if !self.exec || self.user {
            d |= DESC_PXN;
        }
        if !(self.exec && self.user) {
            d |= DESC_UXN;
        }
        d
    }
}

const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE_OR_PAGE: u64 = 1 << 1;
const DESC_AF: u64 = 1 << 10;
const DESC_NG: u64 = 1 << 11;
const DESC_PXN: u64 = 1 << 53;
const DESC_UXN: u64 = 1 << 54;
const DESC_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapError {
    /// Address or length not 4 KiB aligned, empty, or outside the 48-bit space.
    BadRange,
    /// Part of the range is already mapped (possibly by a larger block).
    AlreadyMapped,
    /// Ran out of memory for page-table pages.
    OutOfMemory,
}

/// One page-table page (512 descriptors).
#[repr(C, align(4096))]
pub struct Table([u64; ENTRIES]);

/// Source of zeroed 4 KiB pages for page-table nodes.
pub trait TableAlloc {
    fn alloc_table(&mut self) -> Option<PhysAddr>;
}

/// Page-table pages from the physical frame allocator.
pub struct PmmTables;

impl TableAlloc for PmmTables {
    fn alloc_table(&mut self) -> Option<PhysAddr> {
        pmm::alloc_frame_zeroed_as(pmm::Use::PageTables)
    }
}

/// A tree of page tables. Dropping does not free the tables (kernel space is
/// forever); a process space would add a `destroy` that walks and frees.
pub struct AddressSpace {
    root: PhysAddr,
    half: Half,
}

const fn level_shift(level: usize) -> u32 {
    39 - 9 * level as u32
}

fn table_ptr(pa: PhysAddr) -> *mut u64 {
    phys_to_virt(pa) as *mut u64
}

impl AddressSpace {
    /// An empty address space with a fresh root table, for TTBR0 or TTBR1 (`half`).
    pub fn new(half: Half, alloc: &mut dyn TableAlloc) -> Option<AddressSpace> {
        Some(AddressSpace {
            root: alloc.alloc_table()?,
            half,
        })
    }

    /// Wrap an existing, already-zeroed root table.
    pub fn from_root(half: Half, root: PhysAddr) -> AddressSpace {
        AddressSpace { root, half }
    }

    /// Physical address of the root table (what goes in TTBRn_EL1).
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    /// A fresh, empty user address space (TTBR0). The kernel lives in TTBR1, so nothing of
    /// it is in here.
    pub fn new_user() -> Option<AddressSpace> {
        Some(AddressSpace {
            root: pmm::alloc_frame_zeroed_as(pmm::Use::PageTables)?,
            half: Half::Lower,
        })
    }

    /// Free the page-table pages of a space made by [`new_user`](Self::new_user), the root
    /// included. Leaf frames are NOT freed.
    ///
    /// # Safety
    /// The space must not be live in any TTBR (or the TLB must be flushed for its ASID
    /// before it is reused).
    pub unsafe fn destroy_user(self) {
        unsafe fn free_tree(table: PhysAddr, level: usize) {
            if level < 3 {
                for slot in 0..ENTRIES {
                    // SAFETY: walking tables the caller owns.
                    let desc = unsafe { table_ptr(table).add(slot).read_volatile() };
                    if desc & DESC_VALID != 0 && desc & DESC_TABLE_OR_PAGE != 0 {
                        unsafe { free_tree((desc & DESC_ADDR_MASK) as usize, level + 1) };
                    }
                }
            }
            pmm::free_frame(table);
        }
        for slot in 0..ENTRIES {
            // SAFETY: walking our own root.
            let desc = unsafe { table_ptr(self.root).add(slot).read_volatile() };
            if desc & DESC_VALID != 0 && desc & DESC_TABLE_OR_PAGE != 0 {
                unsafe { free_tree((desc & DESC_ADDR_MASK) as usize, 1) };
            }
        }
        pmm::free_frame(self.root);
    }

    /// Map `[va, va+len)` to `[pa, pa+len)`. Uses 1 GiB / 2 MiB blocks where
    /// alignment and length allow, 4 KiB pages otherwise. Never overwrites an
    /// existing mapping.
    pub fn map(
        &mut self,
        mut va: VirtAddr,
        mut pa: PhysAddr,
        mut len: usize,
        attrs: MapAttrs,
        alloc: &mut dyn TableAlloc,
    ) -> Result<(), MapError> {
        let iva = self.half.index(va).ok_or(MapError::BadRange)?;
        if len == 0
            || !va.is_multiple_of(PAGE_SIZE)
            || !pa.is_multiple_of(PAGE_SIZE)
            || !len.is_multiple_of(PAGE_SIZE)
            || iva.checked_add(len).is_none_or(|e| e > 1 << VA_BITS)
        {
            return Err(MapError::BadRange);
        }
        let attr_bits = attrs.bits();
        while len > 0 {
            let mut level = 3;
            for l in 1..=2 {
                let size = 1usize << level_shift(l);
                if va.is_multiple_of(size) && pa.is_multiple_of(size) && len >= size {
                    level = l;
                    break;
                }
            }
            self.map_one(va, pa, level, attr_bits, alloc)?;
            let size = 1usize << level_shift(level);
            va += size;
            pa += size;
            len -= size;
        }
        // Make the new descriptors visible to the table walker.
        // SAFETY: barriers only.
        unsafe { core::arch::asm!("dsb ishst", "isb", options(nostack)) };
        Ok(())
    }

    fn map_one(
        &mut self,
        va: VirtAddr,
        pa: PhysAddr,
        level: usize,
        attr_bits: u64,
        alloc: &mut dyn TableAlloc,
    ) -> Result<(), MapError> {
        let mut table = self.root;
        for l in 0..level {
            let slot = (va >> level_shift(l)) & (ENTRIES - 1);
            // SAFETY: `table` is a page-table page owned by this space and
            // `slot` < 512.
            let entry = unsafe { table_ptr(table).add(slot) };
            let desc = unsafe { entry.read_volatile() };
            if desc & DESC_VALID == 0 {
                let next = alloc.alloc_table().ok_or(MapError::OutOfMemory)?;
                unsafe {
                    entry.write_volatile(next as u64 | DESC_TABLE_OR_PAGE | DESC_VALID);
                }
                table = next;
            } else if desc & DESC_TABLE_OR_PAGE == 0 {
                return Err(MapError::AlreadyMapped); // a block covers this range
            } else {
                table = (desc & DESC_ADDR_MASK) as usize;
            }
        }
        let slot = (va >> level_shift(level)) & (ENTRIES - 1);
        // SAFETY: as above.
        let entry = unsafe { table_ptr(table).add(slot) };
        if unsafe { entry.read_volatile() } & DESC_VALID != 0 {
            return Err(MapError::AlreadyMapped);
        }
        let kind = if level == 3 { DESC_TABLE_OR_PAGE } else { 0 };
        unsafe { entry.write_volatile(pa as u64 | attr_bits | kind | DESC_VALID) };
        Ok(())
    }

    /// Free every page-table page of this space (the root included) back to
    /// the frame allocator. Leaf frames are NOT freed; the owner does that.
    ///
    /// # Safety
    /// The space must not be live in any TTBR, and its tables must have come
    /// from the PMM (never call this on the kernel's boot-pool space).
    pub unsafe fn destroy(self) {
        unsafe fn free_tree(table: PhysAddr, level: usize) {
            if level < 3 {
                for slot in 0..ENTRIES {
                    // SAFETY: walking tables the caller owns.
                    let desc = unsafe { table_ptr(table).add(slot).read_volatile() };
                    if desc & DESC_VALID != 0 && desc & DESC_TABLE_OR_PAGE != 0 {
                        unsafe { free_tree((desc & DESC_ADDR_MASK) as usize, level + 1) };
                    }
                }
            }
            pmm::free_frame(table);
        }
        // SAFETY: forwarded from the caller's contract.
        unsafe { free_tree(self.root, 0) };
    }

    /// Software page-table walk. Returns the physical address `va` maps to.
    pub fn translate(&self, va: VirtAddr) -> Option<PhysAddr> {
        let va = self.half.index(va)?;
        let mut table = self.root;
        for level in 0..4 {
            let slot = (va >> level_shift(level)) & (ENTRIES - 1);
            // SAFETY: walking tables we own.
            let desc = unsafe { table_ptr(table).add(slot).read_volatile() };
            if desc & DESC_VALID == 0 {
                return None;
            }
            let is_leaf = level == 3 || desc & DESC_TABLE_OR_PAGE == 0;
            if is_leaf {
                if level == 0 {
                    return None;
                }
                let mask = (1usize << level_shift(level)) - 1;
                return Some((desc & DESC_ADDR_MASK) as usize & !mask | (va & mask));
            }
            table = (desc & DESC_ADDR_MASK) as usize;
        }
        None
    }

    /// Unmap one 4 KiB page that was mapped as a page (not inside a block) and
    /// flush its TLB entry. Returns the physical page it pointed at.
    pub fn unmap_page(&mut self, va: VirtAddr) -> Option<PhysAddr> {
        self.half.index(va)?;
        if !va.is_multiple_of(PAGE_SIZE) {
            return None;
        }
        let mut table = self.root;
        for level in 0..3 {
            let slot = (va >> level_shift(level)) & (ENTRIES - 1);
            // SAFETY: walking tables we own.
            let desc = unsafe { table_ptr(table).add(slot).read_volatile() };
            if desc & DESC_VALID == 0 || desc & DESC_TABLE_OR_PAGE == 0 {
                return None; // not mapped, or inside a block
            }
            table = (desc & DESC_ADDR_MASK) as usize;
        }
        let slot = (va >> 12) & (ENTRIES - 1);
        // SAFETY: as above.
        let entry = unsafe { table_ptr(table).add(slot) };
        let desc = unsafe { entry.read_volatile() };
        if desc & DESC_VALID == 0 {
            return None;
        }
        unsafe {
            entry.write_volatile(0);
            // Single-address TLB invalidate for all ASIDs, inner shareable.
            core::arch::asm!(
                "dsb ishst",
                "tlbi vaae1is, {0}",
                "dsb ish",
                "isb",
                in(reg) (va >> 12) as u64,
                options(nostack)
            );
        }
        Some((desc & DESC_ADDR_MASK) as usize)
    }
}

// ---- the kernel address space ---------------------------------------------

static KERNEL_SPACE: SpinLock<Option<AddressSpace>> = SpinLock::new(None);

/// Number of page-table pages available before the frame allocator exists: L0, L1, the
/// L2s for RAM, and L3s for the kernel image, which is mapped in 4 KiB pages by section
/// (W^X; ~10 MiB = 5 L3 tables) plus the RAM below it.
const BOOT_TABLES: usize = 32;

struct BootPool {
    tables: UnsafeCell<[Table; BOOT_TABLES]>,
}

// SAFETY: only touched by core 0 in `init`, before anything else runs.
unsafe impl Sync for BootPool {}

static BOOT_POOL: BootPool = BootPool {
    tables: UnsafeCell::new([const { Table([0; ENTRIES]) }; BOOT_TABLES]),
};

struct BootTables {
    used: usize,
}

impl TableAlloc for BootTables {
    fn alloc_table(&mut self) -> Option<PhysAddr> {
        if self.used == BOOT_TABLES {
            return None;
        }
        let base = BOOT_POOL.tables.get() as *mut Table;
        // SAFETY: in bounds; the pool is .bss so already zero.
        let t = unsafe { base.add(self.used) };
        self.used += 1;
        Some(virt_to_phys(t as usize))
    }
}

/// TTBR0 for kernel-only threads: an empty table (any low address faults).
#[repr(C, align(4096))]
struct EmptyTable([u64; ENTRIES]);
static EMPTY_TTBR0: EmptyTable = EmptyTable([0; ENTRIES]);
static KERNEL_TTBR1: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

unsafe extern "C" {
    /// boot.S: the boot tables (identity in TTBR0, same L0 in TTBR1). Kept for secondaries,
    /// which turn their MMU on through them.
    static boot_l0: Table;
    fn mmu_switch_ttbr1(root: u64);
    // linker.ld: the image's sections, page aligned.
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_end: u8;
    static __kernel_end: u8;
}

/// Physical [start, end) of the kernel's text, rodata, and data+bss+boot stack.
fn image_sections() -> [(PhysAddr, PhysAddr, MapAttrs); 3] {
    let pa = |s: *const u8| virt_to_phys(s as usize);
    let (t0, t1) = (pa(&raw const __text_start), pa(&raw const __text_end));
    let (r1, k1) = (pa(&raw const __rodata_end), pa(&raw const __kernel_end));
    [
        (t0, t1, MapAttrs::KERNEL_TEXT),
        (t1, r1, MapAttrs::KERNEL_RODATA),
        (r1, k1.next_multiple_of(PAGE_SIZE), MapAttrs::KERNEL_DATA),
    ]
}

global_asm!(
    include_str!("el.S"),
    include_str!("boot.S"),
    MAIR = const MAIR_VALUE,
    TCR = const TCR_BOOT,
    SCTLR_ON = const SCTLR_M | SCTLR_C | SCTLR_SA | SCTLR_I,
    SCTLR_OFF = const SCTLR_A | SCTLR_WXN,
    L1_0 = const boot_block(0),
    L1_1 = const boot_block(1),
    L1_2 = const boot_block(2),
    L1_3 = const boot_block(3),
    RELOC = const platform::RELOCATE as u64,
    TEXT_OFFSET = const platform::TEXT_OFFSET,
);

// The TTBR1 swap must run while nothing uses TTBR1: it is called at its *physical*
// address, i.e. through the boot identity map in TTBR0, and touches no memory.
global_asm!(
    ".section .text",
    ".global mmu_switch_ttbr1",
    "mmu_switch_ttbr1:",
    "    dsb ishst",
    "    msr ttbr1_el1, x0",
    "    isb",
    "    tlbi vmalle1",
    "    dsb nsh",
    "    isb",
    "    ret",
);

/// Build the kernel's final tables for TTBR1 (linear map of MMIO and all of RAM, at
/// KERNEL_BASE + pa) and switch to them. The MMU is already on (boot.S, through the boot
/// tables); afterwards TTBR0 is empty until a user thread loads its own.
///
/// # Safety
/// Call once, on the boot CPU, before the frame allocator (tables come from the boot pool).
pub unsafe fn init(banks: &[crate::dtb::MemRange]) {
    let mut pool = BootTables { used: 0 };
    let mut space = AddressSpace::new(Half::Upper, &mut pool).expect("boot table pool empty");
    // MMIO, as the board has it (qemu virt: everything below 1 GiB; Pi: the top 64 MiB).
    for &(start, end) in platform::CURRENT.device {
        space
            .map(
                phys_to_virt(start),
                start,
                end - start,
                MapAttrs::KERNEL_DEVICE,
                &mut pool,
            )
            .expect("map device region");
    }
    // RAM, W^X: everything read/write + execute-never, except the kernel image's text
    // (read + execute) and rodata (read only). The image is mapped in 4 KiB pages; the rest
    // in 2 MiB / 1 GiB blocks where alignment allows. Only the banks that exist are mapped
    // (a gap between two is not memory: touching it could fault the bus).
    let secs = image_sections();
    let (img_start, img_end) = (secs[0].0, secs[2].1);
    let mut map = |start: PhysAddr, end: PhysAddr, attrs: MapAttrs| {
        if end > start {
            space
                .map(phys_to_virt(start), start, end - start, attrs, &mut pool)
                .expect("map RAM");
        }
    };
    for bank in banks {
        let (base, end) = (bank.base, bank.base + bank.size);
        if (base..end).contains(&img_start) {
            map(base, img_start, MapAttrs::KERNEL_DATA);
            for (a, b, attrs) in secs {
                map(a, b, attrs);
            }
            map(img_end, end, MapAttrs::KERNEL_DATA);
        } else {
            map(base, end, MapAttrs::KERNEL_DATA);
        }
    }
    let root = space.root() as u64;
    KERNEL_TTBR1.store(root, core::sync::atomic::Ordering::Relaxed);
    // SAFETY: called at its physical address through the boot identity map in TTBR0; it
    // only writes TTBR1 and flushes this CPU's TLB, then returns to our (kernel VA) caller,
    // which the new tables map exactly as the boot tables did.
    unsafe {
        let pa = virt_to_phys(mmu_switch_ttbr1 as *const () as usize);
        let f: extern "C" fn(u64) = core::mem::transmute(pa);
        f(root);
        // TTBR0 is user space from now on: kernel threads get an empty table.
        core::arch::asm!(
            "msr ttbr0_el1, {t}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            t = in(reg) kernel_ttbr0(),
            options(nostack)
        );
        // Hardware W^X on top: any writable page is execute-never (SCTLR.WXN), for EL1 and
        // EL0. Only now: the boot tables were one writable+executable block.
        set_wxn();
    }
    *KERNEL_SPACE.lock() = Some(space);
}

/// Turn on SCTLR_EL1.WXN on this CPU (and flush its TLB: WXN may be cached there).
/// smp.S does the same on secondaries once they run on the final tables.
pub fn set_wxn() {
    // SAFETY: system register write; every mapping in use honours W^X already.
    unsafe {
        core::arch::asm!(
            "mrs {t}, sctlr_el1",
            "orr {t}, {t}, {wxn}",
            "msr sctlr_el1, {t}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            t = out(reg) _,
            wxn = in(reg) SCTLR_WXN,
            options(nostack)
        );
    }
}

/// SCTLR.WXN bit, for smp.rs (secondaries enable the MMU without it, then set it).
pub const SCTLR_WXN_BIT: u64 = SCTLR_WXN;

/// Run `f` on the kernel's address space.
pub fn with_kernel_space<R>(f: impl FnOnce(&mut AddressSpace) -> R) -> R {
    let mut guard = KERNEL_SPACE.lock();
    f(guard.as_mut().expect("MMU not initialised"))
}

/// TTBR0 value for kernel-only threads: an empty table, ASID 0.
pub fn kernel_ttbr0() -> u64 {
    virt_to_phys(&raw const EMPTY_TTBR0 as usize) as u64
}

/// TTBR1 value: the kernel's tables (for secondary CPUs).
pub fn kernel_ttbr1() -> u64 {
    KERNEL_TTBR1.load(core::sync::atomic::Ordering::Relaxed)
}

/// Physical address of the boot tables (identity map), which secondaries use as TTBR0
/// while they turn their MMU on.
pub fn boot_ttbr0() -> u64 {
    virt_to_phys(&raw const boot_l0 as usize) as u64
}

/// Map an MMIO range into the kernel address space as Device memory.
/// The board's own device ranges (platform.rs) are already mapped; this is for PCIe high windows etc.
/// Returns the kernel virtual address.
pub fn map_device(pa: PhysAddr, len: usize) -> Result<VirtAddr, MapError> {
    let start = pa & !(PAGE_SIZE - 1);
    let end = (pa + len).next_multiple_of(PAGE_SIZE);
    with_kernel_space(|s| {
        s.map(
            phys_to_virt(start),
            start,
            end - start,
            MapAttrs::KERNEL_DEVICE,
            &mut PmmTables,
        )
    })?;
    Ok(phys_to_virt(pa))
}

// ---- system registers -------------------------------------------------------

const SCTLR_M: u64 = 1 << 0; // MMU enable
const SCTLR_A: u64 = 1 << 1; // alignment check (off: Normal memory allows unaligned)
const SCTLR_C: u64 = 1 << 2; // data cache
const SCTLR_SA: u64 = 1 << 3; // SP alignment check at EL1
const SCTLR_I: u64 = 1 << 12; // instruction cache
const SCTLR_WXN: u64 = 1 << 19; // writable => execute-never (set after the boot tables)

const TCR_T0SZ: u64 = 64 - VA_BITS as u64;
const TCR_IRGN0_WBWA: u64 = 0b01 << 8;
const TCR_ORGN0_WBWA: u64 = 0b01 << 10;
const TCR_SH0_INNER: u64 = 0b11 << 12;
const TCR_TG0_4K: u64 = 0b00 << 14;
const TCR_T1SZ: u64 = (64 - VA_BITS as u64) << 16;
const TCR_IRGN1_WBWA: u64 = 0b01 << 24;
const TCR_ORGN1_WBWA: u64 = 0b01 << 26;
const TCR_SH1_INNER: u64 = 0b11 << 28;
const TCR_TG1_4K: u64 = 0b10 << 30;
/// TCR_EL1 without IPS (boot.S ORs in the CPU's physical address size, capped at 48 bits).
const TCR_BOOT: u64 = TCR_T0SZ
    | TCR_IRGN0_WBWA
    | TCR_ORGN0_WBWA
    | TCR_SH0_INNER
    | TCR_TG0_4K
    | TCR_T1SZ
    | TCR_IRGN1_WBWA
    | TCR_ORGN1_WBWA
    | TCR_SH1_INNER
    | TCR_TG1_4K;
/// boot.S's 1 GiB block descriptors (address ORed in by boot.S): device (AttrIdx 0, XN)
/// and RAM (AttrIdx 1, inner shareable, executable at EL1).
const BOOT_BLOCK_DEV: u64 = DESC_VALID | DESC_AF | DESC_PXN | DESC_UXN | (0b10 << 8);
const BOOT_BLOCK_RAM: u64 = DESC_VALID | DESC_AF | DESC_UXN | (1 << 2) | (0b11 << 8);

/// The boot identity map's L1 entry for GiB `i` (boot.S): the board's choice (platform.rs).
const fn boot_block(i: usize) -> u64 {
    let addr = (i as u64) << 30;
    match platform::CURRENT.boot_map[i] {
        platform::Block::None => 0,
        platform::Block::Device => BOOT_BLOCK_DEV | addr,
        platform::Block::Ram => BOOT_BLOCK_RAM | addr,
    }
}

/// True if SCTLR_EL1 reports MMU, D-cache and I-cache all enabled.
pub fn caches_on() -> bool {
    let sctlr: u64;
    // SAFETY: read-only system register.
    unsafe { core::arch::asm!("mrs {0}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    sctlr & (SCTLR_M | SCTLR_C | SCTLR_I) == SCTLR_M | SCTLR_C | SCTLR_I
}
