//! virtio-blk driver (virtio 1.x, 5.2) implementing [`fs::blockdev::BlockDevice`].
//!
//! One request queue (0), one request in flight. A request is a 3-descriptor chain:
//! header (type, sector) → data (device-readable for writes, device-writable for reads) →
//! status byte. All device-visible memory comes from the PMM: one frame for the queue, one
//! for the header + status, and a 64 KiB bounce buffer that data is copied through (so
//! callers can pass any kernel buffer). Bigger transfers are split.
//!
//! Requests are thread context: the device lock is a `sched::Mutex`, and after kicking the
//! queue the caller sleeps on [`BLK_WQ`] until the used ring moves (5 s timeout). The IRQ
//! handler acknowledges the interrupt and wakes the queue. Before `sched::init` the wait
//! is a `wfi` loop, so early boot (the fs self-test) uses the same path.
//!
//! [`fs::blockdev::BlockDevice`]: crate::fs::blockdev::BlockDevice

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::mmio::{self, Error, Transport};
use super::queue::{MAX_QUEUE_SIZE, QueueMem, Virtqueue};
use crate::exceptions::TrapFrame;
use crate::fs::blockdev::{self, BlockDevice};
use crate::fs::errno::{Errno, Result};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::sched::{Mutex, WaitQueue};
use crate::{gic, kprintln, pmm, timer};

const F_RO: u64 = 1 << 5;
const F_FLUSH: u64 = 1 << 9;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;

const S_OK: u8 = 0;
const S_UNSUPP: u8 = 2;

const SECTOR: usize = 512;
const BOUNCE_PAGES: usize = 16;
const BOUNCE_BYTES: usize = BOUNCE_PAGES * PAGE_SIZE;
const TIMEOUT_NS: u64 = 5_000_000_000;

/// Offsets inside the header frame.
const HDR_OFF: usize = 0;
const STATUS_OFF: usize = 64;

static COUNT: AtomicUsize = AtomicUsize::new(0);
/// Woken by every virtio-blk interrupt; waiters re-check their own used ring.
static BLK_WQ: WaitQueue = WaitQueue::new();

struct Inner {
    t: Transport,
    vq: Virtqueue,
    hdr_pa: PhysAddr,
    bounce_pa: PhysAddr,
}

pub struct VirtioBlk {
    name: String,
    sectors: u64,
    ro: bool,
    can_flush: bool,
    inner: Mutex<Inner>,
}

fn virt(pa: PhysAddr) -> *mut u8 {
    phys_to_virt(pa) as *mut u8
}

impl Inner {
    /// Issue one request and wait for it. `data`: bounce-buffer bytes used (0 for flush).
    fn submit(&mut self, ty: u32, sector: u64, data: usize, device_writes: bool) -> Result<()> {
        let hdr = virt(self.hdr_pa);
        // SAFETY: our own frame; the device is idle (one request at a time, lock held).
        unsafe {
            write_volatile(hdr.add(HDR_OFF) as *mut u32, ty);
            write_volatile(hdr.add(HDR_OFF + 4) as *mut u32, 0);
            write_volatile(hdr.add(HDR_OFF + 8) as *mut u64, sector);
            write_volatile(hdr.add(STATUS_OFF), 0xff);
        }
        let header = (self.hdr_pa as u64 + HDR_OFF as u64, 16u32);
        let status = (self.hdr_pa as u64 + STATUS_OFF as u64, 1u32);
        let buf = (self.bounce_pa as u64, data as u32);
        let head = match (data, device_writes) {
            (0, _) => self.vq.add(&[header], &[status]),
            (_, true) => self.vq.add(&[header], &[buf, status]),
            (_, false) => self.vq.add(&[header, buf], &[status]),
        }
        .ok_or(Errno::EIO)?;
        if self.vq.should_notify() {
            self.t.notify(0);
        }
        let start = timer::now_ns();
        loop {
            let mut done = None;
            let vq = &mut self.vq;
            let left = TIMEOUT_NS.saturating_sub(timer::now_ns() - start);
            BLK_WQ.wait_until_timeout(
                || {
                    if done.is_none() {
                        done = vq.pop_used();
                    }
                    done.is_some()
                },
                left,
            );
            match done {
                Some((id, _len)) => {
                    self.vq.free(id);
                    if id == head {
                        break;
                    }
                }
                None => {
                    kprintln!("[virtio-blk] request timed out (type {ty}, sector {sector})");
                    return Err(Errno::EIO);
                }
            }
        }
        // SAFETY: as above.
        match unsafe { read_volatile(hdr.add(STATUS_OFF)) } {
            S_OK => Ok(()),
            S_UNSUPP => Err(Errno::EOPNOTSUPP),
            _ => Err(Errno::EIO),
        }
    }
}

impl VirtioBlk {
    fn check(&self, lba: u64, len: usize) -> Result<()> {
        if !len.is_multiple_of(SECTOR) {
            return Err(Errno::EINVAL);
        }
        let end = lba
            .checked_add((len / SECTOR) as u64)
            .ok_or(Errno::EINVAL)?;
        if end > self.sectors {
            return Err(Errno::ENXIO);
        }
        Ok(())
    }
}

impl BlockDevice for VirtioBlk {
    fn name(&self) -> &str {
        &self.name
    }
    fn block_size(&self) -> usize {
        SECTOR
    }
    fn num_blocks(&self) -> u64 {
        self.sectors
    }
    fn read_only(&self) -> bool {
        self.ro
    }

    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        self.check(lba, buf.len())?;
        let mut d = self.inner.lock();
        for (i, chunk) in buf.chunks_mut(BOUNCE_BYTES).enumerate() {
            let sector = lba + (i * BOUNCE_BYTES / SECTOR) as u64;
            d.submit(T_IN, sector, chunk.len(), true)?;
            // SAFETY: bounce buffer is BOUNCE_BYTES long and ours.
            unsafe {
                core::ptr::copy_nonoverlapping(virt(d.bounce_pa), chunk.as_mut_ptr(), chunk.len())
            };
        }
        Ok(())
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()> {
        if self.ro {
            return Err(Errno::EROFS);
        }
        self.check(lba, buf.len())?;
        let mut d = self.inner.lock();
        for (i, chunk) in buf.chunks(BOUNCE_BYTES).enumerate() {
            let sector = lba + (i * BOUNCE_BYTES / SECTOR) as u64;
            // SAFETY: as in read_blocks.
            unsafe {
                core::ptr::copy_nonoverlapping(chunk.as_ptr(), virt(d.bounce_pa), chunk.len())
            };
            d.submit(T_OUT, sector, chunk.len(), false)?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        if !self.can_flush {
            return Ok(()); // no volatile write cache to flush
        }
        self.inner.lock().submit(T_FLUSH, 0, 0, false)
    }
}

fn on_irq(irq: u32, _frame: &mut TrapFrame) {
    let slot = (irq - gic::spi(mmio::MMIO_SPI_BASE)) as usize;
    if let Ok(t) = Transport::probe(slot) {
        t.ack_interrupt();
    }
    BLK_WQ.wake_all();
}

/// Bring up a virtio-blk device and register it as `/dev/vd{a,b,...}`.
pub fn attach(t: Transport) -> core::result::Result<(), Error> {
    let features = t.begin_init(0, F_RO | F_FLUSH)?;
    // Capacity (le64 sectors) at config 0; re-read if the config changes under us.
    let sectors = loop {
        let g = t.config_generation();
        let lo = t.config_read32(0) as u64;
        let hi = t.config_read32(4) as u64;
        if t.config_generation() == g {
            break hi << 32 | lo;
        }
    };
    let qsize = (t.queue_num_max(0) as usize).min(MAX_QUEUE_SIZE);
    if qsize < 4 {
        t.fail();
        return Err(Error::BadQueue);
    }
    let qsize = 1u16 << (usize::BITS - 1 - qsize.leading_zeros()); // power of two
    let alloc = |n| pmm::alloc_contiguous_zeroed(n, 1).ok_or(Error::BadQueue);
    let qmem_pa = alloc(1)?;
    let hdr_pa = alloc(1)?;
    let bounce_pa = alloc(BOUNCE_PAGES)?;
    // SAFETY: fresh zeroed frame, identity mapped, used only by this queue from now on.
    let vq = unsafe { Virtqueue::new(phys_to_virt(qmem_pa) as *mut QueueMem, qsize) };
    t.setup_queue(0, &vq)?;
    if gic::register(t.irq(), on_irq).is_ok() {
        let _ = gic::set_edge_triggered(t.irq(), false); // level, like virtio-input
        let _ = gic::enable(t.irq());
    }
    t.driver_ok();

    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let dev = VirtioBlk {
        name: format!("vd{}", (b'a' + n as u8) as char),
        sectors,
        ro: features & F_RO != 0,
        can_flush: features & F_FLUSH != 0,
        inner: Mutex::new(Inner {
            t,
            vq,
            hdr_pa,
            bounce_pa,
        }),
    };
    let dev: blockdev::BlockRef = Arc::new(dev);
    kprintln!(
        "[virtio-blk] {}, queue {}, flush {}",
        blockdev::describe(&dev),
        qsize,
        if features & F_FLUSH != 0 { "yes" } else { "no" }
    );
    blockdev::register(dev);
    Ok(())
}
