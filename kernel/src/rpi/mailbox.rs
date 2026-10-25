//! The VideoCore property mailbox: how the ARM asks the firmware for the framebuffer, clock
//! rates, power domains and board data (message formats: rpikit::mailbox).
//!
//! Two registers blocks (BCM2835 ARM Peripherals, "Mailbox"): mailbox 0 is what the ARM reads
//! (data at +0x00, status at +0x18; bit 30 = empty), mailbox 1 what it writes (data at +0x20,
//! status at +0x38; bit 31 = full). A word is a 16-byte-aligned bus address of the message
//! ORed with the channel number (8 = property tags). The firmware writes its answer into the
//! same buffer and echoes the word back. One transaction at a time, under a lock.
//!
//! The buffer is a page of ordinary RAM. The VideoCore reads it by bus address, not through
//! the ARM's caches, so the CPU cleans the lines before the call and drops them after.
//!
//! **Finding out whether the transport works** ([`probe`], once, from the first user): the first
//! message is a harmless one (the firmware revision), sent through the preferred bus alias; if nothing
//! answers, the other aliases of the same RAM are tried (rpikit::mailbox::BUS_ALIASES) and everything
//! seen is logged: the page, the word sent, both status registers before and after, anything
//! already waiting in mailbox 0, the buffer's words after a failure. The reply is accepted if it is on
//! the property channel and names our buffer (under any alias), and the buffer's response code says
//! the firmware answered; so a firmware that hands the address back without the alias bits still works.
//! If nothing answers, the mailbox is marked down and later calls fail at once (no second of waiting for
//! each clock query). The answers to the harmless tags (board model and revision, memory split, clocks)
//! are logged, so a boot log shows whether the transport ever worked apart from the framebuffer.
//! (This is untested on real hardware as of this writing: docs/raspberry-pi.md.)

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use rpikit::mailbox::{self as msg, CHANNEL_PROPERTY, Echo};

use super::{Timeout, flush_cache};
use crate::kprintln;
use crate::mmu::{PAGE_SIZE, phys_to_virt, virt_to_phys};
use crate::platform::periph;
use crate::sync::SpinLock;

const BASE: usize = phys_to_virt(periph(0xb880));
const READ: usize = 0x00;
const STATUS_READ: usize = 0x18;
const CONFIG_READ: usize = 0x1c;
const WRITE: usize = 0x20;
const STATUS_WRITE: usize = 0x38;
const CONFIG_WRITE: usize = 0x3c;
const EMPTY: u32 = 1 << 30;
const FULL: u32 = 1 << 31;

/// How long the first transaction may take before we give up on an alias, and the later ones.
const FIRST_TIMEOUT_MS: u64 = 2000;
const ALIAS_TIMEOUT_MS: u64 = 500;
/// How long an ordinary transaction may take before we give up on the firmware.
const TIMEOUT_MS: u64 = 1000;

/// Why a transaction failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No memory for the message buffer, or a message that does not fit it.
    NoMemory,
    /// The mailbox did not accept or answer the message in time (or is known to be down).
    Timeout,
    /// The firmware answered, but not usefully.
    Reply(msg::Error),
}

impl From<msg::Error> for Error {
    fn from(e: msg::Error) -> Error {
        Error::Reply(e)
    }
}

/// Physical address of the message page (0 until the first call).
static PAGE: SpinLock<usize> = SpinLock::new(0);

/// 0: not probed, 1: probing, 2: up, 3: down.
static STATE: AtomicU8 = AtomicU8::new(0);
/// The bus alias that works (an index into BUS_ALIASES).
static ALIAS: AtomicU8 = AtomicU8::new(0);
/// Ordinary failures logged so far (a failing mailbox must not fill the log).
static FAILURES_LOGGED: AtomicU32 = AtomicU32::new(0);

fn reg_read(off: usize) -> u32 {
    // SAFETY: mailbox registers, mapped as Device memory.
    crate::serror::note_read(BASE + off);
    unsafe { read_volatile((BASE + off) as *const u32) }
}

fn reg_write(off: usize, v: u32) {
    // SAFETY: as above.
    let _c = crate::serror::write_guard(BASE + off, v);
    unsafe { write_volatile((BASE + off) as *mut u32, v) }
}

/// What went wrong with one exchange, for the log.
#[derive(Debug, Clone, Copy)]
struct Trace {
    /// The word written (0 if it never was).
    sent: u32,
    /// The write mailbox stayed full.
    full: bool,
    /// Unexpected words read from mailbox 0 while waiting (the last one).
    other: Option<u32>,
    others: u32,
    /// Status registers when it ended.
    status_read: u32,
    status_write: u32,
    /// Microseconds from ringing the doorbell to the answer (or to giving up).
    us: u64,
}

/// Microseconds on the generic counter (works before the timer driver and with interrupts off).
pub(super) fn now_us() -> u64 {
    let (c, f): (u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) c, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    c / (f.max(1_000_000) / 1_000_000)
}

/// Send the message at `pa` (16-byte aligned) through bus alias `alias` and wait for the firmware to hand
/// it back.
fn exchange(pa: usize, alias: u32, timeout_ms: u64) -> Result<Trace, Trace> {
    let bus = (pa as u64 & 0x3fff_ffff) as u32 | alias;
    let mut tr = Trace {
        sent: 0,
        full: false,
        other: None,
        others: 0,
        status_read: 0,
        status_write: 0,
        us: 0,
    };
    let Some(word) = msg::mail_word(bus, CHANNEL_PROPERTY) else {
        return Err(tr);
    };
    let t = Timeout::ms(timeout_ms);
    while reg_read(STATUS_WRITE) & FULL != 0 {
        if t.expired() {
            tr.full = true;
            tr.status_read = reg_read(STATUS_READ);
            tr.status_write = reg_read(STATUS_WRITE);
            return Err(tr);
        }
    }
    // SAFETY: barrier: the message is in memory before the doorbell rings.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
    let t0 = now_us();
    reg_write(WRITE, word);
    tr.sent = word;
    let t = Timeout::ms(timeout_ms);
    loop {
        while reg_read(STATUS_READ) & EMPTY != 0 {
            if t.expired() {
                tr.status_read = reg_read(STATUS_READ);
                tr.status_write = reg_read(STATUS_WRITE);
                tr.us = now_us().wrapping_sub(t0);
                return Err(tr);
            }
        }
        let got = reg_read(READ);
        // Our channel and our buffer, under whatever alias the firmware names it: the answer. Anything
        // else (another channel, another buffer) is not ours: keep waiting, and remember it.
        match msg::echo(got, word) {
            Echo::Exact | Echo::SameBuffer => {
                tr.status_read = reg_read(STATUS_READ);
                tr.status_write = reg_read(STATUS_WRITE);
                tr.us = now_us().wrapping_sub(t0);
                return Ok(tr);
            }
            Echo::OtherBuffer | Echo::OtherChannel => {
                tr.other = Some(got);
                tr.others += 1;
            }
        }
        if t.expired() {
            tr.status_read = reg_read(STATUS_READ);
            tr.status_write = reg_read(STATUS_WRITE);
            tr.us = now_us().wrapping_sub(t0);
            return Err(tr);
        }
    }
}

/// Words waiting in mailbox 0 before we send anything (at most 16 are taken out, and logged).
fn drain() -> u32 {
    let mut n = 0;
    while reg_read(STATUS_READ) & EMPTY == 0 && n < 16 {
        let w = reg_read(READ);
        kprintln!("mbox: drained a stale word {w:#010x} from mailbox 0");
        n += 1;
    }
    n
}

/// The buffer's first words, for the log after a failure.
fn log_buffer(buf: &[u32], why: &str) {
    let n = ((buf.first().copied().unwrap_or(0) / 4) as usize).clamp(4, 32).min(buf.len());
    kprintln!("mbox: {why}: buffer ({n} words)");
    for (i, row) in buf[..n].chunks(8).enumerate() {
        match row.len() {
            8 => kprintln!(
                "mbox:   [{:2}] {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x}",
                i * 8, row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7]
            ),
            _ => kprintln!("mbox:   [{:2}] {:08x?}", i * 8, row),
        }
    }
}

fn log_trace(why: &str, alias: u32, tr: &Trace) {
    kprintln!(
        "mbox: {why}: alias {alias:#010x} sent {:#010x}{} after {} us; status read {:#010x} write {:#010x} (empty bit {}, full bit {}); {} other word(s){}",
        tr.sent,
        if tr.full { " (write mailbox stayed full)" } else { "" },
        tr.us,
        tr.status_read,
        tr.status_write,
        tr.status_read >> 30 & 1,
        tr.status_write >> 31,
        tr.others,
        match tr.other {
            Some(w) => alloc::format!(", last {w:#010x}"),
            None => alloc::string::String::new(),
        }
    );
}

/// The message page, allocated on first use.
fn page() -> Result<(usize, usize), Error> {
    let mut page = PAGE.lock();
    if *page == 0 {
        // The VideoCore reads the message through a 1 GiB bus alias: it must sit in the first GiB
        // (on a 4 or 8 GB board most of RAM is above that).
        *page = crate::pmm::alloc_contiguous_zeroed_between(
            crate::pmm::Use::Devices,
            1,
            1,
            0,
            0x4000_0000,
        )
        .ok_or(Error::NoMemory)?;
    }
    Ok((*page, phys_to_virt(*page)))
}

/// One property transaction. `fill` writes the message into the buffer (a page of words) and
/// returns whatever handle `read` needs; `read` then interprets the firmware's answer. The
/// whole call holds the mailbox.
pub fn transact<H, R>(
    fill: impl FnOnce(&mut [u32]) -> Option<H>,
    read: impl FnOnce(&[u32], H) -> Result<R, msg::Error>,
) -> Result<R, Error> {
    if STATE.load(Ordering::Acquire) == 0 {
        probe();
    }
    if STATE.load(Ordering::Acquire) != 2 {
        return Err(Error::Timeout); // down (or being probed): do not wait for a firmware that is not there
    }
    transact_via(ALIAS.load(Ordering::Relaxed), TIMEOUT_MS, fill, read).map_err(|(e, _)| e)
}

/// [`transact`] through a chosen alias, with the exchange's trace on failure.
fn transact_via<H, R>(
    alias_idx: u8,
    timeout_ms: u64,
    fill: impl FnOnce(&mut [u32]) -> Option<H>,
    read: impl FnOnce(&[u32], H) -> Result<R, msg::Error>,
) -> Result<R, (Error, Option<Trace>)> {
    let (pa, va) = page().map_err(|e| (e, None))?;
    let guard = PAGE.lock(); // the call holds the page
    // SAFETY: a page we own, 4096 bytes, u32 aligned; the lock keeps it ours.
    let buf = unsafe { core::slice::from_raw_parts_mut(va as *mut u32, PAGE_SIZE / 4) };
    let handle = fill(buf).ok_or((Error::NoMemory, None))?;
    debug_assert_eq!(virt_to_phys(va), pa);
    flush_cache(va, PAGE_SIZE);
    let alias = msg::BUS_ALIASES[alias_idx as usize % msg::BUS_ALIASES.len()];
    let res = exchange(pa, alias, timeout_ms);
    flush_cache(va, PAGE_SIZE);
    // The firmware wrote into our memory by DMA and the doorbell was a posted write: take a pending SError here.
    super::serror_checkpoint("mbox-call");
    // SAFETY: as above; the firmware is done with the buffer (or gave up on it).
    let buf = unsafe { core::slice::from_raw_parts(va as *const u32, PAGE_SIZE / 4) };
    let out = match res {
        Err(tr) => {
            if FAILURES_LOGGED.fetch_add(1, Ordering::Relaxed) < 6 {
                log_trace("no answer", alias, &tr);
                log_buffer(buf, "after no answer");
            }
            Err((Error::Timeout, Some(tr)))
        }
        Ok(tr) => read(buf, handle).map_err(|e| {
            if FAILURES_LOGGED.fetch_add(1, Ordering::Relaxed) < 6 {
                log_trace("answered, but not usefully", alias, &tr);
                log_buffer(buf, &alloc::format!("reply error {e:?}"));
            }
            (Error::Reply(e), Some(tr))
        }),
    };
    drop(guard);
    out
}

/// Find out whether the mailbox transport works, once (later calls return the first answer), and log
/// what was seen. True if the firmware answered a message.
pub fn probe() -> bool {
    if STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return STATE.load(Ordering::Acquire) == 2;
    }
    let (f, c): (u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) c, options(nomem, nostack));
    }
    let pa = match page() {
        Ok((pa, _)) => pa,
        Err(e) => {
            kprintln!("mbox: no message page ({e:?}): {}", pmm_summary());
            STATE.store(3, Ordering::Release);
            return false;
        }
    };
    kprintln!(
        "mbox: registers at {:#x}; message page {pa:#x}; status read {:#010x} write {:#010x} config read {:#x} write {:#x}; counter {f} Hz at {c}",
        periph(0xb880),
        reg_read(STATUS_READ),
        reg_read(STATUS_WRITE),
        reg_read(CONFIG_READ),
        reg_read(CONFIG_WRITE)
    );
    drain();
    let mut winner = None;
    for idx in 0..msg::BUS_ALIASES.len() as u8 {
        let alias = msg::BUS_ALIASES[idx as usize];
        let t = if idx == 0 { FIRST_TIMEOUT_MS } else { ALIAS_TIMEOUT_MS };
        let r = transact_via(
            idx,
            t,
            |buf| {
                let mut b = msg::Builder::new(buf);
                let tag = b.tag(msg::tag::GET_FIRMWARE_REVISION, &[], 1)?;
                b.finish()?;
                Some(tag)
            },
            |buf, tag| msg::Reply::new(buf).word(tag, 0),
        );
        match r {
            Ok(rev) => {
                kprintln!("mbox: the firmware answers through bus alias {alias:#010x}: firmware revision {rev:#x}");
                winner = Some(idx);
                break;
            }
            Err((Error::Reply(e), tr)) => {
                // The mailbox carried the exchange (the reply named our buffer): the transport works; the message was not accepted.
                kprintln!("mbox: the firmware replied through bus alias {alias:#010x} but the message failed: {e:?}");
                if let Some(tr) = tr {
                    log_trace("reply", alias, &tr);
                }
                winner = Some(idx);
                break;
            }
            Err((e, tr)) => {
                kprintln!("mbox: no answer through bus alias {alias:#010x} within {t} ms ({e:?})");
                if let Some(tr) = tr {
                    log_trace("attempt", alias, &tr);
                }
                drain();
            }
        }
    }
    let Some(idx) = winner else {
        kprintln!("mbox: the firmware did not answer on any bus alias: the mailbox is marked down (clock, board and framebuffer queries will fail at once)");
        STATE.store(3, Ordering::Release);
        return false;
    };
    ALIAS.store(idx, Ordering::Relaxed);
    STATE.store(2, Ordering::Release);
    harmless_tags();
    true
}

fn pmm_summary() -> alloc::string::String {
    let s = crate::pmm::stats();
    alloc::format!(
        "pmm {} frames, {} free, {} reserved; wanted one page below 1 GiB",
        s.total_frames, s.free_frames, s.reserved_frames
    )
}

/// Log what the firmware says about the board with queries that cannot depend on a display.
fn harmless_tags() {
    use msg::tag;
    let ask = |name: &str, t: u32, req: &[u32], words: usize| match query(t, req, words) {
        Ok(v) => Some(v),
        Err(e) => {
            kprintln!("mbox: {name}: {e:?}");
            None
        }
    };
    if let Some(v) = ask("board model", tag::GET_BOARD_MODEL, &[], 1) {
        kprintln!("mbox: board model {:#x}", v[0]);
    }
    if let Some(v) = ask("board revision", tag::GET_BOARD_REVISION, &[], 1) {
        kprintln!("mbox: board revision {:#x}", v[0]);
    }
    if let Some(v) = ask("serial", tag::GET_BOARD_SERIAL, &[], 2) {
        kprintln!("mbox: board serial {:08x}{:08x}", v[1], v[0]);
    }
    if let Some(v) = ask("ARM memory", tag::GET_ARM_MEMORY, &[], 2) {
        kprintln!("mbox: ARM memory base {:#x} size {:#x} ({} MiB)", v[0], v[1], v[1] >> 20);
    }
    if let Some(v) = ask("VC memory", tag::GET_VC_MEMORY, &[], 2) {
        kprintln!("mbox: VideoCore memory base {:#x} size {:#x} ({} MiB)", v[0], v[1], v[1] >> 20);
    }
    if let Some(v) = ask("throttled", tag::GET_THROTTLED, &[0], 1) {
        kprintln!("mbox: throttled/under-voltage flags {:#x}", v[0]);
    }
    if let Some(v) = ask("temperature", tag::GET_TEMPERATURE, &[0], 2) {
        kprintln!("mbox: SoC temperature {} m C", v[1]);
    }
    for (id, name) in [
        (msg::clock::ARM, "arm"),
        (msg::clock::CORE, "core"),
        (msg::clock::EMMC, "emmc"),
        (msg::clock::EMMC2, "emmc2"),
        (msg::clock::UART, "uart"),
    ] {
        if let Some(v) = ask(name, tag::GET_CLOCK_RATE, &[id], 2) {
            kprintln!("mbox: clock {name} ({id}) {} Hz", v[1]);
        }
    }
    if let Some(v) = ask("physical display size", tag::GET_PHYSICAL_SIZE, &[], 2) {
        kprintln!("mbox: display size the firmware has: {}x{}", v[0], v[1]);
    }
}

/// A single-tag query: `tag` with `request` words, answer of up to `answer` words.
pub fn query(tag: u32, request: &[u32], answer: usize) -> Result<[u32; 4], Error> {
    transact(
        |buf| {
            let mut b = msg::Builder::new(buf);
            let t = b.tag(tag, request, answer)?;
            b.finish()?;
            Some(t)
        },
        |buf, t| {
            let v = msg::Reply::new(buf).values(t)?;
            let mut out = [0u32; 4];
            for (o, w) in out.iter_mut().zip(v) {
                *o = *w;
            }
            Ok(out)
        },
    )
}

/// Rate of clock `id` in Hz (0 if the firmware does not know it).
pub fn clock_rate(id: u32) -> Result<u32, Error> {
    Ok(query(msg::tag::GET_CLOCK_RATE, &[id], 2)?[1])
}

/// The highest rate clock `id` will be set to, in Hz (0 if the firmware does not know it). The
/// core clock follows the load between its lowest and highest rate, so a divider computed from
/// the highest never makes a bus faster than asked.
pub fn max_clock_rate(id: u32) -> Result<u32, Error> {
    Ok(query(msg::tag::GET_MAX_CLOCK_RATE, &[id], 2)?[1])
}

/// Board revision code (the Pi's `Revision` line in /proc/cpuinfo).
pub fn board_revision() -> Result<u32, Error> {
    Ok(query(msg::tag::GET_BOARD_REVISION, &[], 1)?[0])
}

/// The board's Ethernet MAC address.
pub fn board_mac() -> Result<[u8; 6], Error> {
    let w = query(msg::tag::GET_BOARD_MAC, &[], 2)?;
    let b0 = w[0].to_le_bytes();
    let b1 = w[1].to_le_bytes();
    Ok([b0[0], b0[1], b0[2], b0[3], b1[0], b1[1]])
}

/// Firmware revision (a build timestamp).
pub fn firmware_revision() -> Result<u32, Error> {
    Ok(query(msg::tag::GET_FIRMWARE_REVISION, &[], 1)?[0])
}

/// Switch a power domain (rpikit::mailbox::power) on or off and wait for it. True if it is in
/// the state asked for afterwards.
#[allow(dead_code)] // for the USB and PCIe bring-up (docs/raspberry-pi.md)
pub fn set_power(device: u32, on: bool) -> Result<bool, Error> {
    let w = query(msg::tag::SET_POWER_STATE, &[device, on as u32 | 2], 2)?;
    // Bit 0 of the answer: on; bit 1: the device does not exist.
    Ok(w[1] & 2 == 0 && (w[1] & 1 != 0) == on)
}
