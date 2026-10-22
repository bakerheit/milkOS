//! virtio-rng (virtio 1.x, 5.4): one request queue; each request is a device-writable
//! buffer the host fills with random bytes (qemu: `-object rng-random,id=rng0,
//! filename=/dev/urandom -device virtio-rng-device,rng=rng0`).
//!
//! Used only by the kernel CSPRNG ([`crate::crypto::rng`]) for its seed and reseeds, so it
//! is polled, not interrupt driven: [`read`] posts one buffer and spins (briefly, with a
//! timeout) until the device returns it. No IRQ handler, no wait queue.

use core::sync::atomic::{AtomicBool, Ordering};

use super::mmio::{Error, Transport};
use super::queue::{QueueMem, Virtqueue};
use crate::mmu::{PhysAddr, phys_to_virt};
use crate::sync::SpinLock;
use crate::{kprintln, pmm, timer};

const REQQ: u16 = 0;
/// One request's buffer (bytes): what [`read`] can return per call.
pub const CHUNK: usize = 64;

struct Dev {
    t: Transport,
    q: Virtqueue,
    buf: PhysAddr,
}

// SAFETY: the queue memory and buffer are PMM pages owned by this device; access is under
// DEV's lock.
unsafe impl Send for Dev {}

static DEV: SpinLock<Option<Dev>> = SpinLock::new(None);
static PRESENT: AtomicBool = AtomicBool::new(false);

pub fn present() -> bool {
    PRESENT.load(Ordering::Acquire)
}

pub fn attach(t: Transport) -> Result<(), Error> {
    if present() {
        kprintln!("[virtio-rng] slot {}: second RNG ignored", t.slot());
        t.reset();
        return Ok(());
    }
    t.begin_init(0, 0)?;
    let n = t.queue_num_max(REQQ).min(8);
    if n == 0 {
        t.fail();
        return Err(Error::BadQueue);
    }
    let size = 1u16 << (15 - n.leading_zeros()); // largest power of two <= n
    let (Some(qpa), Some(buf)) = (
        pmm::alloc_contiguous_zeroed_as(pmm::Use::Devices, 1, 1),
        pmm::alloc_contiguous_zeroed_as(pmm::Use::Devices, 1, 1),
    ) else {
        t.fail();
        return Err(Error::BadQueue);
    };
    // SAFETY: a fresh zeroed page, owned by this queue from now on.
    let q = unsafe { Virtqueue::new(phys_to_virt(qpa) as *mut QueueMem, size) };
    t.setup_queue(REQQ, &q)?;
    t.driver_ok();
    let slot = t.slot();
    *DEV.lock() = Some(Dev { t, q, buf });
    PRESENT.store(true, Ordering::Release);
    kprintln!("[virtio-rng] slot {slot}: entropy source ({size} descriptors, polled)");
    Ok(())
}

/// Let go of the device for a user-space driver (`udrv`, docs/drivers.md): reset it (no
/// request can be in flight: [`read`] waits for each under the lock, or abandons it before
/// this reset stops the device) and free the queue and buffer pages.
pub fn detach() {
    let Some(d) = DEV.lock().take() else { return };
    PRESENT.store(false, Ordering::Release);
    d.t.reset();
    let (desc, _, _) = d.q.addresses();
    pmm::free_contiguous(desc as PhysAddr, 1);
    pmm::free_contiguous(d.buf, 1);
    kprintln!(
        "[virtio-rng] slot {}: handed to a user-space driver",
        d.t.slot()
    );
}

/// Fill `out` (up to [`CHUNK`] bytes) from the device. Returns how many bytes it gave (0:
/// no device, or no answer within `timeout_ns`). Thread or boot context; spins.
pub fn read(out: &mut [u8], timeout_ns: u64) -> usize {
    let mut g = DEV.lock();
    let Some(d) = g.as_mut() else { return 0 };
    let want = out.len().min(CHUNK);
    // Nothing else is ever in flight: we wait for each request before returning.
    let Some(head) = d.q.add(&[], &[(d.buf as u64, want as u32)]) else {
        return 0;
    };
    if d.q.should_notify() {
        d.t.notify(REQQ);
    }
    let end = timer::now_ns().saturating_add(timeout_ns);
    loop {
        if let Some((h, len)) = d.q.pop_used() {
            d.q.free(h);
            if h != head {
                continue;
            }
            let n = (len as usize).min(want);
            // SAFETY: the device wrote `len` bytes into our buffer page and handed it back.
            let src = unsafe { core::slice::from_raw_parts(phys_to_virt(d.buf) as *const u8, n) };
            out[..n].copy_from_slice(src);
            // Don't leave the bytes lying around in the DMA page.
            // SAFETY: as above, the page is ours again.
            unsafe { core::ptr::write_bytes(phys_to_virt(d.buf) as *mut u8, 0, n) };
            return n;
        }
        if timer::now_ns() >= end {
            // Leave the descriptor posted; the next call reclaims it (h != head above).
            return 0;
        }
        core::hint::spin_loop();
    }
}
