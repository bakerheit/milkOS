//! SD card through an SDHCI host controller, as a block device (`/dev/mmcblk0`).
//!
//! The Pi 4's SD slot is on EMMC2 (0xfe34_0000), a standard SD Host Controller (spec 3.0) that
//! only accepts 32-bit register accesses: the 8 and 16 bit registers are reached through
//! aligned 32-bit reads and read-modify-writes, and the registers whose bits are cleared by writing
//! them (the interrupt status) get one whole-word write. qemu's raspi4b has the same controller
//! type at EMMC2 and at the older EMMC address; [`init`] looks at both and uses the one that has a card.
//!
//! Programmed I/O, polled, with a bound on every wait: the card's data goes through the
//! buffer data port a word at a time. That is slow next to DMA (a few MB/s at 25 MHz, 4 bit) but
//! has nothing to get wrong on real silicon (no bus-address windows, no cache coherency), and
//! the first goal is a root disk that works. SDMA/ADMA2 is the next step (docs/raspberry-pi.md).
//!
//! Card bring-up is the physical layer spec's: CMD0, CMD8, ACMD41 until powered up, CMD2, CMD3,
//! CMD9 (the capacity), CMD7, ACMD6 (4 bit), 25 MHz. Command encodings and answers' meanings are
//! `rpikit::sd`; this file is registers and waiting.

#![allow(clippy::chunks_exact_to_as_chunks)] // sector-sized chunks of a runtime-length slice

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use rpikit::sd::{self, Command, Dir, Resp, cmd};

use super::{Timeout, mailbox};
use crate::fs::blockdev::{self, BlockDevice};
use crate::fs::errno::{Errno, Result};
use crate::kprintln;
use crate::mmu::phys_to_virt;
use crate::platform::periph;
use crate::sched::Mutex;

/// EMMC2, the Pi 4's SD slot, and the older EMMC controller (the Wi-Fi chip's on a board).
const EMMC2: usize = periph(0x34_0000);
const EMMC1: usize = periph(0x30_0000);

// SDHCI registers (the 32-bit words they live in; sub-fields noted).
const BLOCK: usize = 0x04; // block size [15:0], block count [31:16]
const ARG: usize = 0x08;
const CMDMODE: usize = 0x0c; // transfer mode [15:0], command [31:16]
const RESP: usize = 0x10; // four words
const DATA: usize = 0x20;
const STATE: usize = 0x24;
const HOSTCTL: usize = 0x28; // host control [7:0], power [15:8]
const CLOCK: usize = 0x2c; // clock control [15:0], timeout [23:16], software reset [31:24]
const INT: usize = 0x30; // normal [15:0], error [31:16] (write 1 to clear)
const INT_ENABLE: usize = 0x34;
const INT_SIGNAL: usize = 0x38;
const CAPS: usize = 0x40;
const VERSION: usize = 0xfc; // [31:16]

// STATE bits.
const ST_CMD_INHIBIT: u32 = 1 << 0;
const ST_DAT_INHIBIT: u32 = 1 << 1;
const ST_CARD_PRESENT: u32 = 1 << 16;

// INT bits.
const INT_CMD_DONE: u32 = 1 << 0;
const INT_XFER_DONE: u32 = 1 << 1;
const INT_WRITE_READY: u32 = 1 << 4;
const INT_READ_READY: u32 = 1 << 5;
const INT_ERROR: u32 = 1 << 15;

// CLOCK bits.
const CLK_INTERNAL_STABLE: u32 = 1 << 1;
const CLK_SD_ENABLE: u32 = 1 << 2;
const RESET_ALL: u32 = 1 << 24;
const RESET_CMD: u32 = 1 << 25;
const RESET_DAT: u32 = 1 << 26;

// HOSTCTL bits.
const HC_4BIT: u32 = 1 << 1;
const PWR_ON_3V3: u32 = 0x0f << 8; // 3.3 V, bus power on

/// Card identification clock, and the transfer clock (default speed; high speed needs CMD6).
const ID_HZ: u32 = 400_000;
const XFER_HZ: u32 = 25_000_000;

/// Largest transfer one command carries.
const MAX_BLOCKS_PER_CMD: usize = 128;
const SECTOR: usize = 512;

/// Why a command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// The controller did not finish (or accept) in time.
    Timeout,
    /// The controller or the card reported an error (CRC, index, end bit, timeout on the line).
    Error(u32),
    /// The card's status after the command says it failed.
    Status(u32),
}

struct Host {
    base: usize,
}

impl Host {
    fn r32(&self, off: usize) -> u32 {
        // SAFETY: SDHCI registers, mapped as Device memory (platform.rs).
        unsafe { read_volatile((phys_to_virt(self.base) + off) as *const u32) }
    }

    fn w32(&self, off: usize, v: u32) {
        // SAFETY: as above.
        unsafe { write_volatile((phys_to_virt(self.base) + off) as *mut u32, v) }
    }

    /// Wait until `done(register)` is true, at most `ms` milliseconds. The register value that
    /// satisfied it, or `None` on a timeout.
    fn wait(&self, off: usize, ms: u64, done: impl Fn(u32) -> bool) -> Option<u32> {
        let t = Timeout::ms(ms);
        loop {
            let v = self.r32(off);
            if done(v) {
                return Some(v);
            }
            if t.expired() {
                return None;
            }
        }
    }

    /// Software reset of `bits` (RESET_ALL / RESET_CMD / RESET_DAT), waiting for it to finish.
    fn reset(&self, bits: u32) -> bool {
        let w = self.r32(CLOCK);
        self.w32(CLOCK, w | bits);
        self.wait(CLOCK, 100, |v| v & bits == 0).is_some()
    }

    fn clear_interrupts(&self) {
        self.w32(INT, 0xffff_ffff);
    }

    /// Program the card clock: the divider for `target` Hz from `base` Hz, SD clock on.
    /// Returns the frequency it got (0 if the clock would not start).
    fn set_clock(&self, base: u32, target: u32) -> u32 {
        let Some((n, hz)) = sd::clock_divider(base, target) else {
            return 0;
        };
        // Stop the SD clock, set the divider with the internal clock on, wait for it to settle.
        let w = self.r32(CLOCK) & 0xffff_0000;
        self.w32(CLOCK, w);
        self.w32(CLOCK, w | sd::clock_control(n) as u32);
        if self
            .wait(CLOCK, 150, |v| v & CLK_INTERNAL_STABLE != 0)
            .is_none()
        {
            return 0;
        }
        let v = self.r32(CLOCK);
        self.w32(CLOCK, v | CLK_SD_ENABLE);
        hz
    }

    /// Send `c` and wait for its answer. The response registers are read by the caller.
    fn command(&self, c: &Command, blocks: u32) -> core::result::Result<(), Fail> {
        let busy_cmd = c.data.is_some() || c.resp == Resp::R48Busy;
        let inhibit = ST_CMD_INHIBIT | if busy_cmd { ST_DAT_INHIBIT } else { 0 };
        if self.wait(STATE, 100, |v| v & inhibit == 0).is_none() {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            return Err(Fail::Timeout);
        }
        self.clear_interrupts();
        // Longest data timeout (2^27 clocks), the rest of the CLOCK word kept.
        let clock = self.r32(CLOCK);
        self.w32(CLOCK, clock & !0x00ff_0000 | 0x000e_0000);
        if c.data.is_some() {
            self.w32(BLOCK, blocks << 16 | SECTOR as u32);
        }
        self.w32(ARG, c.arg);
        // SAFETY: the argument and block registers are written before the command starts it.
        unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
        self.w32(CMDMODE, sd::sdhci_cmd_word(c, false));
        // Command complete (or an error): cards answer within microseconds, 100 ms is generous.
        let Some(status) = self.wait(INT, 100, |v| v & (INT_CMD_DONE | INT_ERROR) != 0) else {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            return Err(Fail::Timeout);
        };
        if status & INT_ERROR != 0 {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            self.clear_interrupts();
            return Err(Fail::Error(status >> 16));
        }
        self.w32(INT, INT_CMD_DONE);
        // R1b: the card holds DAT0 low until it is done; the controller reports it as transfer
        // complete (no data phase) or when the data is through.
        if c.data.is_none() && c.resp == Resp::R48Busy {
            let Some(s) = self.wait(INT, 500, |v| v & (INT_XFER_DONE | INT_ERROR) != 0) else {
                let _ = self.reset(RESET_CMD | RESET_DAT);
                return Err(Fail::Timeout);
            };
            self.w32(INT, INT_XFER_DONE);
            if s & INT_ERROR != 0 {
                let _ = self.reset(RESET_CMD | RESET_DAT);
                self.clear_interrupts();
                return Err(Fail::Error(s >> 16));
            }
        }
        Ok(())
    }

    fn response(&self) -> [u32; 4] {
        [
            self.r32(RESP),
            self.r32(RESP + 4),
            self.r32(RESP + 8),
            self.r32(RESP + 12),
        ]
    }

    /// Wait for the buffer to be ready (`ready` = INT_READ_READY / INT_WRITE_READY) and move one
    /// 512-byte block through the data port.
    fn block_io(
        &self,
        ready: u32,
        data: Option<&mut [u8]>,
        out: Option<&[u8]>,
    ) -> core::result::Result<(), Fail> {
        let Some(status) = self.wait(INT, 1000, |v| v & (ready | INT_ERROR) != 0) else {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            return Err(Fail::Timeout);
        };
        if status & INT_ERROR != 0 {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            self.clear_interrupts();
            return Err(Fail::Error(status >> 16));
        }
        self.w32(INT, ready);
        if let Some(buf) = data {
            for chunk in buf.chunks_exact_mut(4) {
                chunk.copy_from_slice(&self.r32(DATA).to_le_bytes());
            }
        } else if let Some(buf) = out {
            for chunk in buf.chunks_exact(4) {
                self.w32(
                    DATA,
                    u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                );
            }
        }
        Ok(())
    }

    /// After the last block: wait for transfer complete.
    fn finish_transfer(&self) -> core::result::Result<(), Fail> {
        let Some(status) = self.wait(INT, 1000, |v| v & (INT_XFER_DONE | INT_ERROR) != 0) else {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            return Err(Fail::Timeout);
        };
        if status & INT_ERROR != 0 {
            let _ = self.reset(RESET_CMD | RESET_DAT);
            self.clear_interrupts();
            return Err(Fail::Error(status >> 16));
        }
        self.w32(INT, INT_XFER_DONE);
        Ok(())
    }
}

/// What the card looked like when we initialised it.
struct Card {
    high_capacity: bool,
    blocks: u64,
}

/// The caller's buffer for one transfer.
enum Buf<'a> {
    /// Fill it from the card.
    Read(&'a mut [u8]),
    /// Send it to the card.
    Write(&'a [u8]),
}

struct Inner {
    host: Host,
    card: Card,
}

impl Inner {
    /// An R1 answer's card status, as an error if it says the command failed.
    fn check_r1(&self) -> core::result::Result<u32, Fail> {
        let s = self.host.r32(RESP);
        if s & sd::R1_ERROR_MASK != 0 {
            Err(Fail::Status(s))
        } else {
            Ok(s)
        }
    }

    fn rw(&self, lba: u64, mut buf: Buf) -> core::result::Result<(), Fail> {
        let (len, write) = match &buf {
            Buf::Read(b) => (b.len(), false),
            Buf::Write(b) => (b.len(), true),
        };
        let blocks = len / SECTOR;
        let arg = sd::data_arg(lba, self.card.high_capacity).ok_or(Fail::Status(0))?;
        let (index, dir) = match (write, blocks > 1) {
            (false, false) => (cmd::READ_SINGLE_BLOCK, Dir::Read),
            (false, true) => (cmd::READ_MULTIPLE_BLOCK, Dir::Read),
            (true, false) => (cmd::WRITE_BLOCK, Dir::Write),
            (true, true) => (cmd::WRITE_MULTIPLE_BLOCK, Dir::Write),
        };
        let mut c = Command::new(index, arg, Resp::R48);
        c.data = Some(dir);
        c.multi = blocks > 1;
        c.auto_stop = blocks > 1;
        self.host.command(&c, blocks as u32)?;
        self.check_r1()?;
        match &mut buf {
            Buf::Read(b) => {
                for chunk in b.chunks_exact_mut(SECTOR) {
                    self.host.block_io(INT_READ_READY, Some(chunk), None)?;
                }
            }
            Buf::Write(b) => {
                for chunk in b.chunks_exact(SECTOR) {
                    self.host.block_io(INT_WRITE_READY, None, Some(chunk))?;
                }
            }
        }
        self.host.finish_transfer()?;
        Ok(())
    }
}

/// `/dev/mmcblk0`.
struct SdCard {
    name: String,
    blocks: u64,
    /// Writes are refused (the default: a card that boots another system must not be written by accident;
    /// `milk.sdwrite=1` turns writing on).
    ro: bool,
    inner: Mutex<Inner>,
    io: [AtomicU64; 4],
}

fn errno_for(f: Fail) -> Errno {
    kprintln!("[sdhci] I/O failed: {f:?}");
    Errno::EIO
}

impl SdCard {
    fn check(&self, lba: u64, len: usize) -> Result<()> {
        if !len.is_multiple_of(SECTOR) {
            return Err(Errno::EINVAL);
        }
        let end = lba
            .checked_add((len / SECTOR) as u64)
            .ok_or(Errno::EINVAL)?;
        if end > self.blocks {
            return Err(Errno::ENXIO);
        }
        Ok(())
    }
}

impl BlockDevice for SdCard {
    fn name(&self) -> &str {
        &self.name
    }
    fn block_size(&self) -> usize {
        SECTOR
    }
    fn num_blocks(&self) -> u64 {
        self.blocks
    }
    fn read_only(&self) -> bool {
        self.ro
    }

    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        self.check(lba, buf.len())?;
        self.io[0].fetch_add(1, Ordering::Relaxed);
        self.io[1].fetch_add((buf.len() / SECTOR) as u64, Ordering::Relaxed);
        let d = self.inner.lock();
        for (i, chunk) in buf.chunks_mut(MAX_BLOCKS_PER_CMD * SECTOR).enumerate() {
            d.rw(lba + (i * MAX_BLOCKS_PER_CMD) as u64, Buf::Read(chunk))
                .map_err(errno_for)?;
        }
        Ok(())
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()> {
        if self.ro {
            return Err(Errno::EROFS);
        }
        self.check(lba, buf.len())?;
        self.io[2].fetch_add(1, Ordering::Relaxed);
        self.io[3].fetch_add((buf.len() / SECTOR) as u64, Ordering::Relaxed);
        let d = self.inner.lock();
        for (i, chunk) in buf.chunks(MAX_BLOCKS_PER_CMD * SECTOR).enumerate() {
            d.rw(lba + (i * MAX_BLOCKS_PER_CMD) as u64, Buf::Write(chunk))
                .map_err(errno_for)?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        // Every write completes (the card's programming state ends) before write_blocks returns:
        // transfer complete waits for the busy line, so there is nothing cached to flush.
        Ok(())
    }

    fn io_stats(&self) -> Option<blockdev::IoStats> {
        let g = |i: usize| self.io[i].load(Ordering::Relaxed);
        Some(blockdev::IoStats {
            reads: g(0),
            sectors_read: g(1),
            writes: g(2),
            sectors_written: g(3),
        })
    }
}

/// Bring a card up on `host`. `None` (with a log line) if there is no card or it does not
/// answer sensibly.
fn init_card(host: &Host, base_hz: u32) -> Option<Card> {
    if !host.reset(RESET_ALL) {
        kprintln!("[sdhci] {:#x}: reset did not finish", host.base);
        return None;
    }
    // Power: 3.3 V. Then the card clock at the identification rate.
    host.w32(HOSTCTL, host.r32(HOSTCTL) & !0xff00 | PWR_ON_3V3);
    // Status bits on (polled: the signals stay off): command, transfer, buffers, card, all errors.
    host.w32(INT_ENABLE, 0x01ff_00ff);
    host.w32(INT_SIGNAL, 0);
    if host.set_clock(base_hz, ID_HZ) == 0 {
        kprintln!("[sdhci] {:#x}: the clock would not start", host.base);
        return None;
    }
    crate::timer::delay_ms(2); // power and clock settle before the first command
    // The Pi 4's EMMC2 does not wire card detect (the card-present bit stays 0 with a card in): try
    // regardless. Elsewhere (qemu's older controller) the bit is trustworthy and saves a timeout.
    if host.r32(STATE) & ST_CARD_PRESENT == 0 && host.base != EMMC2 {
        return None;
    }

    let fail = |what: &str, f: Fail| -> Option<Card> {
        kprintln!("[sdhci] {:#x}: {what} failed: {f:?}", host.base);
        None
    };
    if let Err(f) = host.command(&Command::new(cmd::GO_IDLE_STATE, 0, Resp::None), 0) {
        return fail("CMD0", f);
    }
    crate::timer::delay_ms(2);
    // CMD8: a version 2 card echoes the argument; no answer means a version 1 card.
    let v2 = match host.command(
        &Command::new(cmd::SEND_IF_COND, sd::IF_COND_ARG, Resp::R48),
        0,
    ) {
        Ok(()) => sd::if_cond_ok(host.r32(RESP)),
        Err(_) => false,
    };
    // ACMD41 until the card says it is powered up (bounded: up to a second).
    let t = Timeout::ms(1000);
    let ocr = loop {
        if let Err(f) = host.command(&Command::new(cmd::APP_CMD, 0, Resp::R48), 0) {
            return fail("CMD55", f);
        }
        if let Err(f) = host.command(
            &Command::new(cmd::SD_SEND_OP_COND, sd::op_cond_arg(v2), Resp::R48),
            0,
        ) {
            return fail("ACMD41", f);
        }
        let ocr = host.r32(RESP);
        if sd::ocr_ready(ocr) {
            break ocr;
        }
        if t.expired() {
            kprintln!(
                "[sdhci] {:#x}: the card never finished powering up",
                host.base
            );
            return None;
        }
        crate::timer::delay_ms(5);
    };
    let high_capacity = sd::ocr_high_capacity(ocr);
    if let Err(f) = host.command(&Command::new(cmd::ALL_SEND_CID, 0, Resp::R136), 0) {
        return fail("CMD2", f);
    }
    if let Err(f) = host.command(&Command::new(cmd::SEND_RELATIVE_ADDR, 0, Resp::R48), 0) {
        return fail("CMD3", f);
    }
    let (rca, _) = sd::parse_r6(host.r32(RESP));
    if rca == 0 {
        kprintln!("[sdhci] {:#x}: the card gave RCA 0", host.base);
        return None;
    }
    let rca = (rca as u32) << 16;
    if let Err(f) = host.command(&Command::new(cmd::SEND_CSD, rca, Resp::R136), 0) {
        return fail("CMD9", f);
    }
    let Some(csd) = sd::parse_csd(host.response()) else {
        kprintln!("[sdhci] {:#x}: unreadable CSD", host.base);
        return None;
    };
    if let Err(f) = host.command(&Command::new(cmd::SELECT_CARD, rca, Resp::R48Busy), 0) {
        return fail("CMD7", f);
    }
    // 4-bit bus (ACMD6), then the transfer clock; a standard capacity card needs the block length.
    let ok4 = host
        .command(&Command::new(cmd::APP_CMD, rca, Resp::R48), 0)
        .and_then(|()| host.command(&Command::new(cmd::SET_BUS_WIDTH, 2, Resp::R48), 0))
        .is_ok();
    if ok4 {
        host.w32(HOSTCTL, host.r32(HOSTCTL) | HC_4BIT);
    }
    if !high_capacity {
        let _ = host.command(
            &Command::new(cmd::SET_BLOCKLEN, SECTOR as u32, Resp::R48),
            0,
        );
    }
    let hz = host.set_clock(base_hz, XFER_HZ);
    if hz == 0 {
        kprintln!(
            "[sdhci] {:#x}: the transfer clock would not start",
            host.base
        );
        return None;
    }
    kprintln!(
        "[sdhci] {:#x}: card ready, {} blocks ({} MiB), {}, {} bit, {} kHz",
        host.base,
        csd.blocks,
        csd.blocks >> 11,
        if high_capacity { "SDHC/SDXC" } else { "SDSC" },
        if ok4 { 4 } else { 1 },
        hz / 1000
    );
    Some(Card {
        high_capacity,
        blocks: csd.blocks,
    })
}

/// Look for an SD card on the controllers and register the first one found as `mmcblk0`.
pub fn init() {
    for base in [EMMC2, EMMC1] {
        let host = Host { base };
        let version = host.r32(VERSION) >> 16;
        // An absent controller reads all ones (or zero); a real one says 0x0002 (spec 3.0) or less.
        if version == 0 || version == 0xffff {
            continue;
        }
        let caps = host.r32(CAPS);
        // The firmware knows the base clock (CAPS is wrong on the BCM2711).
        let from_fw = if base == EMMC2 {
            mailbox::clock_rate(rpikit::mailbox::clock::EMMC2).unwrap_or(0)
        } else {
            mailbox::clock_rate(rpikit::mailbox::clock::EMMC).unwrap_or(0)
        };
        let from_caps = ((caps >> 8) & 0xff) * 1_000_000;
        let base_hz = if from_fw != 0 { from_fw } else { from_caps };
        if version & 0xff < 1 || base_hz == 0 {
            kprintln!(
                "[sdhci] {base:#x}: SDHCI version {:#x}, base clock {base_hz} Hz: not usable",
                version
            );
            continue;
        }
        let Some(card) = init_card(&host, base_hz) else {
            continue;
        };
        let blocks = card.blocks;
        let writable = crate::fwcfg::find_file("opt/milkos/sdwrite").is_some_and(|f| {
            let mut b = [0u8; 4];
            f.size as usize <= b.len()
                && crate::fwcfg::read_file(f, &mut b[..f.size as usize])
                && &b[..f.size as usize] == b"1"
        });
        if !writable {
            kprintln!("[sdhci] the card is read-only (milk.sdwrite=1 on the command line allows writes)");
        }
        let dev = SdCard {
            name: format!("mmcblk{}", 0),
            blocks,
            ro: !writable,
            inner: Mutex::new(Inner { host, card }),
            io: [const { AtomicU64::new(0) }; 4],
        };
        let dev: blockdev::BlockRef = Arc::new(dev);
        kprintln!("[sdhci] {}", blockdev::describe(&dev));
        blockdev::register(dev);
        return;
    }
    kprintln!("[sdhci] no SD card");
}
