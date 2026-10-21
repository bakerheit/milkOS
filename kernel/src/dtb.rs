#![allow(dead_code)]
//! Minimal flattened device tree (FDT) reader.
//!
//! Only what early boot needs: validate the header, report the blob's size, and
//! find the `/memory` node's `reg` ranges. It never allocates and runs with the
//! MMU off, so every read is a naturally-aligned load.

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// A `(base, size)` range of physical memory.
#[derive(Clone, Copy, Debug)]
pub struct MemRange {
    pub base: usize,
    pub size: usize,
}

pub const MAX_BANKS: usize = 4;

pub struct Dtb {
    base: usize,
    total_size: usize,
    struct_off: usize,
    struct_size: usize,
    strings_off: usize,
}

fn be32(addr: usize) -> u32 {
    // SAFETY: callers bounds-check against the blob size; FDT fields are
    // 4-byte aligned relative to the (8-byte aligned) blob start.
    u32::from_be(unsafe { core::ptr::read_volatile(addr as *const u32) })
}

impl Dtb {
    /// Validate and wrap the blob at `addr`.
    ///
    /// # Safety
    /// `addr` must be 0 or point to readable memory.
    pub unsafe fn from_addr(addr: usize) -> Option<Dtb> {
        if addr == 0 || addr % 8 != 0 {
            return None;
        }
        if be32(addr) != FDT_MAGIC {
            return None;
        }
        let total_size = be32(addr + 4) as usize;
        let struct_off = be32(addr + 8) as usize;
        let strings_off = be32(addr + 12) as usize;
        let struct_size = be32(addr + 36) as usize;
        if total_size < 40 || struct_off % 4 != 0 {
            return None;
        }
        if struct_off.checked_add(struct_size)? > total_size || strings_off > total_size {
            return None;
        }
        Some(Dtb {
            base: addr,
            total_size,
            struct_off,
            struct_size,
            strings_off,
        })
    }

    pub fn base(&self) -> usize {
        self.base
    }

    pub fn total_size(&self) -> usize {
        self.total_size
    }

    /// NUL-terminated string at `addr`, as bytes (without the NUL).
    fn cstr(&self, addr: usize, limit: usize) -> &[u8] {
        let mut n = 0;
        // SAFETY: bounded by `limit`, which is inside the blob.
        while addr + n < limit && unsafe { *((addr + n) as *const u8) } != 0 {
            n += 1;
        }
        // SAFETY: the n bytes just scanned are readable.
        unsafe { core::slice::from_raw_parts(addr as *const u8, n) }
    }

    /// Read the `reg` ranges of every top-level `memory` node. Returns how many
    /// ranges were stored in `out`.
    pub fn memory(&self, out: &mut [MemRange; MAX_BANKS]) -> usize {
        let start = self.base + self.struct_off;
        let end = start + self.struct_size;
        let strings = self.base + self.strings_off;
        let limit = self.base + self.total_size;

        // Root cell sizes default to 2/2 if the properties are absent.
        let mut addr_cells = 2usize;
        let mut size_cells = 2usize;
        let mut depth = 0usize;
        let mut in_memory = false;
        let mut count = 0;
        let mut p = start;

        while p + 4 <= end {
            let token = be32(p);
            p += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let name = self.cstr(p, end);
                    p = (p + name.len() + 1 + 3) & !3;
                    depth += 1;
                    in_memory = depth == 2 && (name == b"memory" || name.starts_with(b"memory@"));
                }
                FDT_END_NODE => {
                    depth = depth.saturating_sub(1);
                    in_memory = false;
                }
                FDT_PROP => {
                    if p + 8 > end {
                        return count;
                    }
                    let len = be32(p) as usize;
                    let name_off = be32(p + 4) as usize;
                    let data = p + 8;
                    if data + len > end {
                        return count;
                    }
                    p = (data + len + 3) & !3;
                    let name = self.cstr(strings + name_off, limit);
                    if depth == 1 && name == b"#address-cells" && len == 4 {
                        addr_cells = be32(data) as usize;
                    } else if depth == 1 && name == b"#size-cells" && len == 4 {
                        size_cells = be32(data) as usize;
                    } else if in_memory && name == b"reg" {
                        if !(1..=2).contains(&addr_cells) || !(1..=2).contains(&size_cells) {
                            return count;
                        }
                        let entry = (addr_cells + size_cells) * 4;
                        let mut off = 0;
                        while off + entry <= len && count < MAX_BANKS {
                            let cells = |at: usize, n: usize| -> usize {
                                let mut v = 0usize;
                                for i in 0..n {
                                    v = (v << 32) | be32(data + at + i * 4) as usize;
                                }
                                v
                            };
                            let base = cells(off, addr_cells);
                            let size = cells(off + addr_cells * 4, size_cells);
                            if size != 0 {
                                out[count] = MemRange { base, size };
                                count += 1;
                            }
                            off += entry;
                        }
                    }
                }
                FDT_NOP => {}
                FDT_END => break,
                _ => return count,
            }
        }
        count
    }
}
