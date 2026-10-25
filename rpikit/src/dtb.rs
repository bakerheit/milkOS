#![allow(dead_code)]
//! Minimal flattened device tree (FDT) reader.
//!
//! Only what early boot needs: validate the header, report the blob's size, and find the
//! `/memory` node's `reg` ranges, the CPUs (and how the firmware starts them), the PSCI
//! conduit, the kernel command line and the root `compatible`. It never allocates and runs
//! with the MMU off, so every read is a naturally-aligned load. Everything is bounds-checked
//! against the blob's own sizes: a hostile or truncated tree gives fewer answers, never a
//! read outside it.

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

pub const MAX_BANKS: usize = 32;
/// CPUs [`Dtb::cpus`] reports.
pub const MAX_CPUS: usize = 8;

/// How a CPU is brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnableMethod {
    /// No `enable-method` (or one we do not know): the PSCI node, if any, decides.
    Unknown,
    /// `psci`: a firmware call (hvc or smc, see [`Dtb::psci_conduit`]).
    Psci,
    /// `spin-table`: write the entry point to `cpu-release-addr` and send an event.
    SpinTable,
}

/// One `/cpus/cpu@N` node.
#[derive(Clone, Copy, Debug)]
pub struct Cpu {
    /// `reg`: the MPIDR affinity bits (Aff0 on the Pi).
    pub reg: u64,
    pub method: EnableMethod,
    /// `cpu-release-addr` (spin-table), 0 if absent.
    pub release_addr: u64,
}

/// The firmware-call instruction PSCI uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conduit {
    Hvc,
    Smc,
}

/// Does a `compatible` property (NUL-separated strings) list `wanted` (as a whole string or a part of one)?
pub fn compat_has(compat: &[u8], wanted: &str) -> bool {
    !wanted.is_empty()
        && compat
            .split(|&b| b == 0)
            .any(|s| s.windows(wanted.len()).any(|w| w == wanted.as_bytes()))
}

/// The RAM banks to use out of what the tree listed: sorted by address, rounded in to whole
/// `page`s, nothing at or above `limit`, overlapping or touching banks merged, and only the bank
/// that holds `kstart` and those above it (the allocator covers one span upwards from the
/// kernel). Returns the banks and how many; none if no bank holds `kstart`.
pub fn usable_banks(
    tree: &[MemRange],
    kstart: usize,
    limit: usize,
    page: usize,
) -> ([MemRange; MAX_BANKS], usize) {
    let mut banks = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    let mut n = 0;
    for b in tree {
        let base = b.base.checked_next_multiple_of(page);
        let end = b
            .base
            .checked_add(b.size)
            .map(|e| e.min(limit) / page * page);
        let (Some(base), Some(end)) = (base, end) else {
            continue;
        };
        if end > base && n < MAX_BANKS {
            banks[n] = MemRange {
                base,
                size: end - base,
            };
            n += 1;
        }
    }
    banks[..n].sort_unstable_by_key(|b| b.base);
    // Merge what overlaps or touches.
    let mut m = 0;
    for i in 0..n {
        if m > 0 && banks[i].base <= banks[m - 1].base + banks[m - 1].size {
            let end = (banks[i].base + banks[i].size).max(banks[m - 1].base + banks[m - 1].size);
            banks[m - 1].size = end - banks[m - 1].base;
        } else {
            banks[m] = banks[i];
            m += 1;
        }
    }
    let Some(first) = banks[..m]
        .iter()
        .position(|b| (b.base..b.base + b.size).contains(&kstart))
    else {
        return (banks, 0);
    };
    banks.copy_within(first..m, 0);
    (banks, m - first)
}

/// A `reg` property read as `(address, size)` pairs of `ac` and `sc` 32-bit cells (1 or 2 each);
/// up to `out.len()` pairs. Anything else (0 cells, 3 or more) gives none.
pub fn parse_reg(data: &[u8], ac: usize, sc: usize, out: &mut [(u64, u64)]) -> usize {
    if !(1..=2).contains(&ac) || !(1..=2).contains(&sc) {
        return 0;
    }
    let cell = |at: usize| u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as u64;
    let rd = |at: usize, n: usize| (0..n).fold(0u64, |v, i| (v << 32) | cell(at + 4 * i));
    let entry = (ac + sc) * 4;
    let mut n = 0;
    while (n + 1) * entry <= data.len() && n < out.len() {
        out[n] = (rd(n * entry, ac), rd(n * entry + ac * 4, sc));
        n += 1;
    }
    n
}

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

    /// The first property called `prop` of the node called exactly `node` (any depth), as a
    /// slice of the blob. `None` if absent.
    pub fn find(&self, node: &[u8], prop: &[u8]) -> Option<&'static [u8]> {
        let mut found: Option<&'static [u8]> = None;
        self.for_each_prop(|_parent, n, _depth, p, data| {
            if found.is_none() && n == node && p == prop {
                // SAFETY: `data` is inside the blob, which outlives the kernel's use of it
                // (the caller reserves it); on the host the test keeps the blob alive.
                found = Some(unsafe { core::slice::from_raw_parts(data.as_ptr(), data.len()) });
            }
        });
        found
    }

    /// Like [`Dtb::find`], for the first node whose name starts with `prefix` (`pcie@` matches
    /// `pcie@10000000`).
    pub fn find_prefix(&self, prefix: &[u8], prop: &[u8]) -> Option<&'static [u8]> {
        let mut found: Option<&'static [u8]> = None;
        self.for_each_prop(|_parent, n, depth, p, data| {
            if found.is_none() && depth >= 2 && n.starts_with(prefix) && p == prop {
                // SAFETY: as in `find`.
                found = Some(unsafe { core::slice::from_raw_parts(data.as_ptr(), data.len()) });
            }
        });
        found
    }

    /// Has the first node whose name starts with `prefix` been switched off (`status` other than
    /// `okay`)? qemu does this to the nodes of hardware it does not model (a real firmware's tree
    /// says `okay` or nothing).
    pub fn node_disabled(&self, prefix: &[u8]) -> bool {
        self.find_prefix(prefix, b"status")
            .is_some_and(|s| !(s.starts_with(b"okay") || s.starts_with(b"ok\0")))
    }

    /// The root node's `compatible` (NUL-separated strings), empty if absent.
    pub fn root_compatible(&self) -> &'static [u8] {
        let mut found: &'static [u8] = &[];
        let mut seen = false;
        self.for_each_prop(|_parent, _n, depth, p, data| {
            if depth == 1 && p == b"compatible" && !seen {
                seen = true;
                // SAFETY: as in `find`.
                found = unsafe { core::slice::from_raw_parts(data.as_ptr(), data.len()) };
            }
        });
        found
    }

    /// The kernel command line (`/chosen/bootargs`) without its terminating NUL; empty if none.
    pub fn bootargs(&self) -> &'static [u8] {
        let v = self.find(b"chosen", b"bootargs").unwrap_or(&[]);
        match v.iter().position(|&b| b == 0) {
            Some(n) => &v[..n],
            None => v,
        }
    }

    /// The `/cpus/cpu@N` nodes in tree order. Returns how many were stored in `out`.
    pub fn cpus(&self, out: &mut [Cpu; MAX_CPUS]) -> usize {
        let mut n = 0usize;
        let mut last_id = usize::MAX;
        let mut cur: Option<usize> = None;
        self.for_each_prop_id(|parent, node, id, _depth, prop, data| {
            if parent != b"cpus" || !node.starts_with(b"cpu@") {
                return;
            }
            if id != last_id {
                last_id = id;
                cur = if n < MAX_CPUS {
                    out[n] = Cpu {
                        reg: 0,
                        method: EnableMethod::Unknown,
                        release_addr: 0,
                    };
                    n += 1;
                    Some(n - 1)
                } else {
                    None
                };
            }
            let Some(i) = cur else { return };
            let be = |d: &[u8]| d.iter().take(8).fold(0u64, |a, &b| a << 8 | b as u64);
            match prop {
                b"reg" => out[i].reg = be(data),
                b"enable-method" => {
                    out[i].method = if data.starts_with(b"psci") {
                        EnableMethod::Psci
                    } else if data.starts_with(b"spin-table") {
                        EnableMethod::SpinTable
                    } else {
                        EnableMethod::Unknown
                    }
                }
                b"cpu-release-addr" => out[i].release_addr = be(data),
                _ => {}
            }
        });
        n
    }

    /// The `/psci` node's `method`, if the tree has one.
    pub fn psci_conduit(&self) -> Option<Conduit> {
        let m = self.find(b"psci", b"method")?;
        if m.starts_with(b"smc") {
            Some(Conduit::Smc)
        } else if m.starts_with(b"hvc") {
            Some(Conduit::Hvc)
        } else {
            None
        }
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

    /// Visit every property: `f(parent node name, node name, depth, property name, data)`.
    /// Depth 1 is the root. Names are as in the blob (`cpu@1`, `psci`).
    pub fn for_each_prop(&self, mut f: impl FnMut(&[u8], &[u8], usize, &[u8], &[u8])) {
        self.for_each_prop_id(|parent, node, _id, depth, name, data| {
            f(parent, node, depth, name, data)
        });
    }

    /// Like [`Dtb::for_each_prop`], with a node id (the node's offset in the structure block,
    /// different for every node) in front of the depth, so a visitor can tell two nodes of the
    /// same name apart.
    pub fn for_each_prop_id(&self, mut f: impl FnMut(&[u8], &[u8], usize, usize, &[u8], &[u8])) {
        let start = self.base + self.struct_off;
        let end = start + self.struct_size;
        let strings = self.base + self.strings_off;
        let limit = self.base + self.total_size;
        // Node names and ids by depth (bounded; deeper nodes keep the last slot).
        let mut stack: [&[u8]; 16] = [b""; 16];
        let mut ids = [0usize; 16];
        let mut depth = 0usize;
        let mut p = start;
        while p + 4 <= end {
            let node_at = p - start;
            let token = be32(p);
            p += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let name = self.cstr(p, end);
                    p = (p + name.len() + 1 + 3) & !3;
                    depth += 1;
                    stack[depth.min(15)] = name;
                    ids[depth.min(15)] = node_at;
                }
                FDT_END_NODE => depth = depth.saturating_sub(1),
                FDT_PROP => {
                    if p + 8 > end {
                        return;
                    }
                    let len = be32(p) as usize;
                    let name_off = be32(p + 4) as usize;
                    let data = p + 8;
                    if data.checked_add(len).is_none_or(|e| e > end) {
                        return;
                    }
                    p = (data + len + 3) & !3;
                    let name = self.cstr(strings.saturating_add(name_off), limit);
                    // SAFETY: inside the blob (checked above).
                    let bytes = unsafe { core::slice::from_raw_parts(data as *const u8, len) };
                    let parent = if depth >= 2 {
                        stack[(depth - 1).min(15)]
                    } else {
                        b""
                    };
                    f(
                        parent,
                        stack[depth.min(15)],
                        ids[depth.min(15)],
                        depth,
                        name,
                        bytes,
                    );
                }
                FDT_NOP => {}
                _ => return,
            }
        }
    }

    /// The memory reservation block of the header (`/memreserve/` entries): `(address, size)`
    /// pairs, up to the terminating zero entry. Returns how many were stored in `out`.
    pub fn mem_reserve(&self, out: &mut [MemRange]) -> usize {
        let off = be32(self.base + 16) as usize;
        let mut n = 0;
        let mut p = off;
        while p % 8 == 0 && p.checked_add(16).is_some_and(|e| e <= self.total_size) {
            let rd = |at: usize| ((be32(self.base + at) as u64) << 32) | be32(self.base + at + 4) as u64;
            let (a, s) = (rd(p), rd(p + 8));
            if a == 0 && s == 0 {
                break;
            }
            if n < out.len() {
                out[n] = MemRange {
                    base: a as usize,
                    size: s as usize,
                };
                n += 1;
            }
            p += 16;
        }
        n
    }

    /// Like [`Dtb::for_each_prop_id`], with the `#address-cells` / `#size-cells` the node's
    /// *parent* declares (what the node's `reg` is written in): `f(parent name, node name, node
    /// id, depth, (address cells, size cells), property name, data)`. A node that declares
    /// none inherits nothing, as the specification says: 2 address cells and 1 size cell.
    pub fn for_each_prop_cells(
        &self,
        mut f: impl FnMut(&[u8], &[u8], usize, usize, (usize, usize), &[u8], &[u8]),
    ) {
        let mut ids = [0usize; 16];
        let mut cells = [(2usize, 1usize); 16];
        // The cells each node declares for its children, by depth; (2, 1) until it says.
        let mut declared = [(2usize, 1usize); 16];
        let mut stack: [&[u8]; 16] = [b""; 16];
        let mut depth = 0usize;
        let start = self.base + self.struct_off;
        let end = start + self.struct_size;
        let strings = self.base + self.strings_off;
        let limit = self.base + self.total_size;
        let mut p = start;
        while p + 4 <= end {
            let token = be32(p);
            p += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let at = p - 4 - start;
                    let name = self.cstr(p, end);
                    p = (p + name.len() + 1 + 3) & !3;
                    depth += 1;
                    let d = depth.min(15);
                    stack[d] = name;
                    ids[d] = at;
                    // What this node's own `reg` is written in: its parent's declaration.
                    cells[d] = declared[(d - 1).min(15)];
                    declared[d] = (2, 1);
                }
                FDT_END_NODE => depth = depth.saturating_sub(1),
                FDT_PROP => {
                    if p + 8 > end {
                        return;
                    }
                    let len = be32(p) as usize;
                    let name_off = be32(p + 4) as usize;
                    let data = p + 8;
                    if data.checked_add(len).is_none_or(|e| e > end) {
                        return;
                    }
                    p = (data + len + 3) & !3;
                    let name = self.cstr(strings.saturating_add(name_off), limit);
                    // SAFETY: inside the blob (checked above).
                    let bytes = unsafe { core::slice::from_raw_parts(data as *const u8, len) };
                    let d = depth.min(15);
                    if len == 4 && name == b"#address-cells" {
                        declared[d].0 = be32(data) as usize;
                    } else if len == 4 && name == b"#size-cells" {
                        declared[d].1 = be32(data) as usize;
                    }
                    let parent = if depth >= 2 {
                        stack[(depth - 1).min(15)]
                    } else {
                        b""
                    };
                    f(parent, stack[d], ids[d], depth, cells[d], name, bytes);
                }
                FDT_NOP => {}
                _ => return,
            }
        }
    }

    /// The `reg` ranges of the first node whose name starts with `prefix` (at depth 2 or
    /// deeper), read with the cell sizes of its parent. Returns how many `(address, size)` pairs
    /// were stored in `out`. A `reg` with 3 or more cells for an address or a size is skipped.
    pub fn node_reg(&self, prefix: &[u8], out: &mut [(u64, u64)]) -> usize {
        let mut n = 0;
        let mut done = false;
        self.for_each_prop_cells(|_parent, node, _id, depth, (ac, sc), prop, data| {
            if done || depth < 2 || !node.starts_with(prefix) || prop != b"reg" {
                return;
            }
            done = true;
            n = parse_reg(data, ac, sc, out);
        });
        n
    }

    /// The `reg` ranges of the first node whose `compatible` lists `compat` (as a whole string),
    /// read with the cell sizes of its parent: `(address, size)` pairs stored in `out`, how many.
    /// For nodes whose name differs between trees (`interrupt-controller@17a00000` here,
    /// `intc@8000000` on qemu).
    pub fn node_reg_compat(&self, compat: &[u8], out: &mut [(u64, u64)]) -> usize {
        // Pass 1: which node. Pass 2: its reg.
        let mut which = None;
        self.for_each_prop_cells(|_p, _n, id, depth, _c, prop, data| {
            if which.is_none()
                && depth >= 2
                && prop == b"compatible"
                && data.split(|&b| b == 0).any(|s| s == compat)
            {
                which = Some(id);
            }
        });
        let Some(want) = which else { return 0 };
        let mut n = 0;
        self.for_each_prop_cells(|_p, _n, id, _d, (ac, sc), prop, data| {
            if id == want && prop == b"reg" && n == 0 {
                n = parse_reg(data, ac, sc, out);
            }
        });
        n
    }

    /// The fixed regions the tree reserves: the `/memreserve/` entries and every child of
    /// `/reserved-memory` that has a `reg` (the ones with only a `size` are for the operating
    /// system to place, they are not anywhere yet). Returns how many were stored in `out`.
    pub fn reserved_regions(&self, out: &mut [MemRange]) -> usize {
        let mut n = self.mem_reserve(out);
        self.for_each_prop_cells(|parent, _node, _id, depth, (ac, sc), prop, data| {
            if depth != 3 || parent != b"reserved-memory" || prop != b"reg" {
                return;
            }
            let mut tmp = [(0u64, 0u64); 4];
            let k = parse_reg(data, ac, sc, &mut tmp);
            for &(a, s) in &tmp[..k] {
                if s != 0 && n < out.len() {
                    out[n] = MemRange {
                        base: a as usize,
                        size: s as usize,
                    };
                    n += 1;
                }
            }
        });
        n
    }

    /// The name and `reg` of every child of `/reserved-memory` with one, for the log. Calls
    /// `f(name, base, size)`.
    pub fn each_reserved_child(&self, mut f: impl FnMut(&[u8], u64, u64)) {
        self.for_each_prop_cells(|parent, node, _id, depth, (ac, sc), prop, data| {
            if depth != 3 || parent != b"reserved-memory" || prop != b"reg" {
                return;
            }
            let mut tmp = [(0u64, 0u64); 4];
            let k = parse_reg(data, ac, sc, &mut tmp);
            for &(a, s) in &tmp[..k] {
                f(node, a, s);
            }
        });
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
