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
    AddressSpace, MapAttrs, MemType, PAGE_SIZE, PhysAddr, PmmTables, USER_BASE, USER_END,
    VirtAddr, phys_to_virt,
};
use crate::pmm;

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

#[derive(Clone, Copy)]
struct Page {
    pa: PhysAddr,
    perm: Perm,
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

    /// Map a fresh zeroed page at `va` (page aligned). If one is already there, widen its
    /// permissions to include `perm` and keep its contents (overlapping ELF segments).
    pub fn map_page(&mut self, va: VirtAddr, perm: Perm) -> KResult<PhysAddr> {
        debug_assert!(va.is_multiple_of(PAGE_SIZE));
        if !user_range_ok(va, PAGE_SIZE) {
            return Err(EINVAL);
        }
        if let Some(p) = self.pages.get(&va).copied() {
            let want = p.perm.union(perm);
            if want != p.perm {
                self.space().unmap_page(va);
                self.space()
                    .map(va, p.pa, PAGE_SIZE, want.attrs(), &mut PmmTables)
                    .map_err(|_| ENOMEM)?;
                self.pages.insert(
                    va,
                    Page {
                        pa: p.pa,
                        perm: want,
                    },
                );
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
        self.pages.insert(va, Page { pa, perm });
        Ok(pa)
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
            pmm::free_frame(p.pa);
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
    pub fn copy_out(&self, va: VirtAddr, data: &[u8]) -> KResult<()> {
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

    /// A copy of this space: same layout, same contents, new frames and ASID (eager copy,
    /// no copy-on-write yet).
    pub fn fork(&self) -> KResult<UserSpace> {
        let mut n = UserSpace::new()?;
        n.brk_start = self.brk_start;
        n.brk = self.brk;
        n.mmap_next = self.mmap_next;
        n.stack_low = self.stack_low;
        for (&va, p) in &self.pages {
            let pa = n.map_page(va, p.perm)?;
            // SAFETY: two distinct whole frames.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    phys_to_virt(p.pa) as *const u8,
                    phys_to_virt(pa) as *mut u8,
                    PAGE_SIZE,
                )
            };
        }
        n.sync_icache();
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
            pmm::free_frame(p.pa);
        }
        if let Some(space) = self.space.take() {
            // SAFETY: not live (caller's contract); tables came from the PMM.
            unsafe { space.destroy_user() };
        }
    }
}
