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

use core::ptr::{read_volatile, write_volatile};

use rpikit::mailbox::{self as msg, CHANNEL_PROPERTY};

use super::{Timeout, flush_cache};
use crate::mmu::{PAGE_SIZE, phys_to_virt, virt_to_phys};
use crate::platform::periph;
use crate::sync::SpinLock;

const BASE: usize = phys_to_virt(periph(0xb880));
const READ: usize = 0x00;
const STATUS_READ: usize = 0x18;
const WRITE: usize = 0x20;
const STATUS_WRITE: usize = 0x38;
const EMPTY: u32 = 1 << 30;
const FULL: u32 = 1 << 31;

/// The VideoCore sees ARM RAM through bus aliases; this one is the cached alias Linux uses on the
/// BCM2711 (physical + 0xc000_0000). qemu's models accept any alias.
const BUS_ALIAS: u64 = 0xc000_0000;

/// How long a transaction may take before we give up on the firmware.
const TIMEOUT_MS: u64 = 1000;

/// Why a transaction failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No memory for the message buffer, or a message that does not fit it.
    NoMemory,
    /// The mailbox did not accept or answer the message in time.
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

fn reg_read(off: usize) -> u32 {
    // SAFETY: mailbox registers, mapped as Device memory.
    unsafe { read_volatile((BASE + off) as *const u32) }
}

fn reg_write(off: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile((BASE + off) as *mut u32, v) }
}

/// Send the message at `pa` (16-byte aligned) and wait for the firmware to hand it back.
fn exchange(pa: usize) -> Result<(), Error> {
    let bus = (pa as u64 | BUS_ALIAS) as u32;
    let word = msg::mail_word(bus, CHANNEL_PROPERTY).ok_or(Error::NoMemory)?;
    let t = Timeout::ms(TIMEOUT_MS);
    while reg_read(STATUS_WRITE) & FULL != 0 {
        if t.expired() {
            return Err(Error::Timeout);
        }
    }
    // SAFETY: barrier: the message is in memory before the doorbell rings.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
    reg_write(WRITE, word);
    loop {
        while reg_read(STATUS_READ) & EMPTY != 0 {
            if t.expired() {
                return Err(Error::Timeout);
            }
        }
        let (addr, ch) = msg::split_mail(reg_read(READ));
        // Something else's answer (another channel) is not ours: keep waiting.
        if ch == CHANNEL_PROPERTY && addr == bus & !0xf {
            return Ok(());
        }
        if t.expired() {
            return Err(Error::Timeout);
        }
    }
}

/// One property transaction. `fill` writes the message into the buffer (a page of words) and
/// returns whatever handle `read` needs; `read` then interprets the firmware's answer. The
/// whole call holds the mailbox.
pub fn transact<H, R>(
    fill: impl FnOnce(&mut [u32]) -> Option<H>,
    read: impl FnOnce(&[u32], H) -> Result<R, msg::Error>,
) -> Result<R, Error> {
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
    let pa = *page;
    let va = phys_to_virt(pa);
    // SAFETY: a page we own, 4096 bytes, u32 aligned; the lock keeps it ours.
    let buf = unsafe { core::slice::from_raw_parts_mut(va as *mut u32, PAGE_SIZE / 4) };
    let handle = fill(buf).ok_or(Error::NoMemory)?;
    debug_assert_eq!(virt_to_phys(va), pa);
    flush_cache(va, PAGE_SIZE);
    exchange(pa)?;
    flush_cache(va, PAGE_SIZE);
    // SAFETY: as above; the firmware is done with the buffer.
    let buf = unsafe { core::slice::from_raw_parts(va as *const u32, PAGE_SIZE / 4) };
    read(buf, handle).map_err(Error::Reply)
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
