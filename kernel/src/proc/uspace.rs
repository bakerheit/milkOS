//! A user process's address space: page tables + ASID + the frames it owns, plus brk,
//! anonymous mmap and a stack that grows on demand.
//!
//! The kernel never dereferences user pointers directly: every access goes through the
//! page map here and the kernel's identity map of the frame (`phys_to_virt`), so a bad
//! user pointer is an `EFAULT`, never a kernel data abort, and PAN/TTBR0 state don't matter.
//!
//! Layout (see docs/processes.md):
//! ```text
//! 0x0000_0000_0000 .. USER_BASE         kernel (identity map etc., EL1 only, shared)
//! USER_BASE + 0x40_0000 (IMAGE_BASE)    program image (user/rt/user.ld links here)
//!   ... brk heap right after the image's last segment
//! MMAP_BASE = USER_BASE + 1 TiB          anonymous mmap, bump allocated upwards
//! STACK_TOP - STACK_MAX .. STACK_TOP     stack (STACK_INIT mapped at exec, grows on fault)
//! ```

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::asid::Asid;
use super::errno::*;
use crate::mmu::{
    AddressSpace, MapAttrs, MemType, PAGE_SIZE, PhysAddr, PmmTables, USER_BASE, USER_END, VirtAddr,
    phys_to_virt,
};
use crate::pmm;
use crate::sync::SpinLock;

pub const IMAGE_BASE: VirtAddr = USER_BASE + 0x40_0000;
pub const MMAP_BASE: VirtAddr = USER_BASE + (1 << 40);
pub const STACK_TOP: VirtAddr = USER_END - 0x1_0000;
pub const STACK_MAX: usize = 8 << 20;
pub const STACK_INIT: usize = 64 << 10;
/// brk may grow at most this far past the image.
pub const BRK_MAX: usize = 1 << 30;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Perm {
    pub write: bool,
    pub exec: bool,
}

impl Perm {
    pub const R: Perm = Perm {
        write: false,
        exec: false,
    };
    pub const RW: Perm = Perm {
        write: true,
        exec: false,
    };
    pub const RX: Perm = Perm {
        write: false,
        exec: true,
    };
    pub const RWX: Perm = Perm {
        write: true,
        exec: true,
    };

    fn attrs(self) -> MapAttrs {
        MapAttrs {
            mem: MemType::Normal,
            write: self.write,
            exec: self.exec,
            user: true,
        }
    }

    fn union(self, o: Perm) -> Perm {
        Perm {
            write: self.write || o.write,
            exec: self.exec || o.exec,
        }
    }
}

/// What a region of the address space is (for /proc/<pid>/maps).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Image,
    Heap,
    Stack,
    Anon,
}

pub struct Region {
    pub start: VirtAddr,
    pub end: VirtAddr,
    pub perm: Perm,
    pub kind: RegionKind,
}

#[derive(Clone, Copy)]
struct Page {
    pa: PhysAddr,
    /// What the program asked for.
    perm: Perm,
    /// Someone else's frame mapped in (a desktop window buffer, see `map_shared`): never
    /// freed or copy-on-write shared by this space.
    foreign: bool,
    /// Copy-on-write: logically writable but mapped read-only until the first write.
    cow: bool,
}

impl Page {
    fn new(pa: PhysAddr, perm: Perm) -> Page {
        Page {
            pa,
            perm,
            cow: false,
            foreign: false,
        }
    }

    /// The hardware mapping: like `perm`, but read-only while copy-on-write.
    fn attrs(&self) -> MapAttrs {
        Perm {
            write: self.perm.write && !self.cow,
            exec: self.perm.exec,
        }
        .attrs()
    }
}

// ---- shared frames (copy-on-write) -------------------------------------------------------

/// Frames with more than one owner: frame -> number of owners beyond the first. A frame
/// that is not in here has exactly one owner.
static SHARED: SpinLock<BTreeMap<PhysAddr, u32>> = SpinLock::new(BTreeMap::new());

fn share_frame(pa: PhysAddr) {
    *SHARED.lock().entry(pa).or_insert(0) += 1;
}

/// Drop one owner. True if it was the last one (the caller frees the frame).
fn release_frame(pa: PhysAddr) -> bool {
    let mut m = SHARED.lock();
    match m.get_mut(&pa) {
        Some(c) => {
            *c -= 1;
            if *c == 0 {
                m.remove(&pa);
            }
            false
        }
        None => true,
    }
}

fn frame_shared(pa: PhysAddr) -> bool {
    SHARED.lock().contains_key(&pa)
}

/// Number of frames currently shared between address spaces.
pub fn shared_frames() -> usize {
    SHARED.lock().len()
}

pub struct UserSpace {
    space: Option<AddressSpace>,
    asid: Asid,
    pages: BTreeMap<VirtAddr, Page>,
    pub brk_start: VirtAddr,
    pub brk: VirtAddr,
    mmap_next: VirtAddr,
    /// Lowest mapped stack page.
    stack_low: VirtAddr,
}

fn page_down(va: usize) -> usize {
    va & !(PAGE_SIZE - 1)
}

fn page_up(va: usize) -> usize {
    va.next_multiple_of(PAGE_SIZE)
}

/// `[va, va+len)` lies inside the user range.
pub fn user_range_ok(va: usize, len: usize) -> bool {
    va >= USER_BASE && va.checked_add(len).is_some_and(|e| e <= USER_END)
}

impl UserSpace {
    pub fn new() -> KResult<UserSpace> {
        let asid = Asid::alloc().ok_or(EAGAIN)?;
        let space = AddressSpace::new_user().ok_or(ENOMEM)?;
        Ok(UserSpace {
            space: Some(space),
            asid,
            pages: BTreeMap::new(),
            brk_start: 0,
            brk: 0,
            mmap_next: MMAP_BASE,
            stack_low: STACK_TOP,
        })
    }

    fn space(&mut self) -> &mut AddressSpace {
        self.space.as_mut().unwrap()
    }

    /// TTBR0_EL1 value for this space.
    pub fn ttbr0(&self) -> u64 {
        self.space.as_ref().unwrap().root() as u64 | self.asid.ttbr_bits()
    }

    pub fn asid(&self) -> u16 {
        self.asid.get()
    }

    /// Bytes of user memory mapped.
    pub fn resident(&self) -> usize {
        self.pages.len() * PAGE_SIZE
    }

    fn kind_of(&self, va: VirtAddr) -> RegionKind {
        if va >= STACK_TOP - STACK_MAX {
            RegionKind::Stack
        } else if va >= MMAP_BASE {
            RegionKind::Anon
        } else if va >= self.brk_start && self.brk_start != 0 {
            RegionKind::Heap
        } else {
            RegionKind::Image
        }
    }

    /// Mapped pages merged into runs of the same permissions and kind.
    pub fn regions(&self) -> Vec<Region> {
        let mut out: Vec<Region> = Vec::new();
        for (&va, p) in &self.pages {
            let kind = self.kind_of(va);
            if let Some(last) = out.last_mut()
                && last.end == va
                && last.perm == p.perm
                && last.kind == kind
            {
                last.end += PAGE_SIZE;
                continue;
            }
            out.push(Region {
                start: va,
                end: va + PAGE_SIZE,
                perm: p.perm,
                kind,
            });
        }
        out
    }

    /// Map a fresh zeroed page at `va` (page aligned). If one is already there, widen its
    /// permissions to include `perm` and keep its contents (overlapping ELF segments).
    pub fn map_page(&mut self, va: VirtAddr, perm: Perm) -> KResult<PhysAddr> {
        debug_assert!(va.is_multiple_of(PAGE_SIZE));
        if !user_range_ok(va, PAGE_SIZE) {
            return Err(EINVAL);
        }
        if let Some(mut p) = self.pages.get(&va).copied() {
            let want = p.perm.union(perm);
            if want != p.perm {
                p.perm = want;
                self.remap(va, p)?;
            }
            return Ok(p.pa);
        }
        let pa = pmm::alloc_frame_zeroed().ok_or(ENOMEM)?;
        if self
            .space()
            .map(va, pa, PAGE_SIZE, perm.attrs(), &mut PmmTables)
            .is_err()
        {
            pmm::free_frame(pa);
            return Err(ENOMEM);
        }
        self.pages.insert(va, Page::new(pa, perm));
        Ok(pa)
    }

    /// Replace the mapping of page `va` with `p` (frame and/or permissions).
    fn remap(&mut self, va: VirtAddr, p: Page) -> KResult<()> {
        self.space().unmap_page(va);
        self.space()
            .map(va, p.pa, PAGE_SIZE, p.attrs(), &mut PmmTables)
            .map_err(|_| ENOMEM)?;
        self.pages.insert(va, p);
        Ok(())
    }

    /// A write fault at `va`: if it hit a copy-on-write page, give this space its own
    /// writable copy (or just make it writable if nobody else shares the frame any more)
    /// and return true.
    pub fn cow_fault(&mut self, va: VirtAddr) -> bool {
        let pg = page_down(va);
        let Some(mut p) = self.pages.get(&pg).copied() else {
            return false;
        };
        if !p.cow || !p.perm.write {
            return false;
        }
        if frame_shared(p.pa) {
            let Some(new) = pmm::alloc_frame() else {
                return false;
            };
            // SAFETY: two distinct whole frames.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    phys_to_virt(p.pa) as *const u8,
                    phys_to_virt(new) as *mut u8,
                    PAGE_SIZE,
                )
            };
            // SMP: the other owner may have copied and let go meanwhile; if we turn out to
            // be the last owner, the original is ours to free.
            if release_frame(p.pa) {
                pmm::free_frame(p.pa);
            }
            p.pa = new;
        }
        p.cow = false;
        if self.remap(pg, p).is_err() {
            return false;
        }
        if p.perm.exec {
            self.sync_icache();
        }
        true
    }

    /// Break copy-on-write on every page of `[va, va+len)` (before the kernel writes there
    /// through its own mapping, which ignores the read-only bit).
    fn unshare_range(&mut self, va: VirtAddr, len: usize) {
        if len == 0 || !user_range_ok(va, len) {
            return;
        }
        for pg in (page_down(va)..va + len).step_by(PAGE_SIZE) {
            if self.pages.get(&pg).is_some_and(|p| p.cow) {
                self.cow_fault(pg);
            }
        }
    }

    /// Map `[va, va+len)` (rounded out to pages) with zeroed pages.
    pub fn map_range(&mut self, va: VirtAddr, len: usize, perm: Perm) -> KResult<()> {
        let (start, end) = (page_down(va), page_up(va + len));
        for p in (start..end).step_by(PAGE_SIZE) {
            self.map_page(p, perm)?;
        }
        Ok(())
    }

    /// Unmap and free every page in `[va, va+len)` (rounded out). Holes are fine.
    pub fn unmap_range(&mut self, va: VirtAddr, len: usize) {
        let (start, end) = (page_down(va), page_up(va + len));
        let doomed: Vec<VirtAddr> = self.pages.range(start..end).map(|(&v, _)| v).collect();
        for v in doomed {
            let p = self.pages.remove(&v).unwrap();
            self.space().unmap_page(v);
            if !p.foreign && release_frame(p.pa) {
                pmm::free_frame(p.pa);
            }
        }
    }

    /// Visit `[va, va+len)` page by page as kernel pointers, after checking the whole range
    /// is mapped (and writable, if `write`), so a failure never leaves a partial copy.
    fn chunks(
        &self,
        va: VirtAddr,
        len: usize,
        write: bool,
        mut f: impl FnMut(*mut u8, usize, usize),
    ) -> KResult<()> {
        if len == 0 {
            return Ok(());
        }
        if !user_range_ok(va, len) {
            return Err(EFAULT);
        }
        for p in (page_down(va)..va + len).step_by(PAGE_SIZE) {
            match self.pages.get(&p) {
                Some(pg) if !write || pg.perm.write => {}
                _ => return Err(EFAULT),
            }
        }
        let mut done = 0;
        while done < len {
            let cur = va + done;
            let off = cur % PAGE_SIZE;
            let n = (PAGE_SIZE - off).min(len - done);
            let pa = self.pages[&page_down(cur)].pa;
            f((phys_to_virt(pa) + off) as *mut u8, done, n);
            done += n;
        }
        Ok(())
    }

    /// Copy kernel data out to user memory (needs write permission).
    pub fn copy_out(&mut self, va: VirtAddr, data: &[u8]) -> KResult<()> {
        self.unshare_range(va, data.len());
        self.chunks(va, data.len(), true, |k, off, n| unsafe {
            // SAFETY: k..k+n is inside a frame this space owns.
            core::ptr::copy_nonoverlapping(data.as_ptr().add(off), k, n)
        })
    }

    /// Write into user memory regardless of page permissions (loader, stack setup).
    pub fn poke(&self, va: VirtAddr, data: &[u8]) -> KResult<()> {
        self.chunks(va, data.len(), false, |k, off, n| unsafe {
            // SAFETY: as above.
            core::ptr::copy_nonoverlapping(data.as_ptr().add(off), k, n)
        })
    }

    /// Copy user memory into a kernel buffer.
    pub fn copy_in(&self, va: VirtAddr, buf: &mut [u8]) -> KResult<()> {
        let dst = buf.as_mut_ptr();
        self.chunks(va, buf.len(), false, |k, off, n| unsafe {
            // SAFETY: as above.
            core::ptr::copy_nonoverlapping(k, dst.add(off), n)
        })
    }

    pub fn read_u64(&self, va: VirtAddr) -> KResult<u64> {
        let mut b = [0u8; 8];
        self.copy_in(va, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Read a NUL-terminated string of at most `max` bytes.
    pub fn read_cstr(&self, va: VirtAddr, max: usize) -> KResult<String> {
        let mut out = Vec::new();
        let mut cur = va;
        loop {
            if out.len() >= max {
                return Err(ENAMETOOLONG);
            }
            let pg = page_down(cur);
            let p = self.pages.get(&pg).ok_or(EFAULT)?;
            if !user_range_ok(cur, 1) {
                return Err(EFAULT);
            }
            // SAFETY: inside a frame this space owns.
            let b = unsafe { *((phys_to_virt(p.pa) + cur - pg) as *const u8) };
            if b == 0 {
                break;
            }
            out.push(b);
            cur += 1;
        }
        String::from_utf8(out).map_err(|_| EINVAL)
    }

    /// Read a NULL-terminated array of string pointers (argv / envp).
    pub fn read_str_array(&self, va: VirtAddr, max_items: usize) -> KResult<Vec<String>> {
        let mut v = Vec::new();
        if va == 0 {
            return Ok(v);
        }
        loop {
            let p = self.read_u64(va + v.len() * 8)? as usize;
            if p == 0 {
                return Ok(v);
            }
            if v.len() == max_items {
                return Err(E2BIG);
            }
            v.push(self.read_cstr(p, 4096)?);
        }
    }

    /// Make instruction fetches see code written through the kernel's data-side alias:
    /// clean the exec pages to the point of unification, then drop the I-cache.
    pub fn sync_icache(&self) {
        let ctr: u64;
        // SAFETY: read-only register.
        unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
        let line = 4usize << ((ctr >> 16) & 0xf); // DminLine, in bytes
        for p in self.pages.values().filter(|p| p.perm.exec) {
            let k = phys_to_virt(p.pa);
            for a in (k..k + PAGE_SIZE).step_by(line) {
                // SAFETY: cache maintenance on memory we own.
                unsafe { core::arch::asm!("dc cvau, {}", in(reg) a, options(nostack)) };
            }
        }
        // SAFETY: barriers + I-cache invalidate (inner shareable, all of it).
        unsafe { core::arch::asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack)) };
    }

    /// A copy of this space, copy-on-write: both spaces share every frame; writable pages
    /// become read-only in both until one of them writes (then `cow_fault` copies).
    pub fn fork(&mut self) -> KResult<UserSpace> {
        let mut n = UserSpace::new()?;
        n.brk_start = self.brk_start;
        n.brk = self.brk;
        n.mmap_next = self.mmap_next;
        n.stack_low = self.stack_low;
        let pages: Vec<(VirtAddr, Page)> = self.pages.iter().map(|(&v, &p)| (v, p)).collect();
        for (va, mut p) in pages {
            if p.foreign {
                // A desktop window buffer: the child gets its own private snapshot (the
                // window stays the parent's), and the parent's mapping is left alone.
                let pa = n.map_page(va, p.perm)?;
                // SAFETY: two distinct whole frames.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        phys_to_virt(p.pa) as *const u8,
                        phys_to_virt(pa) as *mut u8,
                        PAGE_SIZE,
                    )
                };
                continue;
            }
            if p.perm.write && !p.cow {
                p.cow = true;
                self.remap(va, p)?; // parent goes read-only too
            }
            if n.space()
                .map(va, p.pa, PAGE_SIZE, p.attrs(), &mut PmmTables)
                .is_err()
            {
                return Err(ENOMEM); // `n` drops: releases what it already shares
            }
            share_frame(p.pa);
            n.pages.insert(va, p);
        }
        Ok(n)
    }

    // ---- stack, brk, mmap -----------------------------------------------------------

    /// Map the initial stack; returns its top.
    pub fn map_stack(&mut self) -> KResult<VirtAddr> {
        self.map_range(STACK_TOP - STACK_INIT, STACK_INIT, Perm::RW)?;
        self.stack_low = STACK_TOP - STACK_INIT;
        Ok(STACK_TOP)
    }

    /// A fault at `va`: if it is just below the stack (within STACK_MAX), grow the stack
    /// down to it and return true.
    pub fn grow_stack(&mut self, va: VirtAddr) -> bool {
        let floor = STACK_TOP - STACK_MAX;
        if va < floor || va >= self.stack_low {
            return false;
        }
        let new_low = page_down(va);
        if self
            .map_range(new_low, self.stack_low - new_low, Perm::RW)
            .is_err()
        {
            return false;
        }
        self.stack_low = new_low;
        true
    }

    /// Set the end of the image; brk starts there.
    pub fn set_brk_start(&mut self, end: VirtAddr) {
        self.brk_start = page_up(end);
        self.brk = self.brk_start;
    }

    /// Linux brk semantics: returns the new break, or the old one if `new` is refused.
    pub fn set_brk(&mut self, new: VirtAddr) -> VirtAddr {
        if new < self.brk_start || new > self.brk_start + BRK_MAX {
            return self.brk;
        }
        let (old_end, new_end) = (page_up(self.brk), page_up(new));
        if new_end > old_end {
            if self
                .map_range(old_end, new_end - old_end, Perm::RW)
                .is_err()
            {
                self.unmap_range(old_end, new_end - old_end);
                return self.brk;
            }
        } else if new_end < old_end {
            self.unmap_range(new_end, old_end - new_end);
        }
        self.brk = new;
        new
    }

    /// Anonymous private mapping. `fixed`: exactly at `addr` (replacing what is there).
    pub fn mmap_anon(
        &mut self,
        addr: VirtAddr,
        len: usize,
        perm: Option<Perm>,
        fixed: bool,
    ) -> KResult<VirtAddr> {
        if len == 0 {
            return Err(EINVAL);
        }
        let len = page_up(len);
        let va = if fixed {
            if !addr.is_multiple_of(PAGE_SIZE) || !user_range_ok(addr, len) {
                return Err(EINVAL);
            }
            self.unmap_range(addr, len);
            addr
        } else {
            let va = self.mmap_next;
            if va + len > STACK_TOP - STACK_MAX {
                return Err(ENOMEM);
            }
            self.mmap_next = va + len + PAGE_SIZE; // leave a hole between mappings
            va
        };
        // PROT_NONE: reserve the range (nothing mapped, so any access faults).
        if let Some(perm) = perm
            && let Err(e) = self.map_range(va, len, perm)
        {
            self.unmap_range(va, len);
            return Err(e);
        }
        Ok(va)
    }

    /// mprotect: set the permissions of the mapped pages in the range. PROT_NONE (`None`)
    /// is accepted but leaves the pages as they are (no "mapped but inaccessible" yet).
    pub fn protect(&mut self, addr: VirtAddr, len: usize, perm: Option<Perm>) -> KResult<()> {
        if !addr.is_multiple_of(PAGE_SIZE) || !user_range_ok(addr, len) {
            return Err(EINVAL);
        }
        let Some(perm) = perm else { return Ok(()) };
        let pages: Vec<(VirtAddr, Page)> = self
            .pages
            .range(addr..page_up(addr + len))
            .map(|(&v, &p)| (v, p))
            .collect();
        for (va, mut p) in pages {
            if p.perm == perm {
                continue;
            }
            p.perm = perm;
            self.remap(va, p)?;
            if perm.exec {
                self.sync_icache();
            }
        }
        Ok(())
    }

    /// Map `len` bytes of physically contiguous memory at `pa` that this space does not own
    /// (a desktop window's pixel buffer), read/write, at a fresh address in the mmap area.
    /// The owner keeps the frames alive until the process has unmapped them or exited.
    pub fn map_shared(&mut self, pa: PhysAddr, len: usize) -> KResult<VirtAddr> {
        let len = page_up(len);
        let va = self.mmap_next;
        if len == 0 || va + len > STACK_TOP - STACK_MAX {
            return Err(ENOMEM);
        }
        self.mmap_next = va + len + PAGE_SIZE;
        for off in (0..len).step_by(PAGE_SIZE) {
            if self
                .space()
                .map(
                    va + off,
                    pa + off,
                    PAGE_SIZE,
                    Perm::RW.attrs(),
                    &mut PmmTables,
                )
                .is_err()
            {
                self.unmap_range(va, off);
                return Err(ENOMEM);
            }
            self.pages.insert(
                va + off,
                Page {
                    pa: pa + off,
                    perm: Perm::RW,
                    cow: false,
                    foreign: true,
                },
            );
        }
        Ok(va)
    }

    /// Unmap every shared page that maps physical `[pa, pa+len)` (whatever its address).
    pub fn unmap_shared(&mut self, pa: PhysAddr, len: usize) {
        let doomed: Vec<VirtAddr> = self
            .pages
            .iter()
            .filter(|(_, p)| p.foreign && p.pa >= pa && p.pa < pa + len)
            .map(|(&v, _)| v)
            .collect();
        for v in doomed {
            self.pages.remove(&v);
            self.space().unmap_page(v);
        }
    }

    pub fn munmap(&mut self, addr: VirtAddr, len: usize) -> KResult<()> {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 || !user_range_ok(addr, len) {
            return Err(EINVAL);
        }
        self.unmap_range(addr, len);
        Ok(())
    }
}

impl Drop for UserSpace {
    /// The space must no longer be live in TTBR0 (exit/exec switch away first).
    fn drop(&mut self) {
        // Flush this ASID before its frames can be reused (Asid's own drop flushes again).
        // SAFETY: TLB maintenance only.
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi aside1is, {0}",
                "dsb ish",
                in(reg) self.asid.ttbr_bits(),
                options(nostack)
            );
        }
        for p in self.pages.values() {
            if !p.foreign && release_frame(p.pa) {
                pmm::free_frame(p.pa);
            }
        }
        if let Some(space) = self.space.take() {
            // SAFETY: not live (caller's contract); tables came from the PMM.
            unsafe { space.destroy_user() };
        }
    }
}
