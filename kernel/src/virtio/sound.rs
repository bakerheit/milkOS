//! virtio-sound driver (virtio 1.2, 5.14): PCM output only.
//!
//! Queues: 0 control, 1 event, 2 tx (3 rx is left alone: no capture). At attach we query
//! the PCM streams, take the first output stream that can do S16_LE / 48 kHz / stereo, and
//! SET_PARAMS + PREPARE it with 10 ms periods (1920 bytes) and a 4-period device buffer.
//! Playback is driven by `crate::audio` (the mixer thread): it fills a free period slot
//! ([`Sound::submit`]), calls [`Sound::start`] once a few are queued, and [`Sound::stop`]
//! when it runs dry. A tx request is a 3-descriptor chain: `virtio_snd_pcm_xfer`
//! (stream id) → PCM data → `virtio_snd_pcm_status` (device-writable); the device returns
//! it once the period has been played, which paces the mixer.
//!
//! Control requests are polled (they complete at once on qemu) with a 1 s timeout, under
//! the driver lock. The IRQ handler only acknowledges and wakes the mixer thread.
//!
//! Memory: one PMM block per queue, one page for control messages, and the period slots,
//! all zeroed and identity mapped (virtio-mmio on qemu `virt` is dma-coherent).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::mmio::{Error, MMIO_SPI_BASE, Transport};
use super::queue::{MAX_QUEUE_SIZE, QueueMem, Virtqueue};
use crate::exceptions::TrapFrame;
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::sync::SpinLock;
use crate::{gic, kprintln, pmm, timer};

const CTLQ: u16 = 0;
const EVTQ: u16 = 1;
const TXQ: u16 = 2;

// Request codes and status (5.14.6).
const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_RELEASE: u32 = 0x0103;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;
const EVT_PCM_XRUN: u32 = 0x1101;

const DIR_OUTPUT: u8 = 0;
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;

/// The one format we play: signed 16-bit little endian, 48 kHz, 2 channels.
pub const RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
pub const FRAME_BYTES: usize = 4;
/// 10 ms periods.
pub const PERIOD_FRAMES: usize = 480;
pub const PERIOD_BYTES: usize = PERIOD_FRAMES * FRAME_BYTES;
/// Periods the device buffers (and that we keep in flight at most).
pub const PERIODS: usize = 4;
/// Period slots we own (in flight + being filled).
const SLOTS: usize = 8;
const SLOT_STRIDE: usize = 2048; // 64 header bytes + 1920 data, rounded
const SLOT_DATA: usize = 64;
const PCM_INFO_SIZE: usize = 32;
const MAX_STREAMS: usize = 32;
const EVENTS: usize = 4;
const CTL_TIMEOUT_NS: u64 = 1_000_000_000;

/// The playback device (one per system).
pub struct Sound {
    t: Transport,
    ctl: Virtqueue,
    evt: Virtqueue,
    tx: Virtqueue,
    /// Control message page: request at 0, response at 512.
    ctl_pa: PhysAddr,
    /// SLOTS period slots, then EVENTS 8-byte event buffers.
    slots_pa: PhysAddr,
    stream: u32,
    /// Bit i: slot i is free.
    free: u32,
    /// tx descriptor head -> slot.
    slot_of: [u8; MAX_QUEUE_SIZE],
    /// event descriptor head -> event buffer index.
    ev_of: [u8; MAX_QUEUE_SIZE],
    in_flight: usize,
    running: bool,
    pub periods_played: u64,
    pub xruns: u64,
    pub ctl_errors: u64,
    /// Bit h: a tx chain with head h is with the device.
    tx_heads: u64,
    /// Completions for chains we never queued (left over from before a reset: qemu's
    /// reset doesn't flush a stream's buffers; see `detach`).
    pub strays: u64,
}

// SAFETY: raw PMM memory owned by the device; access serialised by SOUND's lock.
unsafe impl Send for Sound {}

static SOUND: SpinLock<Option<Sound>> = SpinLock::new(None);
static SLOT: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Run `f` on the sound device, if there is one.
pub fn with<R>(f: impl FnOnce(&mut Sound) -> R) -> Option<R> {
    SOUND.lock().as_mut().map(f)
}

pub fn present() -> bool {
    SLOT.load(Ordering::Acquire) != usize::MAX
}

fn va(pa: PhysAddr) -> *mut u8 {
    phys_to_virt(pa) as *mut u8
}

fn w32(p: *mut u8, off: usize, v: u32) {
    // SAFETY: callers pass offsets inside our own DMA pages; fields are 4-aligned.
    unsafe { write_volatile(p.add(off) as *mut u32, v.to_le()) }
}

fn r32(p: *const u8, off: usize) -> u32 {
    // SAFETY: as in w32.
    u32::from_le(unsafe { read_volatile(p.add(off) as *const u32) })
}

impl Sound {
    /// Send a control request (`req_len` bytes already written at the request offset) and
    /// wait for its response (`resp_len` bytes, status first). Returns the status.
    fn control(&mut self, req_len: usize, resp_len: usize) -> Result<u32, Error> {
        let req = (self.ctl_pa as u64, req_len as u32);
        let resp = (self.ctl_pa as u64 + 512, resp_len as u32);
        w32(va(self.ctl_pa), 512, 0);
        let head = self.ctl.add(&[req], &[resp]).ok_or(Error::BadQueue)?;
        self.t.notify(CTLQ);
        let t0 = timer::now_ns();
        loop {
            if let Some((id, _)) = self.ctl.pop_used() {
                self.ctl.free(id);
                if id == head {
                    break;
                }
            }
            if timer::now_ns() - t0 > CTL_TIMEOUT_NS {
                if self.ctl_errors < 5 {
                    kprintln!(
                        "[virtio-snd] control request {:#x} got no answer in 1 s",
                        r32(va(self.ctl_pa), 0)
                    );
                }
                self.ctl_errors += 1;
                return Err(Error::BadQueue);
            }
            core::hint::spin_loop();
        }
        let status = r32(va(self.ctl_pa), 512);
        if status != S_OK {
            self.ctl_errors += 1;
        }
        Ok(status)
    }

    /// A `virtio_snd_pcm_hdr` request (code + stream id).
    fn pcm_cmd(&mut self, code: u32) -> bool {
        let p = va(self.ctl_pa);
        w32(p, 0, code);
        w32(p, 4, self.stream);
        matches!(self.control(8, 4), Ok(S_OK))
    }

    /// Start playback (queue a few periods first).
    pub fn start(&mut self) -> bool {
        if self.running {
            return true;
        }
        let mut ok = self.pcm_cmd(R_PCM_START);
        if !ok {
            // Some devices want PREPARE again after STOP.
            ok = self.pcm_cmd(R_PCM_PREPARE) && self.pcm_cmd(R_PCM_START);
        }
        self.running = ok;
        ok
    }

    /// Stop playback (after the queued periods have played).
    pub fn stop(&mut self) {
        if self.running {
            self.pcm_cmd(R_PCM_STOP);
            self.running = false;
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Take back played periods and drain events. Returns how many periods completed.
    pub fn reclaim(&mut self) -> usize {
        let mut n = 0;
        while let Some((head, _)) = self.tx.pop_used() {
            if head >= 64 || self.tx_heads & (1 << head) == 0 {
                self.strays += 1;
                continue;
            }
            self.tx_heads &= !(1 << head);
            let s = self.slot_of[head as usize] as usize;
            self.tx.free(head);
            self.free |= 1 << s;
            self.in_flight -= 1;
            self.periods_played += 1;
            n += 1;
        }
        let ev = va(self.slots_pa + SLOTS * SLOT_STRIDE);
        let mut requeued = false;
        while let Some((head, _)) = self.evt.pop_used() {
            let k = self.ev_of[head as usize] as usize;
            if r32(ev, k * 8) == EVT_PCM_XRUN {
                self.xruns += 1;
            }
            w32(ev, k * 8, 0);
            self.evt.requeue(head);
            requeued = true;
        }
        if requeued {
            self.t.notify(EVTQ);
        }
        n
    }

    /// Room for another period right now?
    pub fn can_submit(&self) -> bool {
        self.free != 0 && self.in_flight < PERIODS
    }

    /// Queue one period: `fill` writes exactly [`PERIOD_BYTES`] of S16_LE stereo into the
    /// slice it gets. False if no slot is free.
    pub fn submit(&mut self, fill: impl FnOnce(&mut [u8])) -> bool {
        if !self.can_submit() {
            return false;
        }
        let s = self.free.trailing_zeros() as usize;
        let base = self.slots_pa + s * SLOT_STRIDE;
        let p = va(base);
        w32(p, 0, self.stream); // virtio_snd_pcm_xfer
        w32(p, 16, 0); // status
        w32(p, 20, 0); // latency_bytes
        // SAFETY: slot s is free (not owned by the device), SLOT_STRIDE bytes long.
        let data = unsafe { core::slice::from_raw_parts_mut(p.add(SLOT_DATA), PERIOD_BYTES) };
        fill(data);
        let out = [
            (base as u64, 4u32),
            ((base + SLOT_DATA) as u64, PERIOD_BYTES as u32),
        ];
        let inb = [((base + 16) as u64, 8u32)];
        let Some(head) = self.tx.add(&out, &inb) else {
            return false;
        };
        self.free &= !(1 << s);
        self.slot_of[head as usize] = s as u8;
        self.tx_heads |= 1 << head;
        self.in_flight += 1;
        if self.tx.should_notify() {
            self.t.notify(TXQ);
        }
        true
    }
}

/// Let go of the device for a user-space driver (soundd, docs/drivers.md): reset it (which
/// stops the stream and any DMA), take the interrupt back, free every page. Periods still
/// in flight are lost (at most `PERIODS`).
pub fn detach() {
    let Some(mut d) = SOUND.lock().take() else {
        return;
    };
    // qemu's virtio-sound reset leaves the stream's queued buffers in place and completes
    // them later (writing their status into our pages). RELEASE makes the device complete
    // every pending buffer now (virtio 1.2, 5.14.6.6.5.1), while the pages are still ours.
    d.stop();
    d.pcm_cmd(R_PCM_RELEASE);
    d.reclaim();
    SLOT.store(usize::MAX, Ordering::Release);
    let irq = d.t.irq();
    let _ = gic::disable(irq);
    let _ = gic::unregister(irq);
    d.t.reset();
    for q in [&d.ctl, &d.evt, &d.tx] {
        pmm::free_contiguous(q.addresses().0 as PhysAddr, 1);
    }
    pmm::free_contiguous(d.ctl_pa, 1);
    pmm::free_contiguous(
        d.slots_pa,
        (SLOTS * SLOT_STRIDE + EVENTS * 8).div_ceil(PAGE_SIZE),
    );
    kprintln!(
        "[virtio-snd] slot {}: handed to a user-space driver",
        d.t.slot()
    );
}

/// Before the kernel resets a device a user-space driver died holding (udrv `quiesce`):
/// qemu's reset doesn't flush a PCM stream's buffers. It completes them later, as it
/// plays them or at the next PREPARE, onto whatever tx queue is live by then, which
/// breaks the next driver's queue (qemu then marks the device broken). So, on the dead
/// driver's own control queue: STOP and RELEASE every stream. RELEASE completes all of a
/// stream's pending buffers (virtio 1.2, 5.14.6.6.5.1), onto the dead driver's tx queue,
/// while it still exists. The requests live in a scratch page of ours; descriptors 0 and
/// 1 of the dead queue are reused (nothing else can be using them by then).
pub fn quiesce(t: &Transport, queues: &[crate::udrv::QueueRegs]) {
    let Some(q) = queues.get(CTLQ as usize).copied().filter(|q| q.num >= 2) else {
        return;
    };
    let rd16 = |pa: u64| {
        // SAFETY: inside the dead driver's DMA buffers (udrv checked), ordinary RAM.
        unsafe { read_volatile(phys_to_virt(pa as PhysAddr) as *const u16) }
    };
    let wr = |pa: u64, v: u64, width: usize| {
        let p = phys_to_virt(pa as PhysAddr);
        // SAFETY: as above.
        unsafe {
            match width {
                2 => write_volatile(p as *mut u16, v as u16),
                4 => write_volatile(p as *mut u32, v as u32),
                _ => write_volatile(p as *mut u64, v),
            }
        }
    };
    // A control request the driver was waiting for completes first.
    let end = timer::now_ns() + 100_000_000;
    let mut avail = rd16(q.avail + 2);
    while rd16(q.used + 2) != avail {
        if timer::now_ns() > end {
            return;
        }
        core::hint::spin_loop();
    }
    let Some(scratch) = pmm::alloc_contiguous_zeroed(1, 1) else {
        return;
    };
    let sp = scratch as u64;
    let streams = t.config_read32(4).min(MAX_STREAMS as u32);
    let mut released = 0;
    'outer: for stream in 0..streams {
        for code in [R_PCM_STOP, R_PCM_RELEASE] {
            w32(va(scratch), 0, code);
            w32(va(scratch), 4, stream);
            w32(va(scratch), 512, 0);
            // desc 0: the request (device reads), chained to desc 1: the status.
            wr(q.desc, sp, 8);
            wr(q.desc + 8, 8, 4);
            wr(q.desc + 12, 1, 2); // NEXT
            wr(q.desc + 14, 1, 2);
            wr(q.desc + 16, sp + 512, 8);
            wr(q.desc + 24, 4, 4);
            wr(q.desc + 28, 2, 2); // WRITE
            wr(q.desc + 30, 0, 2);
            wr(q.avail + 4 + 2 * (avail % q.num as u16) as u64, 0, 2);
            super::mmio::mb();
            avail = avail.wrapping_add(1);
            wr(q.avail + 2, avail as u64, 2);
            t.notify(CTLQ);
            let end = timer::now_ns() + 100_000_000;
            while rd16(q.used + 2) != avail {
                if timer::now_ns() > end {
                    break 'outer;
                }
                core::hint::spin_loop();
            }
            if code == R_PCM_RELEASE && r32(va(scratch), 512) == S_OK {
                released += 1;
            }
        }
    }
    pmm::free_contiguous(scratch, 1);
    kprintln!(
        "[virtio-snd] slot {}: released {released} stream(s) a dead user-space driver left behind",
        t.slot()
    );
}

fn on_irq(irq: u32, _frame: &mut TrapFrame) {
    let slot = (irq - gic::spi(MMIO_SPI_BASE)) as usize;
    if let Ok(t) = Transport::probe(slot) {
        t.ack_interrupt();
    }
    crate::audio::on_device_irq();
}

fn queue(t: &Transport, idx: u16, want: usize) -> Result<Virtqueue, Error> {
    let max = (t.queue_num_max(idx) as usize)
        .min(MAX_QUEUE_SIZE)
        .min(want);
    if max < 2 {
        return Err(Error::BadQueue);
    }
    let size = 1u16 << (usize::BITS - 1 - max.leading_zeros());
    let pa = pmm::alloc_contiguous_zeroed(1, 1).ok_or(Error::BadQueue)?;
    // SAFETY: fresh zeroed frame, identity mapped, owned by this queue from now on.
    let vq = unsafe { Virtqueue::new(phys_to_virt(pa) as *mut QueueMem, size) };
    t.setup_queue(idx, &vq)?;
    Ok(vq)
}

/// Bring up a virtio-sound device. Only the first one is used.
pub fn attach(t: Transport) -> Result<(), Error> {
    if present() {
        kprintln!(
            "[virtio-snd] slot {}: second sound device ignored",
            t.slot()
        );
        t.reset();
        return Ok(());
    }
    t.begin_init(0, 0)?;
    let streams = t.config_read32(4);
    let jacks = t.config_read32(0);
    let ctl = queue(&t, CTLQ, 8)?;
    let mut evt = queue(&t, EVTQ, EVENTS)?;
    let tx = queue(&t, TXQ, 64)?;
    let fail = |t: &Transport| {
        t.fail();
        Error::BadQueue
    };
    let ctl_pa = pmm::alloc_contiguous_zeroed(1, 1).ok_or_else(|| fail(&t))?;
    let slot_pages = (SLOTS * SLOT_STRIDE + EVENTS * 8).div_ceil(PAGE_SIZE);
    let slots_pa = pmm::alloc_contiguous_zeroed(slot_pages, 1).ok_or_else(|| fail(&t))?;
    let ev_pa = slots_pa + SLOTS * SLOT_STRIDE;
    let mut ev_of = [0u8; MAX_QUEUE_SIZE];
    for k in 0..EVENTS {
        let head = evt
            .add(&[], &[((ev_pa + k * 8) as u64, 8)])
            .ok_or_else(|| fail(&t))?;
        ev_of[head as usize] = k as u8;
    }
    if gic::register(t.irq(), on_irq).is_ok() {
        let _ = gic::set_edge_triggered(t.irq(), false);
        let _ = gic::enable(t.irq());
    }
    t.driver_ok();
    t.notify(EVTQ);

    let mut dev = Sound {
        t,
        ctl,
        evt,
        tx,
        ctl_pa,
        slots_pa,
        stream: u32::MAX,
        free: (1 << SLOTS) - 1,
        slot_of: [0; MAX_QUEUE_SIZE],
        ev_of,
        in_flight: 0,
        running: false,
        periods_played: 0,
        xruns: 0,
        ctl_errors: 0,
        tx_heads: 0,
        strays: 0,
    };

    // Find an output stream that can do S16 / 48 kHz / 2 channels.
    let n = (streams as usize).min(MAX_STREAMS);
    let p = va(ctl_pa);
    w32(p, 0, R_PCM_INFO);
    w32(p, 4, 0); // start_id
    w32(p, 8, n as u32); // count
    w32(p, 12, PCM_INFO_SIZE as u32);
    if dev.control(16, 4 + n * PCM_INFO_SIZE) != Ok(S_OK) {
        kprintln!("[virtio-snd] PCM_INFO failed");
        return Err(fail(&t));
    }
    let info = va(ctl_pa + 512 + 4) as *const u8;
    for k in 0..n {
        let e = k * PCM_INFO_SIZE;
        // The response starts 4 bytes after a status word, so the u64 fields are only
        // 4-aligned: read them as two u32s.
        let r64 = |o: usize| r32(info, o) as u64 | (r32(info, o + 4) as u64) << 32;
        let formats = r64(e + 8);
        let rates = r64(e + 16);
        // SAFETY: inside the response area of our control page.
        let (dir, cmin, cmax) = unsafe {
            (
                read_volatile(info.add(e + 24)),
                read_volatile(info.add(e + 25)),
                read_volatile(info.add(e + 26)),
            )
        };
        if dir == DIR_OUTPUT
            && formats & (1 << FMT_S16) != 0
            && rates & (1 << RATE_48000) != 0
            && (cmin as u32..=cmax as u32).contains(&CHANNELS)
        {
            dev.stream = k as u32;
            break;
        }
    }
    if dev.stream == u32::MAX {
        kprintln!("[virtio-snd] no output stream does S16_LE/48000/stereo ({n} streams)");
        return Err(fail(&t));
    }
    // SET_PARAMS: buffer, period, features, channels, format, rate.
    w32(p, 0, R_PCM_SET_PARAMS);
    w32(p, 4, dev.stream);
    w32(p, 8, (PERIODS * PERIOD_BYTES) as u32);
    w32(p, 12, PERIOD_BYTES as u32);
    w32(p, 16, 0);
    // SAFETY: our control page.
    unsafe {
        write_volatile(p.add(20), CHANNELS as u8);
        write_volatile(p.add(21), FMT_S16);
        write_volatile(p.add(22), RATE_48000);
        write_volatile(p.add(23), 0);
    }
    if dev.control(24, 4) != Ok(S_OK) || !dev.pcm_cmd(R_PCM_PREPARE) {
        kprintln!("[virtio-snd] SET_PARAMS/PREPARE failed");
        return Err(fail(&t));
    }
    // A driver that died with buffers queued (a user-space one, docs/drivers.md): PREPARE
    // makes qemu complete them, onto our fresh tx queue. They aren't ours: skip them.
    let mut stale = 0;
    while dev.tx.pop_used().is_some() {
        stale += 1;
    }
    dev.strays += stale;
    kprintln!(
        "[virtio-snd] stream {} of {} (jacks {}): S16_LE {} Hz x{}, {} ms periods x {}",
        dev.stream,
        streams,
        jacks,
        RATE,
        CHANNELS,
        PERIOD_FRAMES * 1000 / RATE as usize,
        PERIODS
    );
    SLOT.store(t.slot(), Ordering::Release);
    *SOUND.lock() = Some(dev);
    Ok(())
}
