//! The xHCI host controller (xHCI specification 1.1), polled.
//!
//! Memory: the device context base array, the command ring, one event ring (one segment of 256 TRBs)
//! and per device an output context, an input context, a control ring, an interrupt ring and a
//! buffer, all from `dma::Buffer` (zeroed, below the board's DMA limit, synced around every
//! hand-over). Commands and control transfers are synchronous: queue, ring the doorbell, poll the
//! event ring for the matching completion with a deadline. Interrupt IN endpoints (HID) keep one
//! transfer queued and are re-armed from the polling loop.
//!
//! Every poll is bounded ([`Timeout`]); a controller that stops answering ends the operation with an
//! error, and a device that sends garbage is dropped. Devices are looked after in [`Controller::poll`],
//! which the USB thread calls every few milliseconds.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use rpikit::usb::{self as u, Event, Trb, code, portsc, speed};

use super::hid::Hid;
use crate::dma::Buffer;
use crate::kprintln;
use crate::timeout::Timeout;

const TRBS: usize = 256;
const TRB_BYTES: usize = 16;
/// Most device slots we use.
const MAX_SLOTS: usize = 16;

// Operational registers (offsets from the operational base).
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const PAGESIZE: usize = 0x08;
const CRCR: usize = 0x18;
const DCBAAP: usize = 0x30;
const CONFIG: usize = 0x38;
const PORTSC_BASE: usize = 0x400;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const STS_HALTED: u32 = 1 << 0;
const STS_NOT_READY: u32 = 1 << 11;

/// A producer ring (command or transfer): one segment, the last TRB a link back to the start.
struct Ring {
    mem: Buffer,
    enq: usize,
    cycle: bool,
}

impl Ring {
    fn new() -> Option<Ring> {
        let mem = Buffer::new(1, 1)?;
        Some(Ring {
            mem,
            enq: 0,
            cycle: true,
        })
    }

    fn pa(&self) -> u64 {
        self.mem.pa()
    }

    /// Queue `t`; returns the physical address of the TRB it went in. The caller syncs and rings the
    /// doorbell.
    fn push(&mut self, t: Trb) -> u64 {
        let at = self.enq * TRB_BYTES;
        let w = t.with_cycle(self.cycle).words();
        // Write the control word (with the cycle bit) last: the controller must not see a half-written TRB.
        for (i, v) in w.iter().take(3).enumerate() {
            self.mem.write32(at + i * 4, *v);
        }
        self.mem.write32(at + 12, w[3]);
        let pa = self.mem.pa() + at as u64;
        self.enq += 1;
        if self.enq == TRBS - 1 {
            let link = u::link(self.mem.pa(), true, self.cycle).words();
            let at = self.enq * TRB_BYTES;
            for (i, v) in link.iter().enumerate() {
                self.mem.write32(at + i * 4, *v);
            }
            self.cycle = !self.cycle;
            self.enq = 0;
        }
        self.mem.sync(0, TRBS * TRB_BYTES);
        pa
    }
}

/// The consumer side of the one event ring.
struct EventRing {
    mem: Buffer,
    /// The segment table the controller reads: kept alive for as long as the ring is.
    _erst: Buffer,
    deq: usize,
    cycle: bool,
}

impl EventRing {
    fn next(&mut self) -> Option<Trb> {
        let at = self.deq * TRB_BYTES;
        self.mem.sync(at, TRB_BYTES);
        let w = [
            self.mem.read32(at),
            self.mem.read32(at + 4),
            self.mem.read32(at + 8),
            self.mem.read32(at + 12),
        ];
        let t = Trb::from_words(w);
        if t.cycle() != self.cycle {
            return None;
        }
        self.deq += 1;
        if self.deq == TRBS {
            self.deq = 0;
            self.cycle = !self.cycle;
        }
        Some(t)
    }

    fn deq_pa(&self) -> u64 {
        self.mem.pa() + (self.deq * TRB_BYTES) as u64
    }
}

/// One enumerated device.
struct Slot {
    id: u8,
    port: u8,
    out_ctx: Buffer,
    in_ctx: Buffer,
    ep0: Ring,
    buf: Buffer,
    /// HID interfaces being polled: interrupt ring, report buffer, device context index.
    hid: Vec<HidEndpoint>,
}

struct HidEndpoint {
    dci: u8,
    ring: Ring,
    report: Buffer,
    /// Bytes asked of each transfer (a boot report is at most 8).
    req_len: usize,
    dev: Hid,
}

pub struct Controller {
    op: usize,
    rt: usize,
    db: usize,
    /// Context size is 64 bytes (HCCPARAMS1.CSZ).
    csz: bool,
    max_ports: u8,
    dcbaa: Buffer,
    _scratch: Vec<Buffer>,
    cmd: Ring,
    events: EventRing,
    slots: Vec<Slot>,
    /// Ports we have looked at since they connected (index = port - 1): don't enumerate twice.
    attached: [bool; 32],
    /// When (ms) each port was first seen connected, 0 if it is not: a device gets 100 ms to settle
    /// (USB's connect debounce) before the port is reset.
    connected_since: [u64; 32],
    started: bool,
}

fn rd32(a: usize) -> u32 {
    // SAFETY: controller registers, mapped as Device memory.
    unsafe { read_volatile(a as *const u32) }
}
fn wr32(a: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile(a as *mut u32, v) }
}
fn wr64(a: usize, v: u64) {
    wr32(a, v as u32);
    wr32(a + 4, (v >> 32) as u32);
}

fn wait(ms: u64, mut ok: impl FnMut() -> bool) -> bool {
    let t = Timeout::ms(ms);
    loop {
        if ok() {
            return true;
        }
        if t.expired() {
            return false;
        }
    }
}

/// Why an operation failed; only ever printed.
#[derive(Debug)]
#[allow(dead_code)] // the payloads are read by Debug, in the log
enum Error {
    Timeout,
    Failed(u8),
    NoMemory,
    Device(&'static str),
}

impl Controller {
    /// Reset and start the controller whose registers are at `va`.
    pub fn new(va: usize, _len: usize) -> Option<Controller> {
        let caplength = (rd32(va) & 0xff) as usize;
        let version = rd32(va) >> 16;
        let hcs1 = rd32(va + 4);
        let hcs2 = rd32(va + 8);
        let hcc1 = rd32(va + 0x10);
        let dboff = (rd32(va + 0x14) & !3) as usize;
        let rtsoff = (rd32(va + 0x18) & !0x1f) as usize;
        if caplength < 0x10 || version == 0 || version == 0xffff {
            kprintln!(
                "[usb] xHCI registers read {:#x}: not a controller",
                rd32(va)
            );
            return None;
        }
        let op = va + caplength;
        let max_slots = ((hcs1 & 0xff) as usize).min(MAX_SLOTS);
        let max_ports = ((hcs1 >> 24) & 0xff) as u8;
        let csz = hcc1 & (1 << 2) != 0;
        let scratch_n = ((hcs2 >> 27) & 0x1f) as usize | (((hcs2 >> 21) & 0x1f) as usize) << 5;
        // Stop it if it runs (a firmware's), then reset.
        if rd32(op + USBSTS) & STS_HALTED == 0 {
            wr32(op + USBCMD, rd32(op + USBCMD) & !CMD_RUN);
            if !wait(500, || rd32(op + USBSTS) & STS_HALTED != 0) {
                kprintln!("[usb] the controller does not halt");
                return None;
            }
        }
        wr32(op + USBCMD, CMD_RESET);
        if !wait(1000, || {
            rd32(op + USBCMD) & CMD_RESET == 0 && rd32(op + USBSTS) & STS_NOT_READY == 0
        }) {
            kprintln!("[usb] the controller does not come out of reset");
            return None;
        }
        if rd32(op + PAGESIZE) & 1 == 0 {
            kprintln!("[usb] the controller does not do 4 KiB pages");
            return None;
        }
        let dcbaa = Buffer::new(1, 1)?;
        // Scratchpad buffers the controller wants for itself.
        let mut scratch = Vec::new();
        if scratch_n > 0 {
            let list = Buffer::new(1, 1)?;
            for i in 0..scratch_n.min(512) {
                let b = Buffer::new(1, 1)?;
                list.write64(i * 8, b.pa());
                scratch.push(b);
            }
            list.sync(0, 4096);
            dcbaa.write64(0, list.pa());
            scratch.push(list);
        }
        dcbaa.sync(0, 4096);
        wr32(op + CONFIG, max_slots as u32);
        wr64(op + DCBAAP, dcbaa.pa());
        let cmd = Ring::new()?;
        wr64(op + CRCR, cmd.pa() | 1);
        // The event ring: one segment, one ERST entry.
        let ev = Buffer::new(1, 1)?;
        let erst = Buffer::new(1, 1)?;
        erst.write64(0, ev.pa());
        erst.write32(8, TRBS as u32);
        erst.sync(0, 16);
        let rt = va + rtsoff;
        let ir = rt + 0x20;
        wr32(ir + 8, 1); // ERSTSZ
        wr64(ir + 0x18, ev.pa()); // ERDP
        wr64(ir + 0x10, erst.pa()); // ERSTBA, last
        wr32(ir, 1); // IMAN: clear a pending bit; interrupts stay off (polled)
        wr32(op + USBCMD, CMD_RUN);
        if !wait(500, || rd32(op + USBSTS) & STS_HALTED == 0) {
            kprintln!("[usb] the controller does not start");
            return None;
        }
        kprintln!(
            "[usb] xHCI {}.{:x}: {} ports, {} slots, {} byte contexts, {} scratchpad pages",
            version >> 8,
            version & 0xff,
            max_ports,
            max_slots,
            if csz { 64 } else { 32 },
            scratch_n
        );
        Some(Controller {
            op,
            rt,
            db: va + dboff,
            csz,
            max_ports: max_ports.min(32),
            dcbaa,
            _scratch: scratch,
            cmd,
            events: EventRing {
                mem: ev,
                _erst: erst,
                deq: 0,
                cycle: true,
            },
            slots: Vec::new(),
            attached: [false; 32],
            connected_since: [0; 32],
            started: false,
        })
    }

    fn portsc_addr(&self, port: u8) -> usize {
        self.op + PORTSC_BASE + 0x10 * (port as usize - 1)
    }

    /// Ring the command ring's doorbell.
    fn ring_command(&self) {
        wr32(self.db, 0);
    }

    /// Ring a device slot's doorbell for endpoint `dci` (1 = the control endpoint).
    fn ring_slot(&self, slot: u8, dci: u8) {
        wr32(self.db + 4 * slot as usize, dci as u32);
    }

    /// Tell the controller how far the event ring has been read.
    fn event_done(&self) {
        let ir = self.rt + 0x20;
        wr64(ir + 0x18, self.events.deq_pa() | 8);
    }

    /// Wait for the command completion for the TRB at `trb_pa`; other events seen meanwhile are
    /// handled as they come (`stash`).
    fn wait_command(&mut self, trb_pa: u64) -> Result<(u8, u8), Error> {
        let t = Timeout::ms(1000);
        loop {
            while let Some(ev) = self.events.next() {
                self.event_done();
                match u::decode_event(&ev) {
                    Event::CommandCompletion { trb, code, slot } if trb == trb_pa => {
                        return Ok((code, slot));
                    }
                    other => self.stash(other),
                }
            }
            if t.expired() {
                return Err(Error::Timeout);
            }
        }
    }

    /// An event nobody was waiting for: transfer events of the HID endpoints are handled right away.
    fn stash(&mut self, ev: Event) {
        if let Event::Transfer {
            slot,
            dci,
            code,
            residual,
            ..
        } = ev
        {
            self.hid_event(slot, dci, code, residual);
        }
    }

    fn command(&mut self, t: Trb) -> Result<(u8, u8), Error> {
        let pa = self.cmd.push(t);
        self.ring_command();
        let (code, slot) = self.wait_command(pa)?;
        if code != code::SUCCESS {
            return Err(Error::Failed(code));
        }
        Ok((code, slot))
    }

    /// A control transfer on the default endpoint of slot index `si`. `data` = the buffer's
    /// (offset in the slot's buffer, length) when the request has a data stage. Returns the bytes
    /// received for an IN request.
    fn control(&mut self, si: usize, s: u::Setup) -> Result<usize, Error> {
        let slot = self
            .slots
            .get_mut(si)
            .ok_or(Error::Device("no such slot"))?;
        let id = slot.id;
        let len = s.length as u32;
        if len as usize > slot.buf.len() {
            return Err(Error::Device("transfer too big"));
        }
        slot.ep0.push(u::setup_stage(s, true));
        if len > 0 {
            slot.buf.sync(0, len as usize);
            slot.ep0
                .push(u::data_stage(slot.buf.pa(), len, s.is_in(), true));
        }
        let status_pa = slot.ep0.push(u::status_stage(len == 0 || !s.is_in(), true));
        self.ring_slot(id, 1);
        let t = Timeout::ms(1000);
        loop {
            while let Some(ev) = self.events.next() {
                self.event_done();
                match u::decode_event(&ev) {
                    Event::Transfer {
                        trb,
                        code,
                        residual,
                        slot: sid,
                        ..
                    } if trb == status_pa && sid == id => {
                        let slot = &self.slots[si];
                        slot.buf.sync(0, len as usize);
                        return match code {
                            code::SUCCESS | code::SHORT_PACKET => {
                                Ok((len as usize).saturating_sub(residual as usize))
                            }
                            c => Err(Error::Failed(c)),
                        };
                    }
                    other => self.stash(other),
                }
            }
            if t.expired() {
                return Err(Error::Timeout);
            }
        }
    }

    /// Write `words` into `buf` starting at dword `at`.
    fn put(&self, buf: &Buffer, at: usize, words: &[u32]) {
        for (i, w) in words.iter().enumerate() {
            buf.write32((at + i) * 4, *w);
        }
    }

    /// Reset the port, wait for it to be enabled, return its speed.
    fn reset_port(&self, port: u8) -> Option<u8> {
        let a = self.portsc_addr(port);
        let v = rd32(a);
        if v & portsc::PP == 0 {
            wr32(a, portsc::write_clearing(v, 0) | portsc::PP);
            crate::timer::delay_ms(20);
        }
        wr32(a, portsc::write_reset(rd32(a)));
        // Reset done: PRC set (or, for a USB 3 port that trained by itself, already enabled).
        if !wait(500, || rd32(a) & portsc::PRC != 0) {
            return None;
        }
        wr32(
            a,
            portsc::write_clearing(
                rd32(a),
                portsc::PRC | portsc::CSC | portsc::PEC | portsc::WRC,
            ),
        );
        crate::timer::delay_ms(10);
        let v = rd32(a);
        if v & portsc::PED == 0 || v & portsc::CCS == 0 {
            return None;
        }
        Some(portsc::speed(v))
    }

    /// Bring up the device on `port`: slot, address, descriptors, and its HID boot interfaces.
    fn enumerate(&mut self, port: u8) -> Result<(), Error> {
        let spd = self
            .reset_port(port)
            .ok_or(Error::Device("port reset failed"))?;
        if !matches!(spd, speed::LOW | speed::FULL | speed::HIGH | speed::SUPER) {
            return Err(Error::Device("unknown port speed"));
        }
        let (_, id) = self.command(u::enable_slot(true))?;
        if id == 0 || id as usize > MAX_SLOTS {
            return Err(Error::Device("bad slot id"));
        }
        let slot = Slot {
            id,
            port,
            out_ctx: Buffer::new(1, 1).ok_or(Error::NoMemory)?,
            in_ctx: Buffer::new(1, 1).ok_or(Error::NoMemory)?,
            ep0: Ring::new().ok_or(Error::NoMemory)?,
            buf: Buffer::new(1, 1).ok_or(Error::NoMemory)?,
            hid: Vec::new(),
        };
        // Address Device: slot context + EP0, the device context pointer.
        let mut mps = u::default_max_packet0(spd);
        self.put(&slot.in_ctx, 0, &[0, 0b11]);
        let mut sl = [0u32; 8];
        u::fill_slot(&mut sl, spd, 1, port);
        self.put(&slot.in_ctx, u::input_slot_offset(self.csz), &sl);
        let mut e0 = [0u32; 8];
        u::fill_ep0(&mut e0, mps, slot.ep0.pa());
        self.put(&slot.in_ctx, u::input_ep_offset(self.csz, 1), &e0);
        slot.in_ctx.sync(0, 4096);
        self.dcbaa.write64(id as usize * 8, slot.out_ctx.pa());
        self.dcbaa.sync(0, 4096);
        let in_pa = slot.in_ctx.pa();
        self.slots.push(slot);
        let si = self.slots.len() - 1;
        let cleanup = |c: &mut Controller, e: Error| -> Result<(), Error> {
            let _ = c.command(u::disable_slot(id, true));
            c.dcbaa.write64(id as usize * 8, 0);
            c.dcbaa.sync(0, 4096);
            c.slots.retain(|s| s.id != id);
            Err(e)
        };
        if let Err(e) = self.command(u::address_device(in_pa, id, false, true)) {
            return cleanup(self, e);
        }
        // The first 8 bytes of the device descriptor say the real control packet size.
        let first = match self.control(si, u::req::get_descriptor(u::req::DESC_DEVICE, 0, 8)) {
            Ok(n) if n >= 8 => {
                let mut d = [0u8; 8];
                self.slots[si].buf.read_bytes(0, &mut d);
                u::parse_device(&d)
            }
            Ok(_) => None,
            Err(e) => return cleanup(self, e),
        };
        let Some(first) = first else {
            return cleanup(self, Error::Device("no device descriptor"));
        };
        let real = if spd == speed::SUPER {
            1u16 << first.max_packet0.min(15)
        } else {
            first.max_packet0 as u16
        };
        if real != 0 && real != mps && matches!(real, 8 | 16 | 32 | 64 | 512) {
            mps = real;
            // Evaluate Context: EP0's max packet size.
            let slot = &self.slots[si];
            self.put(&slot.in_ctx, 0, &[0, 0b10]);
            let mut e0 = [0u32; 8];
            u::fill_ep0(&mut e0, mps, slot.ep0.pa());
            self.put(&slot.in_ctx, u::input_ep_offset(self.csz, 1), &e0);
            slot.in_ctx.sync(0, 4096);
            let in_pa = slot.in_ctx.pa();
            if let Err(e) = self.command(u::evaluate_context(in_pa, id, true)) {
                return cleanup(self, e);
            }
        }
        let n = match self.control(si, u::req::get_descriptor(u::req::DESC_DEVICE, 0, 18)) {
            Ok(n) => n,
            Err(e) => return cleanup(self, e),
        };
        let mut d = [0u8; 18];
        self.slots[si].buf.read_bytes(0, &mut d[..n.min(18)]);
        let dev = u::parse_device(&d[..n.min(18)]);
        // The configuration: its header first (the total length), then all of it.
        let nh = match self.control(si, u::req::get_descriptor(u::req::DESC_CONFIG, 0, 9)) {
            Ok(n) => n,
            Err(e) => return cleanup(self, e),
        };
        let mut head = [0u8; 9];
        self.slots[si].buf.read_bytes(0, &mut head[..nh.min(9)]);
        let total = u::config_total_length(&head[..nh.min(9)])
            .unwrap_or(0)
            .min(1024);
        if total < 9 {
            return cleanup(self, Error::Device("no configuration descriptor"));
        }
        let nc = match self.control(si, u::req::get_descriptor(u::req::DESC_CONFIG, 0, total)) {
            Ok(n) => n,
            Err(e) => return cleanup(self, e),
        };
        let mut cfg = alloc::vec![0u8; nc.min(1024)];
        self.slots[si].buf.read_bytes(0, &mut cfg);
        let mut hids = [u::HidBoot {
            config: 0,
            interface: 0,
            kind: u::HidKind::Keyboard,
            ep_addr: 0,
            max_packet: 0,
            interval: 0,
        }; u::MAX_HID];
        let nh = u::find_hid_boot(&cfg, &mut hids);
        let (vendor, product, class) = dev.map_or((0, 0, 0), |d| (d.vendor, d.product, d.class));
        if nh == 0 {
            kprintln!(
                "[usb] port {port}: {vendor:04x}:{product:04x} class {class:#x} ({} speed): no keyboard or mouse interface, ignored",
                speed_name(spd)
            );
            return cleanup(self, Error::Device("nothing we drive"));
        }
        if let Err(e) = self.control(si, u::req::set_configuration(hids[0].config)) {
            return cleanup(self, e);
        }
        for h in &hids[..nh] {
            if let Err(e) = self.start_hid(si, spd, h) {
                kprintln!(
                    "[usb] port {port}: could not start interface {}: {e:?}",
                    h.interface
                );
            }
        }
        if self.slots[si].hid.is_empty() {
            return cleanup(self, Error::Device("no endpoint started"));
        }
        kprintln!(
            "[usb] port {port}: {vendor:04x}:{product:04x} ({} speed), slot {id}: {}",
            speed_name(spd),
            self.slots[si]
                .hid
                .iter()
                .map(|h| h.dev.name())
                .collect::<Vec<_>>()
                .join(" + ")
        );
        Ok(())
    }

    /// Configure the interrupt IN endpoint of a HID boot interface, put the device in boot protocol,
    /// and queue the first report transfer.
    fn start_hid(&mut self, si: usize, spd: u8, h: &u::HidBoot) -> Result<(), Error> {
        let dci = u::dci(h.ep_addr);
        let ring = Ring::new().ok_or(Error::NoMemory)?;
        let report = Buffer::new(1, 1).ok_or(Error::NoMemory)?;
        let id = self.slots[si].id;
        let port = self.slots[si].port;
        let entries = self.slots[si]
            .hid
            .iter()
            .map(|e| e.dci)
            .max()
            .unwrap_or(1)
            .max(dci);
        {
            let slot = &self.slots[si];
            // Configure Endpoint: add the slot context (entries updated) and this endpoint.
            for i in 0..u::input_ctx_dwords(self.csz) {
                slot.in_ctx.write32(i * 4, 0);
            }
            self.put(&slot.in_ctx, 0, &[0, 1 | 1 << dci as u32]);
            let mut sl = [0u32; 8];
            u::fill_slot(&mut sl, spd, entries, port);
            self.put(&slot.in_ctx, u::input_slot_offset(self.csz), &sl);
            let mut ep = [0u32; 8];
            u::fill_interrupt_in(
                &mut ep,
                h.max_packet,
                u::interrupt_interval(spd, h.interval),
                ring.pa(),
            );
            self.put(&slot.in_ctx, u::input_ep_offset(self.csz, dci), &ep);
            slot.in_ctx.sync(0, 4096);
        }
        let in_pa = self.slots[si].in_ctx.pa();
        self.command(u::configure_endpoint(in_pa, id, true))?;
        // Boot protocol (a keyboard may refuse SET_IDLE: that is allowed).
        self.control(si, u::req::hid_set_protocol(h.interface as u16, 0))?;
        let _ = self.control(si, u::req::hid_set_idle(h.interface as u16));
        let mut ep = HidEndpoint {
            dci,
            ring,
            report,
            req_len: (h.max_packet as usize).clamp(1, 8),
            dev: Hid::new(h.kind),
        };
        arm(&mut ep);
        self.slots[si].hid.push(ep);
        self.ring_slot(id, dci);
        Ok(())
    }

    /// An interrupt endpoint's transfer finished: deliver the report, queue the next transfer.
    fn hid_event(&mut self, slot_id: u8, dci: u8, code: u8, residual: u32) {
        let Some(slot) = self.slots.iter_mut().find(|s| s.id == slot_id) else {
            return;
        };
        let Some(ep) = slot.hid.iter_mut().find(|e| e.dci == dci) else {
            return;
        };
        match code {
            code::SUCCESS | code::SHORT_PACKET => {
                let got = ep.req_len.saturating_sub(residual as usize).min(8);
                ep.report.sync(0, 8);
                let mut r = [0u8; 8];
                ep.report.read_bytes(0, &mut r[..got]);
                ep.dev.report(&r[..got]);
                arm(ep);
                self.ring_slot(slot_id, dci);
            }
            c => {
                // A halted endpoint (stall or error): the device is probably going away; the port
                // scan removes it. Do not re-arm, do not spin.
                ep.dev.error(c);
            }
        }
    }

    /// Look for new and gone devices and handle whatever the controller has to say. Call every few ms.
    pub fn poll(&mut self) {
        while let Some(ev) = self.events.next() {
            self.event_done();
            let e = u::decode_event(&ev);
            self.stash(e);
        }
        if !self.started {
            self.started = true;
            // Give the ports a moment after the controller starts (a USB 3 hub-less root takes a while).
            crate::timer::delay_ms(100);
        }
        for port in 1..=self.max_ports {
            let a = self.portsc_addr(port);
            let v = rd32(a);
            let idx = port as usize - 1;
            let connected = v & portsc::CCS != 0;
            if v & portsc::CHANGE_BITS != 0 {
                wr32(a, portsc::write_clearing(v, v & portsc::CHANGE_BITS));
            }
            let now = crate::timer::now_ns() / 1_000_000 + 1;
            if !connected {
                self.connected_since[idx] = 0;
            } else if self.connected_since[idx] == 0 {
                self.connected_since[idx] = now;
            }
            if connected && !self.attached[idx] && now - self.connected_since[idx] >= 100 {
                self.attached[idx] = true;
                if let Err(e) = self.enumerate(port) {
                    kprintln!("[usb] port {port}: {e:?}");
                }
            } else if !connected && self.attached[idx] {
                self.attached[idx] = false;
                self.remove(port);
            }
        }
        for s in &mut self.slots {
            for e in &mut s.hid {
                e.dev.tick();
            }
        }
    }

    /// A device was unplugged: release its keys, free its slot.
    fn remove(&mut self, port: u8) {
        let Some(i) = self.slots.iter().position(|s| s.port == port) else {
            return;
        };
        let id = self.slots[i].id;
        for e in &mut self.slots[i].hid {
            e.dev.unplugged();
        }
        let _ = self.command(u::disable_slot(id, true));
        self.dcbaa.write64(id as usize * 8, 0);
        self.dcbaa.sync(0, 4096);
        self.slots.remove(i);
        kprintln!("[usb] port {port}: device removed");
    }
}

/// Queue one report transfer on an interrupt endpoint.
fn arm(ep: &mut HidEndpoint) {
    ep.report.sync(0, 8);
    ep.ring
        .push(u::normal(ep.report.pa(), ep.req_len as u32, true));
}

fn speed_name(s: u8) -> &'static str {
    match s {
        speed::LOW => "low",
        speed::FULL => "full",
        speed::HIGH => "high",
        speed::SUPER => "super",
        _ => "unknown",
    }
}
