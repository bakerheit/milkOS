#![allow(dead_code)]
//! Page tables and the MMU (4 KiB granule, 4 levels, 48-bit VA).
//!
//! * [`AddressSpace`] is a page-table tree that can map/unmap/translate. It is
//!   not tied to the kernel's own tables, so per-process spaces are just more
//!   `AddressSpace::new` calls (see docs/memory.md).
//! * [`init`] builds the boot identity map in a small static pool of tables
//!   (no allocator, no locks: it runs with the MMU off) and turns the MMU and
//!   caches on.
//! * Every physical/virtual conversion goes through [`phys_to_virt`] and
//!   [`virt_to_phys`], the only place that knows the kernel is identity mapped.

use crate::pmm;
use crate::sync::SpinLock;
use core::cell::UnsafeCell;

pub type PhysAddr = usize;
pub type VirtAddr = usize;

pub const PAGE_SIZE: usize = 4096;
const ENTRIES: usize = 512;
/// Highest VA bit we use: 48-bit address space per TTBR.
const VA_BITS: u32 = 48;

/// Offset between a physical address and its kernel virtual address. 0 while
/// RAM is identity mapped; a higher-half kernel changes only this (plus the
/// mapping set up in `init`).
const PHYS_OFFSET: usize = 0;

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
    va - PHYS_OFFSET
}

// ---- layout of the 48-bit TTBR0 space -------------------------------------
//
// The kernel stays identity mapped and lives in the low L0 slots. Every user address space
// shares those slots (same L1 tables, EL1-only, global) and owns the rest. See
// docs/processes.md.

/// L0 slots `[0, KERNEL_L0_SLOTS)` (512 GiB each) belong to the kernel: the identity map
/// (devices + RAM), high PCIe MMIO and the kernel-stack window.
pub const KERNEL_L0_SLOTS: usize = 2;
/// First user virtual address (1 TiB).
pub const USER_BASE: VirtAddr = KERNEL_L0_SLOTS << 39;
/// One past the last user virtual address (top of the 48-bit TTBR0 range).
pub const USER_END: VirtAddr = 1 << VA_BITS;
/// Kernel-virtual window for things that need guard pages (kernel stacks). Inside L0 slot 0,
/// above any RAM qemu virt can have, below high PCIe MMIO (512 GiB).
pub const VMAP_BASE: VirtAddr = 0x7f_0000_0000;
pub const VMAP_END: VirtAddr = 0x80_0000_0000;

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
        pmm::alloc_frame_zeroed()
    }
}

/// A tree of page tables. Dropping does not free the tables (kernel space is
/// forever); a process space would add a `destroy` that walks and frees.
pub struct AddressSpace {
    root: PhysAddr,
}

const fn level_shift(level: usize) -> u32 {
    39 - 9 * level as u32
}

fn table_ptr(pa: PhysAddr) -> *mut u64 {
    phys_to_virt(pa) as *mut u64
}

impl AddressSpace {
    /// An empty address space with a fresh root table.
    pub fn new(alloc: &mut dyn TableAlloc) -> Option<AddressSpace> {
        Some(AddressSpace {
            root: alloc.alloc_table()?,
        })
    }

    /// Wrap an existing, already-zeroed root table.
    pub fn from_root(root: PhysAddr) -> AddressSpace {
        AddressSpace { root }
    }

    /// Physical address of the root table (what goes in TTBRn_EL1).
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    /// A fresh user address space: a new root whose kernel slots point at the kernel's own
    /// L1 tables (so kernel mappings, current and future, are shared), and nothing else.
    /// Call [`prepare_kernel_slots`] once before the first one.
    pub fn new_user() -> Option<AddressSpace> {
        let root = pmm::alloc_frame_zeroed()?;
        let kroot = with_kernel_space(|k| k.root);
        for slot in 0..KERNEL_L0_SLOTS {
            // SAFETY: both are live root tables; slot < 512.
            unsafe {
                let d = table_ptr(kroot).add(slot).read_volatile();
                table_ptr(root).add(slot).write_volatile(d);
            }
        }
        Some(AddressSpace { root })
    }

    /// Free the page-table pages of a space made by [`new_user`](Self::new_user): every
    /// table under the user slots, and the root. The shared kernel tables are left alone.
    /// Leaf frames are NOT freed.
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
        for slot in KERNEL_L0_SLOTS..ENTRIES {
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
        if len == 0
            || !va.is_multiple_of(PAGE_SIZE)
            || !pa.is_multiple_of(PAGE_SIZE)
            || !len.is_multiple_of(PAGE_SIZE)
            || va.checked_add(len).is_none_or(|e| e > 1 << VA_BITS)
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
        if va >= 1 << VA_BITS {
            return None;
        }
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
        if !va.is_multiple_of(PAGE_SIZE) || va >= 1 << VA_BITS {
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

/// Number of page-table pages available before the frame allocator exists:
/// L0 + L1 + a couple of L2/L3 for RAM edges.
const BOOT_TABLES: usize = 8;

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

/// Everything below this is device space on qemu virt (UART, GIC, virtio-mmio,
/// fw_cfg, PCIe low MMIO window, flash).
const DEVICE_END: usize = 0x4000_0000;
const BLOCK_2M: usize = 2 << 20;

/// Build the boot identity map and turn on the MMU, I-cache and D-cache.
///
/// `[0, DEVICE_END)` becomes Device-nGnRE; `[ram_base, ram_base+ram_size)`
/// (rounded up to 2 MiB) becomes Normal write-back cacheable.
///
/// # Safety
/// Call once, on core 0, with the MMU off, before anything uses atomics.
pub unsafe fn init(ram_base: PhysAddr, ram_size: usize) {
    let mut pool = BootTables { used: 0 };
    let mut space = AddressSpace::new(&mut pool).expect("boot table pool empty");
    space
        .map(0, 0, DEVICE_END, MapAttrs::KERNEL_DEVICE, &mut pool)
        .expect("map device region");
    let ram_end = (ram_base + ram_size).next_multiple_of(BLOCK_2M);
    space
        .map(
            ram_base,
            ram_base,
            ram_end - ram_base,
            MapAttrs::KERNEL_RAM,
            &mut pool,
        )
        .expect("map RAM");

    // SAFETY: identity map covers this code, the stack and the UART, so the
    // instruction after the SCTLR write is fetched from the same address.
    unsafe { enable(space.root()) };

    *KERNEL_SPACE.lock() = Some(space);
}

/// Run `f` on the kernel's address space.
pub fn with_kernel_space<R>(f: impl FnOnce(&mut AddressSpace) -> R) -> R {
    let mut guard = KERNEL_SPACE.lock();
    f(guard.as_mut().expect("MMU not initialised"))
}

/// Make sure every kernel L0 slot has an L1 table, so mappings the kernel adds later land in
/// tables that user spaces already share. Call before creating the first user space.
pub fn prepare_kernel_slots() {
    with_kernel_space(|k| {
        for slot in 0..KERNEL_L0_SLOTS {
            // SAFETY: the kernel root is live; we only fill empty slots with a fresh table.
            unsafe {
                let e = table_ptr(k.root).add(slot);
                if e.read_volatile() & DESC_VALID == 0 {
                    let t = pmm::alloc_frame_zeroed().expect("no frame for kernel L1 table");
                    e.write_volatile(t as u64 | DESC_TABLE_OR_PAGE | DESC_VALID);
                }
            }
        }
        // SAFETY: barrier only.
        unsafe { core::arch::asm!("dsb ishst", "isb", options(nostack)) };
    });
}

/// TTBR0 value for kernel-only threads: the kernel's own root, ASID 0.
pub fn kernel_ttbr0() -> u64 {
    with_kernel_space(|k| k.root as u64)
}

/// Map an MMIO range into the kernel address space as Device memory.
/// Everything below 1 GiB is already mapped; this is for PCIe high windows etc.
/// Returns the virtual address (identity for now).
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
const SCTLR_WXN: u64 = 1 << 19; // writable => execute-never (off: no W^X yet)

const TCR_T0SZ: u64 = 64 - VA_BITS as u64;
const TCR_IRGN0_WBWA: u64 = 0b01 << 8;
const TCR_ORGN0_WBWA: u64 = 0b01 << 10;
const TCR_SH0_INNER: u64 = 0b11 << 12;
const TCR_TG0_4K: u64 = 0b00 << 14;
const TCR_T1SZ: u64 = (64 - VA_BITS as u64) << 16;
const TCR_EPD1: u64 = 1 << 23; // TTBR1 walks disabled until we have a higher half
const TCR_TG1_4K: u64 = 0b10 << 30;

unsafe fn enable(root: PhysAddr) {
    // Physical address size: whatever the CPU supports, capped at 48 bits (0b101).
    let mmfr0: u64;
    // SAFETY: read-only system register.
    unsafe {
        core::arch::asm!("mrs {0}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack))
    };
    let ips = (mmfr0 & 0xf).min(0b101);

    let tcr = TCR_T0SZ
        | TCR_IRGN0_WBWA
        | TCR_ORGN0_WBWA
        | TCR_SH0_INNER
        | TCR_TG0_4K
        | TCR_T1SZ
        | TCR_EPD1
        | TCR_TG1_4K
        | (ips << 32);

    // SAFETY: see `init`. The table walker only sees descriptors we have
    // already written (the dsb below orders them before the SCTLR write).
    unsafe {
        core::arch::asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {root}",
            "msr ttbr1_el1, xzr",
            "isb",
            // Start from a clean TLB and I-cache.
            "tlbi vmalle1",
            "ic iallu",
            "dsb nsh",
            "isb",
            "mrs {tmp}, sctlr_el1",
            "orr {tmp}, {tmp}, {on}",
            "bic {tmp}, {tmp}, {off}",
            "dsb sy",
            "msr sctlr_el1, {tmp}",
            "isb",
            mair = in(reg) MAIR_VALUE,
            tcr = in(reg) tcr,
            root = in(reg) root as u64,
            tmp = out(reg) _,
            on = in(reg) SCTLR_M | SCTLR_C | SCTLR_SA | SCTLR_I,
            off = in(reg) SCTLR_A | SCTLR_WXN,
            options(nostack)
        );
    }
}

/// True if SCTLR_EL1 reports MMU, D-cache and I-cache all enabled.
pub fn caches_on() -> bool {
    let sctlr: u64;
    // SAFETY: read-only system register.
    unsafe { core::arch::asm!("mrs {0}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    sctlr & (SCTLR_M | SCTLR_C | SCTLR_I) == SCTLR_M | SCTLR_C | SCTLR_I
}
