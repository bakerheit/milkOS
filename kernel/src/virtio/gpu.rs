//! virtio-gpu, 2D only (virtio 1.x, 5.7; qemu: `-device virtio-gpu-device`). The display
//! behind `crate::display` when the device is there (ramfb otherwise).
//!
//! Why not ramfb: qemu scans a ramfb straight out of guest RAM on its own timer, so it can
//! show a frame the compositor is halfway through copying (a tear). Here the pixels live in a
//! host-side resource; the guest framebuffer is only its backing. A frame reaches the screen
//! as TRANSFER_TO_HOST_2D for each damaged rect, then one RESOURCE_FLUSH, all queued with a
//! single notify, so qemu copies the whole frame in one go. The rule that keeps it tear-free:
//! nobody writes the backing while a transfer may still be reading it. [`present`] queues a
//! frame and returns; [`wait`] (call it before touching the pixels again) spins until the
//! device has handed every command back. At 60 Hz the previous frame is long done by then.
//!
//! Set up and driven from `display::init`, before `virtio::init` and before interrupts, so
//! everything is polled: one control queue, no IRQ handler (the device's interrupt is never
//! enabled at the GIC). No logging under the lock (the text console presents through here).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::mmio::Transport;
use super::queue::{MAX_QUEUE_SIZE, QueueMem, Virtqueue};
use crate::gfx::{Rect, Surface};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::sync::SpinLock;
use crate::{kprintln, pmm};

const CTRLQ: u16 = 0;

// Control commands and responses (5.7.6.7).
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_RESOURCE_UNREF: u32 = 0x0102;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// Bytes B, G, R, X in memory: the little-endian `0x00RRGGBB` (XRGB8888) the kernel draws.
const FORMAT_B8G8R8X8_UNORM: u32 = 2;

/// `struct virtio_gpu_ctrl_hdr`.
const HDR: usize = 24;
/// Frame commands live in one page of fixed slots: request at +0, response at +RESP.
const SLOT: usize = 128;
const RESP: usize = 64;
const SLOTS: usize = PAGE_SIZE / SLOT;
/// Two descriptors per command, so at most this many commands are in flight.
const MAX_CMDS: usize = MAX_QUEUE_SIZE / 2;
const _: () = assert!(SLOTS >= MAX_CMDS);
/// Setup commands use their own page: request at +0, response at +SYNC_RESP.
const SYNC_RESP: usize = 2048;
/// How long to wait for the device before giving up on it.
const TIMEOUT_MS: u64 = 1000;

const BPP: usize = 4;

struct Gpu {
    t: Transport,
    q: Virtqueue,
    /// The frame command page and the setup command page.
    cmd: PhysAddr,
    sync: PhysAddr,
    /// Commands queued and not yet handed back.
    in_flight: usize,
    /// Slots used by the last frame (their responses are checked once it is back).
    batch: usize,
    /// The scanned-out resource and its backing (the framebuffer).
    res: u32,
    w: usize,
    h: usize,
    fb: PhysAddr,
    fb_pages: usize,
    /// Stopped answering: present nothing from now on.
    dead: bool,
}

// SAFETY: the queue, command pages and framebuffer are PMM pages owned by this driver; all
// access is under GPU's lock.
unsafe impl Send for Gpu {}

static GPU: SpinLock<Option<Gpu>> = SpinLock::new(None);
/// Something went wrong under the lock; logged (once) by the next caller outside it.
static ERR_TIMEOUT: AtomicBool = AtomicBool::new(false);
static ERR_RESP: AtomicBool = AtomicBool::new(false);
static LOGGED: AtomicBool = AtomicBool::new(false);
/// The live framebuffer (kernel address, 0 = none) and its size, readable without the lock
/// (the panic screen).
static FB_PTR: AtomicUsize = AtomicUsize::new(0);
static FB_W: AtomicUsize = AtomicUsize::new(0);
static FB_H: AtomicUsize = AtomicUsize::new(0);

/// The architectural counter, usable before `timer::init` (the driver starts first).
fn ticks() -> (u64, u64) {
    let (now, freq): (u64, u64);
    // SAFETY: reading EL0-accessible system counter registers.
    unsafe {
        core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) now, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack));
    }
    (now, freq)
}

fn put32(base: *mut u8, off: usize, v: u32) {
    // SAFETY: callers pass offsets inside one of our command pages; aligned.
    unsafe { write_volatile(base.add(off) as *mut u32, v) };
}

fn put64(base: *mut u8, off: usize, v: u64) {
    // SAFETY: as in put32.
    unsafe { write_volatile(base.add(off) as *mut u64, v) };
}

fn get32(base: *const u8, off: usize) -> u32 {
    // SAFETY: as in put32.
    unsafe { read_volatile(base.add(off) as *const u32) }
}

/// A control header: type, then zero flags / fence / context.
fn hdr(base: *mut u8, ty: u32) {
    put32(base, 0, ty);
    for off in (4..HDR).step_by(4) {
        put32(base, off, 0);
    }
}

fn put_rect(base: *mut u8, off: usize, r: Rect) {
    put32(base, off, r.x as u32);
    put32(base, off + 4, r.y as u32);
    put32(base, off + 8, r.w as u32);
    put32(base, off + 12, r.h as u32);
}

impl Gpu {
    /// Reap everything the device has handed back; spin until nothing is in flight.
    fn wait_idle(&mut self) {
        if self.in_flight == 0 || self.dead {
            return;
        }
        let (start, freq) = ticks();
        let limit = freq / 1000 * TIMEOUT_MS;
        while self.in_flight > 0 {
            if let Some((head, _)) = self.q.pop_used() {
                self.q.free(head);
                self.in_flight -= 1;
                continue;
            }
            if ticks().0.wrapping_sub(start) > limit {
                self.dead = true;
                ERR_TIMEOUT.store(true, Ordering::Release);
                return;
            }
            core::hint::spin_loop();
        }
        let page = phys_to_virt(self.cmd) as *const u8;
        for i in 0..core::mem::take(&mut self.batch) {
            if get32(page, i * SLOT + RESP) != RESP_OK_NODATA {
                ERR_RESP.store(true, Ordering::Release);
            }
        }
    }

    /// Queue the request in frame slot `i` (`len` bytes); does not notify.
    fn queue_slot(&mut self, i: usize, len: usize) -> bool {
        let pa = (self.cmd + i * SLOT) as u64;
        // SAFETY: the response slot is ours; mark it so a command the device skips shows.
        put32(phys_to_virt(self.cmd) as *mut u8, i * SLOT + RESP, 0);
        if self
            .q
            .add(&[(pa, len as u32)], &[(pa + RESP as u64, HDR as u32)])
            .is_none()
        {
            return false;
        }
        self.in_flight += 1;
        true
    }

    /// Run one setup command from the sync page and wait for it. Returns the response type.
    fn sync_cmd(&mut self, len: usize, resp_len: usize) -> u32 {
        self.wait_idle();
        if self.dead {
            return 0;
        }
        let page = phys_to_virt(self.sync) as *mut u8;
        put32(page, SYNC_RESP, 0);
        let pa = self.sync as u64;
        if self
            .q
            .add(
                &[(pa, len as u32)],
                &[(pa + SYNC_RESP as u64, resp_len as u32)],
            )
            .is_none()
        {
            return 0;
        }
        self.in_flight += 1;
        self.t.notify(CTRLQ);
        self.wait_idle();
        get32(page, SYNC_RESP)
    }

    fn create_2d(&mut self, id: u32, w: usize, h: usize) -> bool {
        let p = phys_to_virt(self.sync) as *mut u8;
        hdr(p, CMD_RESOURCE_CREATE_2D);
        put32(p, HDR, id);
        put32(p, HDR + 4, FORMAT_B8G8R8X8_UNORM);
        put32(p, HDR + 8, w as u32);
        put32(p, HDR + 12, h as u32);
        self.sync_cmd(HDR + 16, HDR) == RESP_OK_NODATA
    }

    /// One contiguous backing region for resource `id`.
    fn attach(&mut self, id: u32, pa: PhysAddr, len: usize) -> bool {
        let p = phys_to_virt(self.sync) as *mut u8;
        hdr(p, CMD_RESOURCE_ATTACH_BACKING);
        put32(p, HDR, id);
        put32(p, HDR + 4, 1);
        put64(p, HDR + 8, pa as u64);
        put32(p, HDR + 16, len as u32);
        put32(p, HDR + 20, 0);
        self.sync_cmd(HDR + 24, HDR) == RESP_OK_NODATA
    }

    fn scanout(&mut self, id: u32, w: usize, h: usize) -> bool {
        let p = phys_to_virt(self.sync) as *mut u8;
        hdr(p, CMD_SET_SCANOUT);
        put_rect(p, HDR, Rect::new(0, 0, w as i32, h as i32));
        put32(p, HDR + 16, 0); // scanout 0
        put32(p, HDR + 20, id);
        self.sync_cmd(HDR + 24, HDR) == RESP_OK_NODATA
    }

    /// Unref also detaches the backing (qemu frees its host copy).
    fn unref(&mut self, id: u32) {
        let p = phys_to_virt(self.sync) as *mut u8;
        hdr(p, CMD_RESOURCE_UNREF);
        put32(p, HDR, id);
        put32(p, HDR + 4, 0);
        self.sync_cmd(HDR + 8, HDR);
    }

    /// Scanout 0's preferred size, if the device reports it enabled.
    fn display_info(&mut self) -> Option<(u32, u32)> {
        let p = phys_to_virt(self.sync) as *mut u8;
        hdr(p, CMD_GET_DISPLAY_INFO);
        // 16 scanouts of { rect, enabled, flags }.
        if self.sync_cmd(HDR, HDR + 16 * 24) != RESP_OK_DISPLAY_INFO {
            return None;
        }
        let r = p.wrapping_add(SYNC_RESP + HDR);
        (get32(r, 16) != 0).then(|| (get32(r, 8), get32(r, 12)))
    }

    /// Make a `w` x `h` resource over fresh frames and scan it out. On failure nothing changed.
    fn switch(&mut self, w: usize, h: usize) -> Option<(PhysAddr, usize)> {
        let pages = (w * h * BPP).div_ceil(PAGE_SIZE);
        let pa = pmm::alloc_contiguous_zeroed_as(pmm::Use::Display, pages, 1)?;
        let id = self.res + 1;
        let ok = self.create_2d(id, w, h);
        if !(ok && self.attach(id, pa, w * h * BPP) && self.scanout(id, w, h)) {
            if ok {
                self.unref(id);
            }
            pmm::free_contiguous(pa, pages);
            return None;
        }
        let old = (self.res, self.fb, self.fb_pages);
        self.res = id;
        self.w = w;
        self.h = h;
        self.fb = pa;
        self.fb_pages = pages;
        FB_W.store(w, Ordering::Release);
        FB_H.store(h, Ordering::Release);
        FB_PTR.store(phys_to_virt(pa), Ordering::Release);
        if old.0 != 0 {
            self.unref(old.0);
            pmm::free_contiguous(old.1, old.2);
        }
        Some((pa, pages))
    }

    /// Queue transfers for `rects` (clipped to the screen) and one flush over them all.
    fn present(&mut self, rects: &[Rect]) {
        self.wait_idle();
        if self.dead {
            return;
        }
        let mut list = [Rect::new(0, 0, 0, 0); MAX_CMDS - 1];
        let mut n = 0;
        let mut bound: Option<Rect> = None;
        for r in rects {
            let r = r.clipped(self.w, self.h);
            if r.is_empty() {
                continue;
            }
            bound = Some(bound.map_or(r, |b| b.union(&r)));
            if n < list.len() {
                list[n] = r;
            }
            n += 1;
        }
        let Some(bound) = bound else { return };
        // More rects than slots: one transfer over their bounding box.
        if n > list.len() {
            list[0] = bound.clipped(self.w, self.h);
            n = 1;
        }
        let page = phys_to_virt(self.cmd) as *mut u8;
        let stride = self.w * BPP;
        for (i, r) in list[..n].iter().enumerate() {
            let p = page.wrapping_add(i * SLOT);
            hdr(p, CMD_TRANSFER_TO_HOST_2D);
            put_rect(p, HDR, *r);
            put64(
                p,
                HDR + 16,
                (r.y as usize * stride + r.x as usize * BPP) as u64,
            );
            put32(p, HDR + 24, self.res);
            put32(p, HDR + 28, 0);
            if !self.queue_slot(i, HDR + 32) {
                break;
            }
        }
        let p = page.wrapping_add(n * SLOT);
        hdr(p, CMD_RESOURCE_FLUSH);
        put_rect(p, HDR, bound);
        put32(p, HDR + 16, self.res);
        put32(p, HDR + 20, 0);
        self.queue_slot(n, HDR + 24);
        self.batch = n + 1;
        self.t.notify(CTRLQ);
    }
}

/// Log (once) a failure recorded under the lock.
fn report() {
    if (ERR_TIMEOUT.load(Ordering::Acquire) || ERR_RESP.load(Ordering::Acquire))
        && !LOGGED.swap(true, Ordering::AcqRel)
    {
        if ERR_TIMEOUT.load(Ordering::Acquire) {
            kprintln!("[virtio-gpu] device stopped answering; display frozen");
        } else {
            kprintln!("[virtio-gpu] the device rejected a frame command");
        }
    }
}

/// Look for a virtio-gpu among the virtio-mmio slots.
pub fn find() -> Option<Transport> {
    (0..super::mmio::MMIO_SLOTS)
        .filter_map(|s| Transport::probe(s).ok())
        .find(|t| t.device_id() == super::mmio::device_id::GPU)
}

/// Bring the device up and scan out a `w` x `h` framebuffer. Returns a surface over it, or
/// `None` (device reset, nothing kept) if anything fails. Call once.
pub fn init(t: Transport, w: usize, h: usize) -> Option<Surface<'static>> {
    t.begin_init(0, 0).ok()?;
    let fail = |t: &Transport, pages: &[PhysAddr]| {
        t.reset();
        for &p in pages {
            pmm::free_contiguous(p, 1);
        }
    };
    let n = t.queue_num_max(CTRLQ) as usize;
    if n < MAX_QUEUE_SIZE {
        fail(&t, &[]);
        return None;
    }
    let (Some(qpa), Some(cmd), Some(sync)) = (
        pmm::alloc_contiguous_zeroed_as(pmm::Use::Devices, 1, 1),
        pmm::alloc_contiguous_zeroed_as(pmm::Use::Devices, 1, 1),
        pmm::alloc_contiguous_zeroed_as(pmm::Use::Devices, 1, 1),
    ) else {
        fail(&t, &[]);
        return None;
    };
    // SAFETY: a fresh zeroed page, owned by this queue from now on.
    let q = unsafe { Virtqueue::new(phys_to_virt(qpa) as *mut QueueMem, MAX_QUEUE_SIZE as u16) };
    if t.setup_queue(CTRLQ, &q).is_err() {
        fail(&t, &[qpa, cmd, sync]);
        return None;
    }
    t.driver_ok();
    let mut g = Gpu {
        t,
        q,
        cmd,
        sync,
        in_flight: 0,
        batch: 0,
        res: 0,
        w: 0,
        h: 0,
        fb: 0,
        fb_pages: 0,
        dead: false,
    };
    let pref = g.display_info();
    let Some((pa, _)) = g.switch(w, h) else {
        fail(&t, &[qpa, cmd, sync]);
        return None;
    };
    let slot = t.slot();
    *GPU.lock() = Some(g);
    match pref {
        Some((pw, ph)) => {
            kprintln!("[virtio-gpu] slot {slot}: 2D, scanout 0 at {w}x{h} (host prefers {pw}x{ph})")
        }
        None => kprintln!("[virtio-gpu] slot {slot}: 2D, scanout 0 at {w}x{h}"),
    }
    // SAFETY: the frames are mapped at phys_to_virt(pa) and belong to the display now.
    Some(unsafe { Surface::from_raw(phys_to_virt(pa) as *mut u32, w, h, w) })
}

/// Switch to a fresh `w` x `h` framebuffer (new resource, SET_SCANOUT). The old one is freed,
/// so the caller must drop its old surface first. `None` (nothing changed) on failure.
pub fn set_mode(w: usize, h: usize) -> Option<Surface<'static>> {
    let r = {
        let mut g = GPU.lock();
        let g = g.as_mut()?;
        g.switch(w, h)
    };
    report();
    let (pa, _) = r?;
    // SAFETY: as in init.
    Some(unsafe { Surface::from_raw(phys_to_virt(pa) as *mut u32, w, h, w) })
}

/// The framebuffer (kernel address) and its size.
pub fn framebuffer() -> Option<(*mut u32, usize, usize)> {
    match FB_PTR.load(Ordering::Acquire) {
        0 => None,
        p => Some((
            p as *mut u32,
            FB_W.load(Ordering::Acquire),
            FB_H.load(Ordering::Acquire),
        )),
    }
}

/// Block until the device no longer reads the framebuffer: call before writing pixels.
pub fn wait() {
    if let Some(g) = GPU.lock().as_mut() {
        g.wait_idle();
    }
    report();
}

/// Put `rects` of the framebuffer on screen (after [`wait`] and the drawing). Returns at
/// once; the next [`wait`] waits for it.
pub fn present(rects: &[Rect]) {
    if let Some(g) = GPU.lock().as_mut() {
        g.present(rects);
    }
    report();
}

/// [`present`] for when the caller might hold the lock already (the panic screen): skips
/// rather than deadlocks.
pub fn try_present(rects: &[Rect]) {
    if let Some(mut g) = GPU.try_lock()
        && let Some(g) = g.as_mut()
    {
        g.present(rects);
        g.wait_idle();
    }
}
