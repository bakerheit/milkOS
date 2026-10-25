//! PCI(e) enumeration: find the devices behind a host bridge, size and place their BARs, open
//! their memory space and bus mastering. Config space access is a trait ([`ConfigSpace`]): qemu
//! virt's is a flat ECAM window ([`Ecam`]), the Raspberry Pi 4's root complex is
//! `rpi/pcie.rs`'s. What the numbers mean is `rpikit::pci`.
//!
//! The only consumers are the USB host controllers, which poll: no MSI, no INTx routing here.
//! Only memory BARs are placed (I/O space is not used by anything we drive). Bridges get a
//! secondary bus number and a memory window sized by what is behind them.

use alloc::boxed::Box;
use alloc::vec::Vec;
#[cfg(not(feature = "raspi"))]
use core::ptr::{read_volatile, write_volatile};

use rpikit::pci::{self, BarKind, Window, cfg};

use crate::kprintln;

/// Bus / device / function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bdf {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

/// Access to a host bridge's configuration space (32-bit aligned accesses only).
pub trait ConfigSpace: Send + Sync {
    fn read(&self, bdf: Bdf, off: u16) -> u32;
    fn write(&self, bdf: Bdf, off: u16, v: u32);
}

#[cfg(not(feature = "raspi"))]
/// A flat ECAM window: config space of (bus, dev, func) at `bus << 20 | dev << 15 | func << 12`.
pub struct Ecam {
    /// Kernel virtual address of the window's first byte (bus `bus_start`).
    pub base: usize,
    pub bus_start: u8,
    pub bus_end: u8,
}

#[cfg(not(feature = "raspi"))]
impl Ecam {
    fn addr(&self, bdf: Bdf, off: u16) -> Option<usize> {
        if bdf.bus < self.bus_start
            || bdf.bus > self.bus_end
            || bdf.dev > 31
            || bdf.func > 7
            || off > 0xffc
        {
            return None;
        }
        let bus = (bdf.bus - self.bus_start) as usize;
        Some(
            self.base
                + (bus << 20 | (bdf.dev as usize) << 15 | (bdf.func as usize) << 12 | off as usize),
        )
    }
}

#[cfg(not(feature = "raspi"))]
impl ConfigSpace for Ecam {
    fn read(&self, bdf: Bdf, off: u16) -> u32 {
        match self.addr(bdf, off & !3) {
            // SAFETY: inside the mapped ECAM window.
            Some(a) => unsafe { read_volatile(a as *const u32) },
            None => !0,
        }
    }
    fn write(&self, bdf: Bdf, off: u16, v: u32) {
        if let Some(a) = self.addr(bdf, off & !3) {
            // SAFETY: as above.
            unsafe { write_volatile(a as *mut u32, v) }
        }
    }
}

/// One memory BAR that was sized and given an address.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bar {
    /// PCI bus address.
    pub pci_addr: u64,
    pub size: u64,
    /// What was written to the BAR's register (low dword with the type bits) and, for a 64-bit BAR, the
    /// high dword: to put the BAR back if the device forgets it (a reset).
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub lo: u32,
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub hi: u32,
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub is64: bool,
}

/// A device found by [`enumerate`].
pub struct Device {
    pub bdf: Bdf,
    pub id: u32,
    pub class: u32,
    pub bars: [Bar; 6],
}

impl Device {
    pub fn is_xhci(&self) -> bool {
        pci::is_xhci(self.class)
    }
}

/// The host bridge: config access plus the translation from PCI bus addresses of its memory window to
/// CPU physical addresses.
pub struct Host {
    pub cfg: Box<dyn ConfigSpace>,
    /// The memory window in PCI addresses, and where the CPU sees it.
    pub window_pci: u64,
    pub window_cpu: u64,
    pub window_size: u64,
    /// First bus number below the root (the host bridge's own bus is `first_bus`).
    pub first_bus: u8,
    pub last_bus: u8,
}

/// PCI bus address -> CPU physical address for the host bridge's memory window.
#[derive(Clone, Copy)]
pub struct Translate {
    window_pci: u64,
    window_cpu: u64,
}

impl Translate {
    pub fn cpu_addr(&self, pci_addr: u64) -> u64 {
        pci_addr - self.window_pci + self.window_cpu
    }
}

impl Host {
    pub fn translate(&self) -> Translate {
        Translate {
            window_pci: self.window_pci,
            window_cpu: self.window_cpu,
        }
    }

    pub fn cpu_addr(&self, pci_addr: u64) -> u64 {
        self.translate().cpu_addr(pci_addr)
    }
}

struct Scan<'a> {
    host: &'a Host,
    mem: Window,
    next_bus: u8,
    found: Vec<Device>,
    /// Bridges seen, parents before their children.
    bridges: Vec<Bdf>,
    /// Leave the command registers alone (the caller turns decoding on when it is ready).
    defer: bool,
}

const MAX_DEVICES: usize = 32;

impl Scan<'_> {
    fn rd(&self, bdf: Bdf, off: u16) -> u32 {
        self.host.cfg.read(bdf, off)
    }
    fn wr(&self, bdf: Bdf, off: u16, v: u32) {
        self.host.cfg.write(bdf, off, v)
    }

    fn scan_bus(&mut self, bus: u8) {
        for dev in 0..32u8 {
            let f0 = Bdf { bus, dev, func: 0 };
            let id = self.rd(f0, cfg::ID);
            if !pci::present(id) {
                continue;
            }
            let (_, multi) = pci::header_type(self.rd(f0, cfg::HEADER));
            for func in 0..if multi { 8 } else { 1 } {
                let bdf = Bdf { bus, dev, func };
                let id = self.rd(bdf, cfg::ID);
                if !pci::present(id) {
                    continue;
                }
                self.probe(bdf, id);
                if self.found.len() >= MAX_DEVICES {
                    return;
                }
            }
        }
    }

    fn probe(&mut self, bdf: Bdf, id: u32) {
        let class = self.rd(bdf, cfg::CLASS);
        let (kind, _) = pci::header_type(self.rd(bdf, cfg::HEADER));
        match kind {
            0 => self.endpoint(bdf, id, class),
            1 => self.bridge(bdf),
            _ => {}
        }
    }

    fn endpoint(&mut self, bdf: Bdf, id: u32, class: u32) {
        let mut bars = [Bar::default(); 6];
        let mut i = 0;
        while i < 6 {
            let off = cfg::BAR0 + 4 * i as u16;
            let old = self.rd(bdf, off);
            self.wr(bdf, off, !0);
            let mask = self.rd(bdf, off);
            self.wr(bdf, off, old);
            match pci::bar_kind(mask) {
                BarKind::Mem32 { .. } => {
                    let size = pci::bar_size(mask, 0);
                    if let Some(a) = self.mem.alloc(size, size.max(4096)) {
                        self.wr(bdf, off, a as u32);
                        bars[i] = Bar {
                            pci_addr: a,
                            size,
                            lo: a as u32,
                            ..Bar::default()
                        };
                    }
                }
                BarKind::Mem64 { .. } => {
                    let hoff = off + 4;
                    let hold = self.rd(bdf, hoff);
                    self.wr(bdf, hoff, !0);
                    let hi = self.rd(bdf, hoff);
                    self.wr(bdf, hoff, hold);
                    let size = pci::bar_size(mask, hi);
                    // Placed below 4 GiB in our window, so the high half is 0.
                    if let Some(a) = self.mem.alloc(size, size.max(4096)) {
                        let lo = a as u32 | (mask & 0xf);
                        self.wr(bdf, off, lo);
                        self.wr(bdf, hoff, (a >> 32) as u32);
                        bars[i] = Bar {
                            pci_addr: a,
                            size,
                            lo,
                            hi: (a >> 32) as u32,
                            is64: true,
                        };
                    }
                    i += 1; // the next BAR is this one's high half
                }
                _ => {}
            }
            i += 1;
        }
        if !self.defer {
            // Memory decode and bus mastering on.
            set_command(self.host, bdf, true, true);
        }
        self.found.push(Device {
            bdf,
            id,
            class,
            bars,
        });
    }

    fn bridge(&mut self, bdf: Bdf) {
        let secondary = self.next_bus;
        if secondary == 0xff || secondary > self.host.last_bus {
            return;
        }
        self.next_bus += 1;
        self.bridges.push(bdf);
        // primary, secondary, subordinate (all the buses that can be below, for the scan).
        self.wr(
            bdf,
            cfg::BUSES,
            (self.rd(bdf, cfg::BUSES) & 0xff00_0000)
                | 0xff << 16
                | (secondary as u32) << 8
                | bdf.bus as u32,
        );
        // The memory window starts on a megabyte boundary; what is placed behind the bridge goes in it.
        self.mem.align_to(1 << 20);
        let base = self.mem.used_until();
        self.scan_bus(secondary);
        self.mem.align_to(1 << 20);
        let end = self.mem.used_until();
        let subordinate = self.next_bus - 1;
        self.wr(
            bdf,
            cfg::BUSES,
            (self.rd(bdf, cfg::BUSES) & 0xff00_0000)
                | (subordinate as u32) << 16
                | (secondary as u32) << 8
                | bdf.bus as u32,
        );
        let win = pci::bridge_mem_window(base, end).unwrap_or(pci::BRIDGE_WINDOW_CLOSED);
        self.wr(bdf, cfg::MEM_WINDOW, win);
        // No prefetchable window.
        self.wr(bdf, cfg::PREFETCH_WINDOW, pci::BRIDGE_WINDOW_CLOSED);
        self.wr(bdf, cfg::PREFETCH_BASE_HI, 0);
        self.wr(bdf, cfg::PREFETCH_LIMIT_HI, 0);
        if !self.defer {
            set_command(self.host, bdf, true, true);
        }
    }
}

/// Set the memory-decode and bus-master bits of `bdf`'s command register to exactly `memory` / `master`
/// (the status half of the dword is written as zeros: its bits clear on a 1).
pub fn set_command(host: &Host, bdf: Bdf, memory: bool, master: bool) {
    let cmd = host.cfg.read(bdf, cfg::COMMAND) & 0xffff & !(pci::CMD_MEMORY | pci::CMD_BUS_MASTER);
    let cmd = cmd
        | if memory { pci::CMD_MEMORY } else { 0 }
        | if master { pci::CMD_BUS_MASTER } else { 0 };
    host.cfg.write(bdf, cfg::COMMAND, cmd);
}

/// What [`enumerate_deferred`] found: the devices, and the bridges that lead to them (parents first).
#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
pub struct Found {
    pub devices: Vec<Device>,
    pub bridges: Vec<Bdf>,
}

#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
impl Found {
    /// Memory decode and bus mastering on or off for the bridges (parents first) and then the devices.
    pub fn set_commands(&self, host: &Host, memory: bool, master: bool) {
        for b in &self.bridges {
            set_command(host, *b, memory, master);
        }
        for d in &self.devices {
            set_command(host, d.bdf, memory, master);
        }
    }
}

/// Put `d`'s BARs back as [`enumerate`] placed them if they read differently now. Returns whether anything
/// had to be rewritten (a device that was reset forgets its BARs).
#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
pub fn restore_bars(host: &Host, d: &Device) -> bool {
    let mut changed = false;
    for (i, b) in d.bars.iter().enumerate().filter(|(_, b)| b.size != 0) {
        let off = cfg::BAR0 + 4 * i as u16;
        if host.cfg.read(d.bdf, off) != b.lo
            || (b.is64 && host.cfg.read(d.bdf, off + 4) != b.hi)
        {
            host.cfg.write(d.bdf, off, b.lo);
            if b.is64 {
                host.cfg.write(d.bdf, off + 4, b.hi);
            }
            changed = true;
        }
    }
    changed
}

/// Enumerate everything behind `host`: bus numbers, BARs, command registers. Logs what it finds.
#[cfg(not(feature = "raspi"))]
pub fn enumerate(host: &Host) -> Vec<Device> {
    scan(host, false).devices
}

/// [`enumerate`] without touching any command register: the caller turns decoding on with
/// [`Found::set_commands`] when it is ready (the Pi must make a firmware call first).
#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
pub fn enumerate_deferred(host: &Host) -> Found {
    scan(host, true)
}

fn scan(host: &Host, defer: bool) -> Found {
    let mut s = Scan {
        host,
        mem: Window::new(host.window_pci, host.window_size),
        next_bus: host.first_bus + 1,
        found: Vec::new(),
        bridges: Vec::new(),
        defer,
    };
    s.scan_bus(host.first_bus);
    let verbose = crate::platform::RASPI;
    for d in s.found.iter().filter(|d| verbose || d.is_xhci()) {
        kprintln!(
            "[pci] {:02x}:{:02x}.{} {:04x}:{:04x} class {:06x}{}",
            d.bdf.bus,
            d.bdf.dev,
            d.bdf.func,
            pci::vendor(d.id),
            pci::device(d.id),
            d.class >> 8,
            if d.is_xhci() {
                " (xHCI USB controller)"
            } else {
                ""
            }
        );
        for (i, b) in d.bars.iter().enumerate().filter(|(_, b)| b.size != 0) {
            kprintln!(
                "[pci]   BAR{i}: {:#x} (CPU {:#x}) {} KiB",
                b.pci_addr,
                host.cpu_addr(b.pci_addr),
                b.size >> 10
            );
        }
    }
    Found {
        devices: s.found,
        bridges: s.bridges,
    }
}

/// A kernel virtual address for `len` bytes of device registers at CPU physical `pa`: through the boot
/// linear map when the board's device ranges cover it, else mapped now.
pub fn mmio(pa: u64, len: usize) -> Option<usize> {
    let pa = usize::try_from(pa).ok()?;
    let covered = crate::platform::CURRENT
        .device
        .iter()
        .any(|&(s, e)| pa >= s && pa + len <= e);
    if covered {
        return Some(crate::mmu::phys_to_virt(pa));
    }
    // Page-granular: map the pages around it (a second BAR in the same page is mapped once).
    let start = pa & !0xfff;
    let end = (pa + len).next_multiple_of(0x1000);
    match crate::mmu::map_device(start, end - start) {
        Ok(_) | Err(crate::mmu::MapError::AlreadyMapped) => Some(crate::mmu::phys_to_virt(pa)),
        Err(_) => None,
    }
}

#[cfg(not(feature = "raspi"))]
/// qemu virt's host bridge, from the device tree (`pci-host-ecam-generic`: reg = ECAM, ranges, bus-range).
pub fn virt_host() -> Option<Host> {
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) }?;
    let reg = dtb.find_prefix(b"pcie@", b"reg")?;
    let ranges = dtb.find_prefix(b"pcie@", b"ranges")?;
    let be32 = |b: &[u8], i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    if reg.len() < 16 {
        return None;
    }
    let ecam_pa = (be32(reg, 0) as u64) << 32 | be32(reg, 4) as u64;
    let ecam_len = (be32(reg, 8) as u64) << 32 | be32(reg, 12) as u64;
    let (first_bus, last_bus) = match dtb.find_prefix(b"pcie@", b"bus-range") {
        Some(b) if b.len() >= 8 => (be32(b, 0) as u8, be32(b, 4) as u8),
        _ => (0, 0xff),
    };
    // Config space for the buses we may use only.
    let buses = (last_bus as u64 - first_bus as u64 + 1)
        .min(ecam_len >> 20)
        .min(16);
    let base = mmio(ecam_pa, (buses << 20) as usize)?;
    let mut r = [pci::Range {
        space: 0,
        prefetch: false,
        pci_addr: 0,
        cpu_addr: 0,
        size: 0,
    }; pci::MAX_RANGES];
    let n = pci::parse_ranges(ranges, 2, 2, &mut r);
    let w = pci::memory_window(&r[..n])?;
    Some(Host {
        cfg: Box::new(Ecam {
            base,
            bus_start: first_bus,
            bus_end: first_bus + (buses - 1) as u8,
        }),
        window_pci: w.pci_addr,
        window_cpu: w.cpu_addr,
        window_size: w.size,
        first_bus,
        last_bus: first_bus + (buses - 1) as u8,
    })
}
