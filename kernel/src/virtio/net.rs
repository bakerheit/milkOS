//! virtio-net driver (virtio 1.x, 5.1): one receive queue (0), one transmit queue (1).
//!
//! Features: `VIRTIO_NET_F_MAC` (address from config space) and `VIRTIO_NET_F_STATUS`
//! (link state) only. No checksum or segmentation offloads, no mergeable rx buffers, no
//! control queue. With `VIRTIO_F_VERSION_1` every packet starts with the 12-byte
//! `virtio_net_hdr` (`num_buffers` included); we send it zeroed and ignore it on receive.
//!
//! Memory (all from the PMM, zeroed, identity mapped; virtio-mmio on qemu `virt` is
//! dma-coherent): one page per queue, and one 2 KiB buffer per descriptor for each queue.
//! Every rx descriptor permanently owns one buffer and goes straight back on the ring once
//! the stack is done with the packet ([`VirtioNet::rx_done`]). Tx buffers are taken from a
//! free bitmap and come back when the device returns the descriptor.
//!
//! The driver does no locking of its own: the network stack owns the [`VirtioNet`] inside
//! its lock (see `crate::net`). The interrupt handler only acknowledges the interrupt and
//! wakes the net thread ([`crate::net::on_device_irq`]), which drains the rings.

use core::ptr::write_bytes;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::mmio::{Error, MMIO_SPI_BASE, Transport};
use super::queue::{MAX_QUEUE_SIZE, QueueMem, Virtqueue};
use crate::exceptions::TrapFrame;
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::{gic, kprintln, pmm};

const F_MAC: u64 = 1 << 5;
const F_STATUS: u64 = 1 << 16;

const RXQ: u16 = 0;
const TXQ: u16 = 1;

/// `struct virtio_net_hdr` with VERSION_1 (num_buffers is always there).
pub const HDR_LEN: usize = 12;
/// Per-descriptor buffer: header + a full 1514-byte Ethernet frame, rounded up.
const BUF_SIZE: usize = 2048;
/// Largest Ethernet frame (no FCS, no VLAN tag) we send or expect.
pub const MAX_FRAME: usize = 1514;

// Config space (5.1.4): mac[6], status le16.
const CFG_MAC: usize = 0;
const CFG_STATUS: usize = 6;
const STATUS_LINK_UP: u8 = 1;

/// Interface counters, the columns of `/proc/net/dev` we can fill.
#[derive(Clone, Copy, Default, Debug)]
pub struct DevStats {
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_dropped: u64,
}

/// A received frame, still sitting in its rx buffer. Hand it back with
/// [`VirtioNet::rx_done`] once processed; the slice stays valid until then.
pub struct RxBuf {
    head: u16,
    ptr: *const u8,
    len: usize,
}

impl RxBuf {
    /// The Ethernet frame (virtio header stripped).
    ///
    /// The returned slice is not tied to a borrow of the device, so the stack can keep
    /// using (and transmitting through) the device while it parses the frame.
    pub fn frame<'a>(&self) -> &'a [u8] {
        // SAFETY: the buffer belongs to a descriptor the device has returned and that is not
        // re-offered until `rx_done(self)`, which consumes `self`; the stack does not keep
        // the slice past that call (it processes each frame to completion first).
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

pub struct VirtioNet {
    t: Transport,
    rx: Virtqueue,
    tx: Virtqueue,
    rx_bufs: PhysAddr,
    tx_bufs: PhysAddr,
    /// rx descriptor head -> buffer index (fixed after init).
    rx_buf_of: [u8; MAX_QUEUE_SIZE],
    /// tx descriptor head -> buffer index, for buffers in flight.
    tx_buf_of: [u8; MAX_QUEUE_SIZE],
    /// Bit i set: tx buffer i is free.
    tx_free: u64,
    rx_requeued: bool,
    mac: [u8; 6],
    has_status: bool,
    pub stats: DevStats,
}

// SAFETY: raw pointers are to PMM memory owned by this device; access is serialised by the
// network stack's lock.
unsafe impl Send for VirtioNet {}

/// Set once a virtio-net device is attached (cheap check for "is there a NIC").
static PRESENT: AtomicBool = AtomicBool::new(false);
/// Devices attached so far (the stack takes up to `net::iface::MAX_LINKS`).
static COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn present() -> bool {
    PRESENT.load(Ordering::Acquire)
}

impl VirtioNet {
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Link state from the config space (always up without `VIRTIO_NET_F_STATUS`).
    pub fn link_up(&self) -> bool {
        !self.has_status || self.t.config_read8(CFG_STATUS) & STATUS_LINK_UP != 0
    }

    fn rx_va(&self, i: usize) -> *mut u8 {
        phys_to_virt(self.rx_bufs + i * BUF_SIZE) as *mut u8
    }

    fn tx_va(&self, i: usize) -> *mut u8 {
        phys_to_virt(self.tx_bufs + i * BUF_SIZE) as *mut u8
    }

    /// Next received frame, if any. Runts (no room for an Ethernet header) are recycled
    /// here and counted as errors.
    pub fn rx_pop(&mut self) -> Option<RxBuf> {
        loop {
            let (head, len) = self.rx.pop_used()?;
            let len = len as usize;
            if !(HDR_LEN + 14..=BUF_SIZE).contains(&len) {
                self.stats.rx_errors += 1;
                self.rx.requeue(head);
                self.rx_requeued = true;
                continue;
            }
            let i = self.rx_buf_of[head as usize] as usize;
            self.stats.rx_packets += 1;
            self.stats.rx_bytes += (len - HDR_LEN) as u64;
            // SAFETY: inside buffer i (len <= BUF_SIZE checked).
            let ptr = unsafe { self.rx_va(i).add(HDR_LEN) };
            return Some(RxBuf {
                head,
                ptr,
                len: len - HDR_LEN,
            });
        }
    }

    /// Give a frame's buffer back to the device. Call [`rx_kick`](Self::rx_kick) after a
    /// batch.
    pub fn rx_done(&mut self, b: RxBuf) {
        self.rx.requeue(b.head);
        self.rx_requeued = true;
    }

    /// Notify the device about re-offered rx buffers, if it wants to know.
    pub fn rx_kick(&mut self) {
        if self.rx_requeued {
            self.rx_requeued = false;
            if self.rx.should_notify() {
                self.t.notify(RXQ);
            }
        }
    }

    /// Take back tx buffers the device is done with.
    pub fn tx_reclaim(&mut self) {
        while let Some((head, _)) = self.tx.pop_used() {
            let i = self.tx_buf_of[head as usize];
            self.tx.free(head);
            self.tx_free |= 1 << i;
        }
    }

    /// Transmit one frame: `fill` writes it into the buffer it is given (up to
    /// [`MAX_FRAME`] bytes) and returns its length; 0 cancels. False (frame counted as
    /// dropped) if every tx buffer is in flight.
    pub fn send_with(&mut self, fill: impl FnOnce(&mut [u8]) -> usize) -> bool {
        self.tx_reclaim();
        if self.tx_free == 0 {
            self.stats.tx_dropped += 1;
            return false;
        }
        let i = self.tx_free.trailing_zeros() as usize;
        let va = self.tx_va(i);
        // SAFETY: buffer i is free (not owned by the device) and BUF_SIZE long.
        let buf = unsafe {
            write_bytes(va, 0, HDR_LEN);
            core::slice::from_raw_parts_mut(va.add(HDR_LEN), MAX_FRAME)
        };
        let n = fill(buf).min(MAX_FRAME);
        if n == 0 {
            return false;
        }
        // Pad short frames to the Ethernet minimum (60 bytes without FCS).
        let n = if n < 60 {
            buf[n..60].fill(0);
            60
        } else {
            n
        };
        let pa = (self.tx_bufs + i * BUF_SIZE) as u64;
        let Some(head) = self.tx.add(&[(pa, (HDR_LEN + n) as u32)], &[]) else {
            self.stats.tx_dropped += 1;
            return false;
        };
        self.tx_free &= !(1 << i);
        self.tx_buf_of[head as usize] = i as u8;
        self.stats.tx_packets += 1;
        self.stats.tx_bytes += n as u64;
        if self.tx.should_notify() {
            self.t.notify(TXQ);
        }
        true
    }
}

fn on_irq(irq: u32, _frame: &mut TrapFrame) {
    // Ack here (only the interrupt status register: no queue state, no lock needed), so the
    // level-triggered line drops; the net thread drains the rings.
    let slot = (irq - gic::spi(MMIO_SPI_BASE)) as usize;
    if let Ok(t) = Transport::probe(slot) {
        t.ack_interrupt();
    }
    crate::net::on_device_irq();
}

fn pow2_queue(t: &Transport, idx: u16) -> Result<u16, Error> {
    let max = (t.queue_num_max(idx) as usize).min(MAX_QUEUE_SIZE);
    if max < 4 {
        return Err(Error::BadQueue);
    }
    Ok(1u16 << (usize::BITS - 1 - max.leading_zeros()))
}

/// Bring up a virtio-net device and hand it to the network stack. Up to two are used
/// (eth0, eth1, named in qemu command-line order by the stack); more are reset and left
/// alone.
pub fn attach(t: Transport) -> Result<(), Error> {
    if COUNT.load(Ordering::Acquire) >= crate::net::iface::MAX_LINKS {
        kprintln!(
            "[virtio-net] slot {}: NIC ignored (eth0 and eth1 are taken)",
            t.slot()
        );
        t.reset();
        return Ok(());
    }
    COUNT.fetch_add(1, Ordering::AcqRel);
    let features = t.begin_init(0, F_MAC | F_STATUS)?;
    let mac = if features & F_MAC != 0 {
        loop {
            let g = t.config_generation();
            let mut m = [0u8; 6];
            for (k, b) in m.iter_mut().enumerate() {
                *b = t.config_read8(CFG_MAC + k);
            }
            if t.config_generation() == g {
                break m;
            }
        }
    } else {
        // Locally administered fallback; the device then accepts any address we use.
        [0x02, 0x6d, 0x69, 0x6c, 0x6b, t.slot() as u8]
    };

    let nrx = pow2_queue(&t, RXQ)? as usize;
    let ntx = pow2_queue(&t, TXQ)? as usize;
    let fail = |t: &Transport| {
        t.fail();
        Error::BadQueue
    };
    let alloc = |n| pmm::alloc_contiguous_zeroed(n, 1);
    let pages = |bufs: usize| (bufs * BUF_SIZE).div_ceil(PAGE_SIZE);
    let (Some(rxq_pa), Some(txq_pa), Some(rx_bufs), Some(tx_bufs)) =
        (alloc(1), alloc(1), alloc(pages(nrx)), alloc(pages(ntx)))
    else {
        return Err(fail(&t));
    };
    // SAFETY: fresh zeroed frames, identity mapped, owned by these queues from now on.
    let mut rx = unsafe { Virtqueue::new(phys_to_virt(rxq_pa) as *mut QueueMem, nrx as u16) };
    let tx = unsafe { Virtqueue::new(phys_to_virt(txq_pa) as *mut QueueMem, ntx as u16) };
    let mut rx_buf_of = [0u8; MAX_QUEUE_SIZE];
    for i in 0..nrx {
        let pa = (rx_bufs + i * BUF_SIZE) as u64;
        let head = rx
            .add(&[], &[(pa, BUF_SIZE as u32)])
            .ok_or_else(|| fail(&t))?;
        rx_buf_of[head as usize] = i as u8;
    }
    t.setup_queue(RXQ, &rx)?;
    t.setup_queue(TXQ, &tx)?;

    let dev = VirtioNet {
        t,
        rx,
        tx,
        rx_bufs,
        tx_bufs,
        rx_buf_of,
        tx_buf_of: [0; MAX_QUEUE_SIZE],
        tx_free: if ntx == 64 {
            u64::MAX
        } else {
            (1u64 << ntx) - 1
        },
        rx_requeued: false,
        mac,
        has_status: features & F_STATUS != 0,
        stats: DevStats::default(),
    };
    if gic::register(t.irq(), on_irq).is_ok() {
        let _ = gic::set_edge_triggered(t.irq(), false); // level, like the other virtio drivers
        let _ = gic::enable(t.irq());
    }
    t.driver_ok();
    t.notify(RXQ);
    let slot = t.slot();
    kprintln!(
        "[virtio-net] slot {slot}: mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, rx {} / tx {} buffers, link {}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        nrx,
        ntx,
        if dev.link_up() { "up" } else { "down" }
    );
    PRESENT.store(true, Ordering::Release);
    crate::net::attach_device(dev, slot);
    Ok(())
}
