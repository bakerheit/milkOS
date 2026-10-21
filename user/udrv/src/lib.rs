//! The userland side of user-space drivers (docs/drivers.md).
//!
//! * [`Device`]: a claimed virtio-mmio device. Its registers (through the kernel, bounded
//!   to the device's window), its DMA memory and its interrupt (the device fd: `wait`
//!   blocks for it, `arm` unmasks it again).
//! * [`Device::init`] / [`Device::setup_queue`] / [`Device::driver_ok`]: virtio 1.x
//!   initialisation (3.1.1).
//! * [`Queue`]: a split virtqueue in one DMA page, the same layout as the kernel's
//!   `virtio::queue::QueueMem` (64 entries at most).

#![no_std]

extern crate alloc;

use alloc::string::String;
use core::ptr::{read_volatile, write_volatile};
use rt::sys::{self, Errno};

pub type Res<T> = Result<T, String>;

// virtio-mmio registers (4.2.2).
pub const DEVICE_ID: usize = 0x008;
pub const DEVICE_FEATURES: usize = 0x010;
pub const DEVICE_FEATURES_SEL: usize = 0x014;
pub const DRIVER_FEATURES: usize = 0x020;
pub const DRIVER_FEATURES_SEL: usize = 0x024;
pub const QUEUE_SEL: usize = 0x030;
pub const QUEUE_NUM_MAX: usize = 0x034;
pub const QUEUE_NUM: usize = 0x038;
pub const QUEUE_READY: usize = 0x044;
pub const QUEUE_NOTIFY: usize = 0x050;
pub const INTERRUPT_STATUS: usize = 0x060;
pub const INTERRUPT_ACK: usize = 0x064;
pub const STATUS: usize = 0x070;
pub const QUEUE_DESC_LOW: usize = 0x080;
pub const QUEUE_DESC_HIGH: usize = 0x084;
pub const QUEUE_DRIVER_LOW: usize = 0x090;
pub const QUEUE_DRIVER_HIGH: usize = 0x094;
pub const QUEUE_DEVICE_LOW: usize = 0x0a0;
pub const QUEUE_DEVICE_HIGH: usize = 0x0a4;
pub const CONFIG: usize = 0x100;

pub const S_ACKNOWLEDGE: u32 = 1;
pub const S_DRIVER: u32 = 2;
pub const S_DRIVER_OK: u32 = 4;
pub const S_FEATURES_OK: u32 = 8;
pub const S_FAILED: u32 = 128;
/// VIRTIO_F_VERSION_1.
pub const F_VERSION_1: u64 = 1 << 32;

pub const VIRTIO_RNG: u32 = 4;
pub const VIRTIO_SOUND: u32 = 25;

fn err(what: &str, e: Errno) -> String {
    alloc::format!("{what}: {e}")
}

/// A device claimed with `MILK_DRIVER`.
pub struct Device {
    pub fd: i32,
}

impl Device {
    /// Claim the first device with this virtio id. The kernel driver lets go of it.
    pub fn claim(id: u32) -> Res<Device> {
        sys::driver_claim(id)
            .map(|fd| Device { fd })
            .map_err(|e| err("claim", e))
    }

    pub fn r32(&self, off: usize) -> u32 {
        sys::driver_read(self.fd, off, 4).unwrap_or(0)
    }
    pub fn w32(&self, off: usize, v: u32) -> Res<()> {
        sys::driver_write(self.fd, off, v, 4).map_err(|e| err(&alloc::format!("write {off:#x}"), e))
    }
    pub fn config_r8(&self, off: usize) -> u8 {
        sys::driver_read(self.fd, CONFIG + off, 1).unwrap_or(0) as u8
    }
    pub fn config_r32(&self, off: usize) -> u32 {
        sys::driver_read(self.fd, CONFIG + off, 4).unwrap_or(0)
    }

    /// Zeroed DMA memory: (virtual, physical).
    pub fn dma(&self, bytes: usize) -> Res<(usize, u64)> {
        sys::driver_dma(self.fd, bytes).map_err(|e| err("dma", e))
    }

    /// Block until the device interrupts; returns how many times it did.
    pub fn wait(&self) -> Res<u64> {
        let mut b = [0u8; 8];
        match sys::read(self.fd, &mut b) {
            Ok(8) => Ok(u64::from_le_bytes(b)),
            Ok(_) => Err(String::from("short interrupt read")),
            Err(e) => Err(err("wait", e)),
        }
    }

    /// Acknowledge the interrupt at the device and unmask it. Returns InterruptStatus.
    pub fn ack_and_arm(&self) -> u32 {
        let st = self.r32(INTERRUPT_STATUS);
        let _ = self.w32(INTERRUPT_ACK, st);
        let _ = sys::driver_arm(self.fd);
        st
    }

    pub fn info(&self) -> String {
        sys::driver_info(self.fd)
    }

    /// Reset, ACKNOWLEDGE | DRIVER, negotiate (VERSION_1 + `wanted` & offered), FEATURES_OK.
    pub fn init(&self, wanted: u64) -> Res<u64> {
        self.w32(STATUS, 0)?;
        while self.r32(STATUS) != 0 {}
        self.w32(STATUS, S_ACKNOWLEDGE)?;
        self.w32(STATUS, S_ACKNOWLEDGE | S_DRIVER)?;
        let mut offered = 0u64;
        for sel in 0..2u32 {
            self.w32(DEVICE_FEATURES_SEL, sel)?;
            offered |= (self.r32(DEVICE_FEATURES) as u64) << (32 * sel);
        }
        if offered & F_VERSION_1 == 0 {
            return Err(String::from("not a virtio 1.x device"));
        }
        let take = (wanted & offered) | F_VERSION_1;
        for sel in 0..2u32 {
            self.w32(DRIVER_FEATURES_SEL, sel)?;
            self.w32(DRIVER_FEATURES, (take >> (32 * sel)) as u32)?;
        }
        self.w32(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK)?;
        if self.r32(STATUS) & S_FEATURES_OK == 0 {
            self.w32(STATUS, S_FAILED)?;
            return Err(String::from("the device refused our features"));
        }
        Ok(take)
    }

    pub fn queue_max(&self, idx: u16) -> u16 {
        let _ = self.w32(QUEUE_SEL, idx as u32);
        self.r32(QUEUE_NUM_MAX) as u16
    }

    /// Give queue `idx` to the device.
    pub fn setup_queue(&self, idx: u16, q: &Queue) -> Res<()> {
        let (d, a, u) = q.addresses();
        self.w32(QUEUE_SEL, idx as u32)?;
        self.w32(QUEUE_NUM, q.size as u32)?;
        self.w32(QUEUE_DESC_LOW, d as u32)?;
        self.w32(QUEUE_DESC_HIGH, (d >> 32) as u32)?;
        self.w32(QUEUE_DRIVER_LOW, a as u32)?;
        self.w32(QUEUE_DRIVER_HIGH, (a >> 32) as u32)?;
        self.w32(QUEUE_DEVICE_LOW, u as u32)?;
        self.w32(QUEUE_DEVICE_HIGH, (u >> 32) as u32)?;
        self.w32(QUEUE_READY, 1)
    }

    pub fn driver_ok(&self) -> Res<()> {
        let s = self.r32(STATUS);
        self.w32(STATUS, s | S_DRIVER_OK)
    }

    pub fn notify(&self, idx: u16) -> Res<()> {
        mb();
        self.w32(QUEUE_NOTIFY, idx as u32)
    }
}

/// Order ring stores before the doorbell (and loads after the device's index).
#[inline]
pub fn mb() {
    // SAFETY: a barrier; allowed at EL0.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

// ---- split virtqueue -------------------------------------------------------------------------

pub const MAX_QUEUE: usize = 64;
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;
const USED_F_NO_NOTIFY: u16 = 1;
// Layout inside the page (as the kernel's QueueMem): 64 descriptors x 16 bytes, the
// available ring (flags, idx, ring[64], used_event), then the used ring at a 4-byte
// boundary (flags, idx, ring[64] x 8 bytes, avail_event).
const AVAIL: usize = 16 * MAX_QUEUE;
const USED: usize = (AVAIL + 4 + 2 * MAX_QUEUE + 2 + 3) & !3;

/// A split virtqueue (2.7) in one zeroed DMA page.
pub struct Queue {
    va: usize,
    pa: u64,
    pub size: u16,
    free_head: u16,
    num_free: u16,
    avail_idx: u16,
    last_used: u16,
}

impl Queue {
    /// A queue of `size` (a power of two <= 64) in fresh DMA memory of `dev`.
    pub fn new(dev: &Device, size: u16) -> Res<Queue> {
        if !size.is_power_of_two() || size as usize > MAX_QUEUE {
            return Err(String::from("bad queue size"));
        }
        let (va, pa) = dev.dma(4096)?;
        let mut q = Queue {
            va,
            pa,
            size,
            free_head: 0,
            num_free: size,
            avail_idx: 0,
            last_used: 0,
        };
        for i in 0..size {
            q.write_desc(i, 0, 0, 0, (i + 1) % size);
        }
        Ok(q)
    }

    /// The device's view: physical addresses of the descriptor table, available and used
    /// rings.
    pub fn addresses(&self) -> (u64, u64, u64) {
        (self.pa, self.pa + AVAIL as u64, self.pa + USED as u64)
    }

    fn p<T>(&self, off: usize) -> *mut T {
        (self.va + off) as *mut T
    }

    fn write_desc(&mut self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let o = 16 * i as usize;
        // SAFETY: inside our DMA page, naturally aligned.
        unsafe {
            write_volatile(self.p::<u64>(o), addr);
            write_volatile(self.p::<u32>(o + 8), len);
            write_volatile(self.p::<u16>(o + 12), flags);
            write_volatile(self.p::<u16>(o + 14), next);
        }
    }

    fn desc_flags_next(&self, i: u16) -> (u16, u16) {
        let o = 16 * i as usize;
        // SAFETY: as above.
        unsafe {
            (
                read_volatile(self.p::<u16>(o + 12)),
                read_volatile(self.p::<u16>(o + 14)),
            )
        }
    }

    /// Write descriptor `i` directly, outside the free list and the available ring (tests:
    /// plant a bad address the kernel must refuse at notify; the device never sees it).
    pub fn set_desc(&mut self, i: u16, addr: u64, len: u32) {
        let (flags, next) = self.desc_flags_next(i);
        self.write_desc(i, addr, len, flags, next);
    }

    pub fn num_free(&self) -> u16 {
        self.num_free
    }

    /// Post a chain: `out` buffers (device reads) then `inb` (device writes), each
    /// (physical address, length). Returns the head. Call [`Device::notify`] after.
    pub fn add(&mut self, out: &[(u64, u32)], inb: &[(u64, u32)]) -> Option<u16> {
        let n = out.len() + inb.len();
        if n == 0 || n > self.num_free as usize {
            return None;
        }
        let head = self.free_head;
        let mut cur = head;
        for (k, &(addr, len)) in out.iter().chain(inb.iter()).enumerate() {
            let (_, next) = self.desc_flags_next(cur);
            let mut flags = if k >= out.len() { DESC_F_WRITE } else { 0 };
            if k + 1 < n {
                flags |= DESC_F_NEXT;
            }
            self.write_desc(cur, addr, len, flags, next);
            if k + 1 < n {
                cur = next;
            } else {
                self.free_head = next;
            }
        }
        self.num_free -= n as u16;
        let slot = (self.avail_idx % self.size) as usize;
        // SAFETY: ring entry inside our page.
        unsafe { write_volatile(self.p::<u16>(AVAIL + 4 + 2 * slot), head) };
        self.avail_idx = self.avail_idx.wrapping_add(1);
        mb();
        unsafe { write_volatile(self.p::<u16>(AVAIL + 2), self.avail_idx) };
        Some(head)
    }

    pub fn should_notify(&self) -> bool {
        mb();
        // SAFETY: inside our page.
        unsafe { read_volatile(self.p::<u16>(USED)) & USED_F_NO_NOTIFY == 0 }
    }

    /// Next completed chain: (head, bytes the device wrote).
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        // SAFETY: inside our page.
        let idx = unsafe { read_volatile(self.p::<u16>(USED + 2)) };
        if idx == self.last_used {
            return None;
        }
        mb();
        let slot = (self.last_used % self.size) as usize;
        let e = USED + 4 + 8 * slot;
        let (id, len) = unsafe {
            (
                read_volatile(self.p::<u32>(e)),
                read_volatile(self.p::<u32>(e + 4)),
            )
        };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id as u16, len))
    }

    /// Give a chain's descriptors back (zeroed, so the kernel's notify check skips them).
    pub fn free(&mut self, head: u16) {
        let mut cur = head;
        let mut n = 1;
        loop {
            let (flags, next) = self.desc_flags_next(cur);
            if flags & DESC_F_NEXT == 0 {
                self.write_desc(cur, 0, 0, 0, self.free_head);
                break;
            }
            self.write_desc(cur, 0, 0, 0, next);
            cur = next;
            n += 1;
        }
        self.free_head = head;
        self.num_free += n;
    }
}
