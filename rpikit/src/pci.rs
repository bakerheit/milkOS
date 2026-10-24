//! The pure parts of PCI(e) enumeration: config space fields, BAR sizing, address window
//! allocation, bridge window registers, and a device tree host bridge's `ranges`.
//!
//! The kernel (`kernel/src/pci.rs`) reads and writes config space; what the numbers mean, and where
//! a BAR may go, is decided here, under host tests.

/// Config space offsets (type 0 and type 1 headers).
pub mod cfg {
    pub const ID: u16 = 0x00;
    pub const COMMAND: u16 = 0x04;
    pub const CLASS: u16 = 0x08;
    pub const HEADER: u16 = 0x0c;
    pub const BAR0: u16 = 0x10;
    /// Type 1 (bridge): primary/secondary/subordinate bus numbers.
    pub const BUSES: u16 = 0x18;
    /// Type 1: memory base (15:4 = address bits 31:20) and limit.
    pub const MEM_WINDOW: u16 = 0x20;
    pub const PREFETCH_WINDOW: u16 = 0x24;
    pub const PREFETCH_BASE_HI: u16 = 0x28;
    pub const PREFETCH_LIMIT_HI: u16 = 0x2c;
}

pub const CMD_MEMORY: u32 = 1 << 1;
pub const CMD_BUS_MASTER: u32 = 1 << 2;

/// Is there a device? (vendor 0xffff = nothing answered).
pub const fn present(id: u32) -> bool {
    id & 0xffff != 0xffff && id != 0
}

pub const fn vendor(id: u32) -> u16 {
    id as u16
}
pub const fn device(id: u32) -> u16 {
    (id >> 16) as u16
}

/// Class code, subclass, programming interface from the dword at [`cfg::CLASS`].
pub const fn class_codes(v: u32) -> (u8, u8, u8) {
    ((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8)
}

/// Header type (0 endpoint, 1 bridge) and the multifunction bit, from the dword at [`cfg::HEADER`].
pub const fn header_type(v: u32) -> (u8, bool) {
    (((v >> 16) & 0x7f) as u8, (v >> 16) & 0x80 != 0)
}

/// The class of an xHCI USB host controller.
pub const fn is_xhci(v: u32) -> bool {
    let (c, s, p) = class_codes(v);
    c == 0x0c && s == 0x03 && p == 0x30
}

/// What a BAR is, from the value read back after writing all ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    /// Unimplemented (reads 0).
    None,
    Io,
    Mem32 {
        prefetch: bool,
    },
    /// 64-bit memory: uses this BAR and the next.
    Mem64 {
        prefetch: bool,
    },
}

pub const fn bar_kind(mask: u32) -> BarKind {
    if mask == 0 {
        return BarKind::None;
    }
    if mask & 1 != 0 {
        return BarKind::Io;
    }
    let prefetch = mask & 8 != 0;
    match (mask >> 1) & 3 {
        2 => BarKind::Mem64 { prefetch },
        _ => BarKind::Mem32 { prefetch },
    }
}

/// Size in bytes of a memory BAR from the low dword read back after writing all ones (and, for a 64-bit BAR,
/// the high dword, `hi`). 0 if the BAR is unusable.
pub fn bar_size(mask: u32, hi: u32) -> u64 {
    match bar_kind(mask) {
        BarKind::Mem32 { .. } => {
            let m = mask & !0xf;
            if m == 0 {
                0
            } else {
                (!m).wrapping_add(1) as u64
            }
        }
        BarKind::Mem64 { .. } => {
            let m = (mask & !0xf) as u64 | (hi as u64) << 32;
            if m == 0 { 0 } else { (!m).wrapping_add(1) }
        }
        _ => 0,
    }
}

/// A bump allocator over an address window (PCI addresses).
#[derive(Clone, Copy, Debug)]
pub struct Window {
    next: u64,
    end: u64,
}

impl Window {
    pub const fn new(base: u64, size: u64) -> Window {
        Window {
            next: base,
            end: base.saturating_add(size),
        }
    }

    /// `size` bytes aligned to `align` (a power of two; a BAR aligns to its size, at least 4 KiB).
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        let a = self.next.checked_next_multiple_of(align)?;
        let e = a.checked_add(size)?;
        if e > self.end {
            return None;
        }
        self.next = e;
        Some(a)
    }

    /// Round the next allocation up to `align`, as a bridge's window start.
    pub fn align_to(&mut self, align: u64) {
        if let Some(a) = self.next.checked_next_multiple_of(align) {
            self.next = a.min(self.end);
        }
    }

    pub fn used_until(&self) -> u64 {
        self.next
    }
}

/// The dword for a bridge's memory window registers ([`cfg::MEM_WINDOW`]) covering `[base, end)`,
/// both multiples of 1 MiB with `end > base`: base in bits 15:4, limit (the last MiB's start) in 31:20.
/// `None` if they do not fit 32 bits or are not aligned (the window stays closed).
pub fn bridge_mem_window(base: u64, end: u64) -> Option<u32> {
    if end <= base || !base.is_multiple_of(1 << 20) || !end.is_multiple_of(1 << 20) || end > 1 << 32
    {
        return None;
    }
    let limit = end - (1 << 20);
    Some((limit as u32 & 0xfff0_0000) | ((base as u32) >> 16 & 0xfff0))
}

/// The dword that closes a bridge's memory window (base above limit).
pub const BRIDGE_WINDOW_CLOSED: u32 = 0x0000_fff0;

/// One entry of a host bridge's `ranges`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    /// 1 = I/O, 2 = 32-bit memory, 3 = 64-bit memory.
    pub space: u8,
    pub prefetch: bool,
    pub pci_addr: u64,
    pub cpu_addr: u64,
    pub size: u64,
}

pub const MAX_RANGES: usize = 8;

/// Parse a `ranges` property with 3 child (PCI) address cells, `parent_cells` parent address cells (1 or 2) and
/// `size_cells` size cells (1 or 2). Returns the count stored in `out`.
pub fn parse_ranges(
    data: &[u8],
    parent_cells: usize,
    size_cells: usize,
    out: &mut [Range; MAX_RANGES],
) -> usize {
    if !(1..=2).contains(&parent_cells) || !(1..=2).contains(&size_cells) {
        return 0;
    }
    let entry = (3 + parent_cells + size_cells) * 4;
    let cell =
        |off: usize| u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    let cells = |off: usize, n: usize| (0..n).fold(0u64, |a, i| a << 32 | cell(off + i * 4) as u64);
    let mut n = 0;
    let mut o = 0;
    while o + entry <= data.len() && n < MAX_RANGES {
        let hi = cell(o);
        out[n] = Range {
            space: ((hi >> 24) & 3) as u8,
            prefetch: hi & (1 << 30) != 0,
            pci_addr: cells(o + 4, 2),
            cpu_addr: cells(o + 12, parent_cells),
            size: cells(o + 12 + parent_cells * 4, size_cells),
        };
        n += 1;
        o += entry;
    }
    n
}

/// The first 32-bit (or, failing that, 64-bit) memory range, as the window BARs are placed in.
pub fn memory_window(ranges: &[Range]) -> Option<Range> {
    ranges
        .iter()
        .find(|r| r.space == 2 && r.size > 0)
        .or_else(|| {
            ranges
                .iter()
                .find(|r| r.space == 3 && r.size > 0 && r.pci_addr < 1 << 32)
        })
        .copied()
}
