//! virtio-9p transport (virtio 1.x, 5.11): carries 9P messages for `fs::ninep`.
//!
//! One request queue; one request in flight. A request is a 2-descriptor chain: the
//! T-message (device reads) and a response buffer of `msize` bytes (device writes). Both
//! buffers come from the PMM. The caller sleeps on [`P9_WQ`] until the used ring moves;
//! the IRQ handler acks and wakes (a `wfi` loop before `sched::init`, like virtio-blk).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::copy_nonoverlapping;

use super::mmio::{self, Error, Transport};
use super::queue::{MAX_QUEUE_SIZE, QueueMem, Virtqueue};
use crate::exceptions::TrapFrame;
use crate::fs::errno::{Errno, Result};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::sched::{Mutex, WaitQueue};
use crate::{gic, kprintln, pmm, timer};

/// Feature: the config space has a mount tag.
const F_MOUNT_TAG: u64 = 1 << 0;
/// Largest 9P message we exchange (request or response).
pub const MSIZE: usize = 64 * 1024;
const BUF_PAGES: usize = MSIZE / PAGE_SIZE;
const TIMEOUT_NS: u64 = 10_000_000_000;

static P9_WQ: WaitQueue = WaitQueue::new();

struct Inner {
    t: Transport,
    vq: Virtqueue,
    out_pa: PhysAddr,
    in_pa: PhysAddr,
}

/// One virtio-9p device: a 9P message pipe to one host export.
pub struct P9Device {
    pub tag: String,
    inner: Mutex<Inner>,
}

static DEVICES: crate::sync::SpinLock<Vec<Arc<P9Device>>> = crate::sync::SpinLock::new(Vec::new());

/// Every virtio-9p device found at boot.
pub fn devices() -> Vec<Arc<P9Device>> {
    DEVICES.lock().clone()
}

impl P9Device {
    /// Send one complete 9P message and return the reply (its own `size` field decides
    /// the length).
    pub fn rpc(&self, msg: &[u8]) -> Result<Vec<u8>> {
        if msg.len() > MSIZE {
            return Err(Errno::EINVAL);
        }
        let mut d = self.inner.lock();
        let d = &mut *d;
        // SAFETY: our buffers, MSIZE long; the device is idle (one request, lock held).
        unsafe { copy_nonoverlapping(msg.as_ptr(), phys_to_virt(d.out_pa) as *mut u8, msg.len()) };
        let head =
            d.vq.add(
                &[(d.out_pa as u64, msg.len() as u32)],
                &[(d.in_pa as u64, MSIZE as u32)],
            )
            .ok_or(Errno::EIO)?;
        if d.vq.should_notify() {
            d.t.notify(0);
        }
        let start = timer::now_ns();
        let len = loop {
            let mut done = None;
            let vq = &mut d.vq;
            let left = TIMEOUT_NS.saturating_sub(timer::now_ns() - start);
            P9_WQ.wait_until_timeout(
                || {
                    if done.is_none() {
                        done = vq.pop_used();
                    }
                    done.is_some()
                },
                left,
            );
            match done {
                Some((id, len)) => {
                    d.vq.free(id);
                    if id == head {
                        break len as usize;
                    }
                }
                None => {
                    kprintln!("[virtio-9p] request timed out");
                    return Err(Errno::EIO);
                }
            }
        };
        let buf = phys_to_virt(d.in_pa) as *const u8;
        // SAFETY: the device wrote `len` <= MSIZE bytes.
        let reply = unsafe { core::slice::from_raw_parts(buf, len.min(MSIZE)) };
        if reply.len() < 7 {
            return Err(Errno::EIO);
        }
        let size = u32::from_le_bytes(reply[..4].try_into().unwrap()) as usize;
        Ok(reply[..size.min(reply.len())].to_vec())
    }
}

fn on_irq(irq: u32, _frame: &mut TrapFrame) {
    let slot = (irq - gic::spi(mmio::MMIO_SPI_BASE)) as usize;
    if let Ok(t) = Transport::probe(slot) {
        t.ack_interrupt();
    }
    P9_WQ.wake_all();
}

pub fn attach(t: Transport) -> core::result::Result<(), Error> {
    let features = t.begin_init(0, F_MOUNT_TAG)?;
    let mut tag = String::new();
    if features & F_MOUNT_TAG != 0 {
        let n = t.config_read8(0) as usize | (t.config_read8(1) as usize) << 8;
        for i in 0..n.min(64) {
            tag.push(t.config_read8(2 + i) as char);
        }
    }
    let qsize = (t.queue_num_max(0) as usize).min(MAX_QUEUE_SIZE);
    if qsize < 2 {
        t.fail();
        return Err(Error::BadQueue);
    }
    let qsize = 1u16 << (usize::BITS - 1 - qsize.leading_zeros());
    let alloc = |n| pmm::alloc_contiguous_zeroed(n, 1).ok_or(Error::BadQueue);
    let qmem_pa = alloc(1)?;
    let out_pa = alloc(BUF_PAGES)?;
    let in_pa = alloc(BUF_PAGES)?;
    // SAFETY: fresh zeroed frame, identity mapped, used only by this queue.
    let vq = unsafe { Virtqueue::new(phys_to_virt(qmem_pa) as *mut QueueMem, qsize) };
    t.setup_queue(0, &vq)?;
    if gic::register(t.irq(), on_irq).is_ok() {
        let _ = gic::set_edge_triggered(t.irq(), false);
        let _ = gic::enable(t.irq());
    }
    t.driver_ok();
    kprintln!("[virtio-9p] mount tag {tag:?}, msize {MSIZE}");
    DEVICES.lock().push(Arc::new(P9Device {
        tag,
        inner: Mutex::new(Inner {
            t,
            vq,
            out_pa,
            in_pa,
        }),
    }));
    Ok(())
}
