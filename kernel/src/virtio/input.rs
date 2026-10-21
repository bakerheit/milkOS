//! virtio-input driver (virtio 1.x, 5.8): `virtio-keyboard-device`, `virtio-tablet-device`
//! (and `virtio-mouse-device`).
//!
//! Each device has an event queue (0) that we keep full of 8-byte `virtio_input_event`
//! buffers. The device fills them with evdev events (EV_KEY / EV_REL / EV_ABS, terminated by
//! EV_SYN); the IRQ handler drains the used ring, forwards the events to [`crate::input`],
//! and re-offers the same buffers. The status queue (1, LEDs) is left unused.
//!
//! Tablet coordinates are scaled from the device's ABS_X / ABS_Y range (qemu: 0..32767) to
//! [`crate::input::screen_size`].

use core::ptr::{addr_of, addr_of_mut, read_volatile};

use super::mmio::{self, Error, Transport};
use super::queue::{QueueMem, Virtqueue};
use crate::exceptions::TrapFrame;
use crate::sync::SpinLock;
use crate::{gic, input, kprintln};

/// How many virtio-input devices we drive at once.
const MAX_DEVS: usize = 4;
/// Event buffers per device (qemu's queue max is 64).
const NUM_EVENTS: usize = 64;

const EVENTQ: u16 = 0;

// Config space (5.8.4).
const CFG_SELECT: usize = 0;
const CFG_SUBSEL: usize = 1;
const CFG_SIZE: usize = 2;
const CFG_DATA: usize = 8;
const CFG_ID_NAME: u8 = 0x01;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;

// evdev event types and codes.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;

/// `struct virtio_input_event`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawEvent {
    ty: u16,
    code: u16,
    value: u32,
}

/// Device-visible memory for one device: its event queue plus the event buffers.
/// TODO(mem): allocate from `pmm` (two contiguous frames: queue + buffers) instead of a static
/// pool. Device addresses go through `mmu::virt_to_phys` (`Virtqueue::addresses`, below).
#[repr(C, align(4096))]
struct DevMem {
    queue: QueueMem,
    events: [RawEvent; NUM_EVENTS],
}

const EMPTY_EVENT: RawEvent = RawEvent {
    ty: 0,
    code: 0,
    value: 0,
};
static mut MEM: [DevMem; MAX_DEVS] = [const {
    DevMem {
        queue: QueueMem::new(),
        events: [EMPTY_EVENT; NUM_EVENTS],
    }
}; MAX_DEVS];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Keyboard,
    Tablet,
    Mouse,
}

#[derive(Clone, Copy)]
struct AbsRange {
    min: i32,
    max: i32,
}

struct Dev {
    t: Transport,
    vq: Virtqueue,
    events: *mut RawEvent,
    abs: [AbsRange; 2],
    // Accumulated since the last SYN_REPORT.
    abs_pending: [Option<i32>; 2],
    rel: [i32; 2],
    wheel: [i32; 2], // (h, v)
}

// SAFETY: `events` points into MEM, owned by this device; access is under DEVS.
unsafe impl Send for Dev {}

static DEVS: SpinLock<[Option<Dev>; MAX_DEVS]> = SpinLock::new([const { None }; MAX_DEVS]);

/// Select a config item; returns its size in bytes.
fn cfg_select(t: &Transport, select: u8, subsel: u8) -> usize {
    t.config_write8(CFG_SELECT, select);
    t.config_write8(CFG_SUBSEL, subsel);
    t.config_read8(CFG_SIZE) as usize
}

fn cfg_u32(t: &Transport, off: usize) -> u32 {
    t.config_read32(CFG_DATA + off)
}

/// Start driving the virtio-input device on `t`.
pub fn attach(t: Transport) -> Result<(), Error> {
    let _g = crate::sync::IrqGuard::new();
    let mut devs = DEVS.lock();
    let Some(n) = devs.iter().position(Option::is_none) else {
        kprintln!("[virtio-input] slot {}: too many input devices", t.slot());
        return Ok(());
    };

    t.begin_init(0, 0)?;

    // Identify it.
    let mut name = [0u8; 64];
    let len = cfg_select(&t, CFG_ID_NAME, 0).min(name.len());
    for (i, b) in name[..len].iter_mut().enumerate() {
        *b = t.config_read8(CFG_DATA + i);
    }
    let len = name[..len].iter().position(|&b| b == 0).unwrap_or(len); // drop the NUL
    let name = core::str::from_utf8(&name[..len]).unwrap_or("?");
    let kind = if cfg_select(&t, CFG_EV_BITS, EV_ABS as u8) > 0 {
        Kind::Tablet
    } else if cfg_select(&t, CFG_EV_BITS, EV_REL as u8) > 0 {
        Kind::Mouse
    } else {
        Kind::Keyboard
    };
    let mut abs = [AbsRange { min: 0, max: 32767 }; 2];
    if kind == Kind::Tablet {
        for (axis, r) in abs.iter_mut().enumerate() {
            if cfg_select(&t, CFG_ABS_INFO, axis as u8) >= 8 {
                r.min = cfg_u32(&t, 0) as i32;
                r.max = (cfg_u32(&t, 4) as i32).max(r.min + 1);
            }
        }
    }

    // Event queue, filled with one device-writable buffer per descriptor.
    let qmax = t.queue_num_max(EVENTQ) as usize;
    let size = NUM_EVENTS.min(qmax);
    if size == 0 {
        t.fail();
        return Err(Error::BadQueue);
    }
    let size = 1u16 << (usize::BITS - 1 - size.leading_zeros()); // round down to a power of 2
    // SAFETY: slot n of MEM belongs to device n, which is not live yet (we hold DEVS).
    let (qmem, events) = unsafe {
        let m = addr_of_mut!(MEM[n]);
        (
            addr_of_mut!((*m).queue),
            addr_of_mut!((*m).events) as *mut RawEvent,
        )
    };
    let mut vq = unsafe { Virtqueue::new(qmem, size) };
    t.setup_queue(EVENTQ, &vq).inspect_err(|_| t.fail())?;
    for i in 0..size as usize {
        let buf = crate::mmu::virt_to_phys(unsafe { events.add(i) } as usize) as u64;
        vq.add(&[], &[(buf, size_of::<RawEvent>() as u32)])
            .expect("fresh queue has room");
    }
    t.driver_ok();
    t.notify(EVENTQ);

    let irq = t.irq();
    devs[n] = Some(Dev {
        t,
        vq,
        events,
        abs,
        abs_pending: [None; 2],
        rel: [0; 2],
        wheel: [0; 2],
    });
    drop(devs);

    // qemu drives the line as a level (it stays up until InterruptACK), so configure it as
    // level even though the DTB says edge: a level line cannot lose an edge.
    gic::register(irq, on_irq).expect("virtio-input IRQ registration");
    gic::set_edge_triggered(irq, false).ok();
    gic::enable(irq).expect("virtio-input IRQ enable");
    kprintln!(
        "[virtio-input] slot {}: \"{}\" as {:?}, {} buffers",
        t.slot(),
        name,
        kind,
        size
    );
    if kind == Kind::Tablet {
        kprintln!(
            "[virtio-input]   ABS_X {}..{}, ABS_Y {}..{} -> screen {:?}",
            abs[0].min,
            abs[0].max,
            abs[1].min,
            abs[1].max,
            input::screen_size()
        );
    }
    Ok(())
}

fn scale(v: i32, r: AbsRange, pixels: i32) -> i32 {
    let span = (r.max - r.min) as i64 + 1;
    ((v - r.min).max(0) as i64 * pixels as i64 / span) as i32
}

impl Dev {
    fn handle(&mut self, ev: RawEvent) {
        match (ev.ty, ev.code) {
            (EV_KEY, code) => input::report_key(code, ev.value as i32),
            (EV_ABS, ABS_X) => self.abs_pending[0] = Some(ev.value as i32),
            (EV_ABS, ABS_Y) => self.abs_pending[1] = Some(ev.value as i32),
            (EV_REL, REL_X) => self.rel[0] += ev.value as i32,
            (EV_REL, REL_Y) => self.rel[1] += ev.value as i32,
            (EV_REL, REL_HWHEEL) => self.wheel[0] += ev.value as i32,
            (EV_REL, REL_WHEEL) => self.wheel[1] += ev.value as i32,
            (EV_SYN, SYN_REPORT) => self.sync(),
            _ => {}
        }
    }

    /// End of one report: publish the accumulated motion.
    fn sync(&mut self) {
        if self.abs_pending != [None, None] {
            let (w, h) = input::screen_size();
            let (cx, cy) = input::pointer();
            let x = self.abs_pending[0].map_or(cx, |v| scale(v, self.abs[0], w));
            let y = self.abs_pending[1].map_or(cy, |v| scale(v, self.abs[1], h));
            self.abs_pending = [None, None];
            input::report_pointer(x, y);
        }
        if self.rel != [0, 0] {
            input::report_pointer_rel(self.rel[0], self.rel[1]);
            self.rel = [0, 0];
        }
        if self.wheel != [0, 0] {
            input::report_scroll(self.wheel[0], self.wheel[1]);
            self.wheel = [0, 0];
        }
    }

    fn drain(&mut self) {
        let mut requeued = false;
        while let Some((head, len)) = self.vq.pop_used() {
            if (head as usize) < self.vq.size() as usize && len as usize >= size_of::<RawEvent>() {
                // SAFETY: head < size; the device has finished writing this buffer.
                // Field by field: each load naturally aligned (Device memory, MMU off).
                let ev = unsafe {
                    let e = self.events.add(head as usize);
                    RawEvent {
                        ty: read_volatile(addr_of!((*e).ty)),
                        code: read_volatile(addr_of!((*e).code)),
                        value: read_volatile(addr_of!((*e).value)),
                    }
                };
                self.handle(ev);
            }
            self.vq.requeue(head);
            requeued = true;
        }
        if requeued && self.vq.should_notify() {
            self.t.notify(EVENTQ);
        }
    }
}

fn on_irq(irq: u32, _frame: &mut TrapFrame) {
    let mut devs = DEVS.lock();
    for dev in devs.iter_mut().flatten() {
        if dev.t.irq() != irq {
            continue;
        }
        let cause = dev.t.ack_interrupt();
        if cause & mmio::INT_USED_RING != 0 {
            dev.drain();
        }
    }
}
