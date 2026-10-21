//! soundd: the virtio-sound driver as a user-space process (docs/drivers.md).
//!
//! The mixer stays in the kernel; soundd is the device side. It claims the device (the
//! kernel driver lets go), finds an S16_LE / 48 kHz / stereo output stream, sets 10 ms
//! periods with a 4-period device buffer, then loops: take mixed periods from /dev/pcm,
//! queue them on the tx queue, START once 4 are queued (or nothing more is coming), report
//! each played period back to /dev/pcm (that paces the mixer), STOP 200 ms after the mixer
//! runs dry. If soundd dies the kernel resets the device and its own driver carries on;
//! milkd restarts soundd, which takes over again.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use core::ptr::{read_volatile, write_volatile};
use rt::net::{self, PollFd};
use rt::{eprintln, println, sys};
use udrv::{Device, Queue, Res, VIRTIO_SOUND};

rt::main!(main);

const CTLQ: u16 = 0;
const EVTQ: u16 = 1;
const TXQ: u16 = 2;
const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;
const EVT_PCM_XRUN: u32 = 0x1101;
const DIR_OUTPUT: u8 = 0;
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;
const CHANNELS: u32 = 2;
const PERIOD_BYTES: usize = 480 * 4;
const PERIODS: usize = 4;
const SLOTS: usize = 8;
const SLOT_STRIDE: usize = 2048;
const SLOT_DATA: usize = 64;
const PCM_INFO_SIZE: usize = 32;
const MAX_STREAMS: usize = 32;
const EVENTS: usize = 4;
const IDLE_STOP_MS: u64 = 200;

fn w32(va: usize, off: usize, v: u32) {
    // SAFETY: offsets inside our own DMA pages, 4-aligned.
    unsafe { write_volatile((va + off) as *mut u32, v.to_le()) }
}
fn r32(va: usize, off: usize) -> u32 {
    // SAFETY: as in w32.
    u32::from_le(unsafe { read_volatile((va + off) as *const u32) })
}
fn w8(va: usize, off: usize, v: u8) {
    // SAFETY: as in w32.
    unsafe { write_volatile((va + off) as *mut u8, v) }
}
fn r8(va: usize, off: usize) -> u8 {
    // SAFETY: as in w32.
    unsafe { read_volatile((va + off) as *const u8) }
}

fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

struct Snd {
    dev: Device,
    ctl: Queue,
    evt: Queue,
    tx: Queue,
    /// Control page: request at 0, response at 512.
    ctl_mem: (usize, u64),
    /// SLOTS period slots, then EVENTS 8-byte event buffers.
    slots: (usize, u64),
    stream: u32,
    free: u32,
    slot_of: [u8; 64],
    ev_of: [u8; 64],
    in_flight: usize,
    running: bool,
    played: u64,
    xruns: u64,
    /// Bit h: we submitted a chain with head h that the device hasn't returned.
    tx_heads: u64,
    strays: u64,
}

impl Snd {
    /// Take the device's interrupt if it fired (non-blocking).
    fn service_irq(&self, wait_ms: u64) {
        let mut pf = [PollFd {
            fd: self.dev.fd,
            events: net::POLLIN,
            revents: 0,
        }];
        if matches!(net::poll(&mut pf, Some(wait_ms)), Ok(1..)) {
            let _ = self.dev.wait();
            self.dev.ack_and_arm();
        }
    }

    fn control(&mut self, req_len: usize, resp_len: usize) -> Res<u32> {
        let (va, pa) = self.ctl_mem;
        w32(va, 512, 0);
        let head = self
            .ctl
            .add(&[(pa, req_len as u32)], &[(pa + 512, resp_len as u32)])
            .ok_or("control queue full")?;
        self.dev.notify(CTLQ)?;
        let t0 = now_ms();
        loop {
            if let Some((id, _)) = self.ctl.pop_used() {
                self.ctl.free(id);
                if id == head {
                    return Ok(r32(va, 512));
                }
            }
            if now_ms() - t0 > 1000 {
                return Err("control request timed out".into());
            }
            self.service_irq(5);
        }
    }

    fn pcm_cmd(&mut self, code: u32) -> bool {
        let va = self.ctl_mem.0;
        w32(va, 0, code);
        w32(va, 4, self.stream);
        matches!(self.control(8, 4), Ok(S_OK))
    }

    fn start(&mut self) {
        if !self.running {
            let mut ok = self.pcm_cmd(R_PCM_START);
            if !ok {
                ok = self.pcm_cmd(R_PCM_PREPARE) && self.pcm_cmd(R_PCM_START);
            }
            self.running = ok;
        }
    }

    fn stop(&mut self) {
        if self.running {
            self.pcm_cmd(R_PCM_STOP);
            self.running = false;
        }
    }

    /// Played periods back from the device, and events. Returns how many periods.
    fn reclaim(&mut self) -> Res<u32> {
        let mut n = 0;
        while let Some((head, _)) = self.tx.pop_used() {
            if head >= 64 || self.tx_heads & (1 << head) == 0 {
                // Not one of ours (left over from before a reset?): never free it.
                self.strays += 1;
                if self.strays <= 5 {
                    eprintln!("soundd: the device returned tx head {head} we never queued");
                }
                continue;
            }
            self.tx_heads &= !(1 << head);
            let s = self.slot_of[head as usize] as usize;
            self.tx.free(head);
            self.free |= 1 << s;
            self.in_flight -= 1;
            self.played += 1;
            n += 1;
        }
        let ev = self.slots.0 + SLOTS * SLOT_STRIDE;
        let ev_pa = self.slots.1 + (SLOTS * SLOT_STRIDE) as u64;
        let mut requeued = false;
        while let Some((head, _)) = self.evt.pop_used() {
            let k = self.ev_of[head as usize] as usize;
            if r32(ev, k * 8) == EVT_PCM_XRUN {
                self.xruns += 1;
            }
            w32(ev, k * 8, 0);
            self.evt.free(head);
            if let Some(h) = self.evt.add(&[], &[(ev_pa + (k * 8) as u64, 8)]) {
                self.ev_of[h as usize] = k as u8;
                requeued = true;
            }
        }
        if requeued {
            self.dev.notify(EVTQ)?;
        }
        Ok(n)
    }

    fn can_submit(&self) -> bool {
        self.free != 0 && self.in_flight < PERIODS
    }

    /// Queue one period read from /dev/pcm into a free slot.
    fn submit_from(&mut self, pcm: i32) -> Res<bool> {
        let s = self.free.trailing_zeros() as usize;
        let va = self.slots.0 + s * SLOT_STRIDE;
        let pa = self.slots.1 + (s * SLOT_STRIDE) as u64;
        // SAFETY: slot s is free (the device doesn't have it), SLOT_STRIDE bytes.
        let data =
            unsafe { core::slice::from_raw_parts_mut((va + SLOT_DATA) as *mut u8, PERIOD_BYTES) };
        match sys::read(pcm, data) {
            Ok(n) if n == PERIOD_BYTES => {}
            Ok(_) => return Ok(false),
            Err(e) => return Err(alloc::format!("/dev/pcm: {e}")),
        }
        w32(va, 0, self.stream); // virtio_snd_pcm_xfer
        w32(va, 16, 0); // status
        w32(va, 20, 0); // latency_bytes
        let out = [(pa, 4u32), (pa + SLOT_DATA as u64, PERIOD_BYTES as u32)];
        let inb = [(pa + 16, 8u32)];
        let head = self.tx.add(&out, &inb).ok_or("tx queue full")?;
        self.free &= !(1 << s);
        self.slot_of[head as usize] = s as u8;
        self.tx_heads |= 1 << head;
        self.in_flight += 1;
        if self.tx.should_notify() {
            self.dev.notify(TXQ)?;
        }
        Ok(true)
    }
}

fn queue(dev: &Device, idx: u16, want: usize) -> Res<Queue> {
    let max = (dev.queue_max(idx) as usize).min(udrv::MAX_QUEUE).min(want);
    if max < 2 {
        return Err(alloc::format!("queue {idx} is too small"));
    }
    let size = 1u16 << (usize::BITS - 1 - max.leading_zeros());
    let q = Queue::new(dev, size)?;
    dev.setup_queue(idx, &q)?;
    Ok(q)
}

fn bring_up() -> Res<Snd> {
    let dev = Device::claim(VIRTIO_SOUND)?;
    dev.init(0)?;
    let jacks = dev.config_r32(0);
    let streams = dev.config_r32(4);
    let ctl = queue(&dev, CTLQ, 8)?;
    let mut evt = queue(&dev, EVTQ, EVENTS)?;
    let tx = queue(&dev, TXQ, 64)?;
    let ctl_mem = dev.dma(4096)?;
    let slots = dev.dma(SLOTS * SLOT_STRIDE + EVENTS * 8)?;
    let ev_pa = slots.1 + (SLOTS * SLOT_STRIDE) as u64;
    let mut ev_of = [0u8; 64];
    for k in 0..EVENTS {
        let h = evt
            .add(&[], &[(ev_pa + (k * 8) as u64, 8)])
            .ok_or("event queue full")?;
        ev_of[h as usize] = k as u8;
    }
    dev.driver_ok()?;
    let _ = sys::driver_arm(dev.fd);
    dev.notify(EVTQ)?;
    let mut s = Snd {
        dev,
        ctl,
        evt,
        tx,
        ctl_mem,
        slots,
        stream: u32::MAX,
        free: (1 << SLOTS) - 1,
        slot_of: [0; 64],
        ev_of,
        in_flight: 0,
        running: false,
        played: 0,
        xruns: 0,
        tx_heads: 0,
        strays: 0,
    };
    // An output stream that does S16 / 48 kHz / stereo.
    let n = (streams as usize).min(MAX_STREAMS);
    let va = ctl_mem.0;
    w32(va, 0, R_PCM_INFO);
    w32(va, 4, 0);
    w32(va, 8, n as u32);
    w32(va, 12, PCM_INFO_SIZE as u32);
    if s.control(16, 4 + n * PCM_INFO_SIZE)? != S_OK {
        return Err("PCM_INFO failed".into());
    }
    let info = va + 512 + 4;
    for k in 0..n {
        let e = info + k * PCM_INFO_SIZE;
        let r64 = |o: usize| r32(e, o) as u64 | (r32(e, o + 4) as u64) << 32;
        let (formats, rates) = (r64(8), r64(16));
        let (dir, cmin, cmax) = (r8(e, 24), r8(e, 25), r8(e, 26));
        if dir == DIR_OUTPUT
            && formats & (1 << FMT_S16) != 0
            && rates & (1 << RATE_48000) != 0
            && (cmin as u32..=cmax as u32).contains(&CHANNELS)
        {
            s.stream = k as u32;
            break;
        }
    }
    if s.stream == u32::MAX {
        return Err(alloc::format!(
            "no output stream does S16_LE/48000/stereo ({n} streams)"
        ));
    }
    w32(va, 0, R_PCM_SET_PARAMS);
    w32(va, 4, s.stream);
    w32(va, 8, (PERIODS * PERIOD_BYTES) as u32);
    w32(va, 12, PERIOD_BYTES as u32);
    w32(va, 16, 0);
    w8(va, 20, CHANNELS as u8);
    w8(va, 21, FMT_S16);
    w8(va, 22, RATE_48000);
    w8(va, 23, 0);
    if s.control(24, 4)? != S_OK || !s.pcm_cmd(R_PCM_PREPARE) {
        return Err("SET_PARAMS/PREPARE failed".into());
    }
    // qemu completes buffers an earlier driver left queued when we PREPARE, onto our new
    // tx queue (its reset doesn't flush them). Not ours: skip them.
    while s.tx.pop_used().is_some() {
        s.strays += 1;
    }
    println!(
        "soundd: virtio-sound ({}) is ours: stream {} of {streams} (jacks {jacks}), S16_LE 48000 Hz x2, 10 ms periods x {PERIODS}",
        s.dev.info(),
        s.stream
    );
    Ok(s)
}

fn run() -> Res<()> {
    let pcm = sys::open("/dev/pcm", sys::O_RDWR | sys::O_CLOEXEC)
        .map_err(|e| alloc::format!("/dev/pcm: {e}"))?;
    let mut s = bring_up()?;
    let mut last_data = now_ms();
    loop {
        let n = s.reclaim()?;
        if n > 0 {
            let _ = sys::write(pcm, &n.to_le_bytes());
        }
        // Fill the device from the mixer while there is room and data.
        let mut more = false;
        while s.can_submit() {
            let mut pf = [PollFd {
                fd: pcm,
                events: net::POLLIN,
                revents: 0,
            }];
            if !matches!(net::poll(&mut pf, Some(0)), Ok(1..)) {
                break;
            }
            if s.submit_from(pcm)? {
                last_data = now_ms();
                more = true;
            }
        }
        if !s.running && s.in_flight > 0 && (s.in_flight >= PERIODS || !more) {
            s.start();
        }
        if s.running && s.in_flight == 0 && now_ms() - last_data > IDLE_STOP_MS {
            s.stop();
        }
        // Sleep until the device returns a period or the mixer has one (only if there is
        // room for it, else we'd spin).
        let mut pf = [
            PollFd {
                fd: s.dev.fd,
                events: net::POLLIN,
                revents: 0,
            },
            PollFd {
                fd: pcm,
                events: if s.can_submit() { net::POLLIN } else { 0 },
                revents: 0,
            },
        ];
        let wait = if s.running || s.in_flight > 0 {
            50
        } else {
            1000
        };
        if let Ok(1..) = net::poll(&mut pf, Some(wait))
            && pf[0].revents & net::POLLIN != 0
        {
            let _ = s.dev.wait();
            s.dev.ack_and_arm();
        }
        if pf[0].revents & (net::POLLERR | net::POLLHUP) != 0 {
            return Err(String::from("the device was taken away"));
        }
    }
}

fn main(_args: &rt::Args) -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("soundd: {e}");
            1
        }
    }
}
