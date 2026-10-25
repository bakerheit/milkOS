//! The SD card log: everything the system says, for the whole session and across boots, in a file on the card's FAT
//! boot partition (docs/raspberry-pi.md, "Reading the log").
//!
//! A board with no serial cable and a screen that may not light up still has an SD card that fits a Mac. The Mac makes
//! a file `MILKLOG.TXT` on the boot partition before the run (scripts/pi-logfile.sh, 8 MiB by default); this module
//! writes the log into it (the text is collected by [`super::logbuf`]; the file's layout, a ring of sectors holding
//! many boots, is rpikit::milklog):
//!
//! * The kernel **never creates a file and never changes the FAT or a directory.** It reads the partition table, the
//!   FAT32 boot sector, the root directory and the file's cluster chain (rpikit::fat, with all the checks that a
//!   hostile card needs), finds where the previous boots' text ends (rpikit::milklog::recover: a header that is only a
//!   hint, then a scan of the sectors, which each say what they are) and then only overwrites sectors of that one
//!   existing file, in place. A power cut in the middle can only lose the text of the flush that was running.
//! * Every sector written is first proved to be inside the file's chain ([`LogFile::contains`]); a card that does not
//!   have the file, or has it in a shape we do not trust (or a file holding something else), is left alone (one log
//!   line says why).
//! * It does not use the block device, milkfs, the VFS or `milk.sdwrite=1`. `milk.sdwrite=1` protects `/dev/mmcblk0`
//!   from ordinary writes (a card that boots another system must not be written by accident); this is a separate path
//!   (`sdhci::raw_write`) that only this module calls, only for the log file's sectors, and it works with interrupts
//!   off and no allocator, so a crash can still flush.
//! * **When**: as soon as the card is up (everything logged so far, after a banner for this boot), after each LED
//!   stage, at least once a second from the low priority `milklog` thread when there is new text (a burst of text
//!   makes it write sooner), on a clean shutdown or restart through the power manager ([`shutdown_flush`]), on request
//!   (`logsync`, [`sync_now`]) and in the crash path ([`crash_flush`]) with the crash dump in it. The same thread writes a
//!   system report when the boot has finished and every five minutes after that (rpi/logreport.rs).
//! * **Errors**: a failed write is retried a few times at once; a flush that still fails is tried again a second later;
//!   three flushes in a row that fail stop the card log for the rest of the boot (the system keeps running; the LED
//!   shows a slow even blink, 1 s on / 1 s off, once the boot is over, and the screens and serial port say so).
//! * **LED**: after the last flush, in a quiet state, the "log saved" signal (two short blinks, then dark until the
//!   next second; led.rs) means everything logged is on the card *and the latest flush succeeded*.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use alloc::string::String;
use alloc::sync::Arc;
use rpikit::fat::{self, LogFile, SECTOR, Sectors};
use rpikit::milklog::{self, Log, OUT_SECTORS, PAYLOAD, Refuse, SCRATCH_SECTORS, Source};

use super::{led, logbuf, logreport, mailbox, sdhci};
use crate::fs::node::{Inode, InodeType, Stat, makedev};
use crate::fs::{self, Errno};
use crate::kprintln;

/// The file's name in the root directory of the boot partition.
const NAME: &str = "MILKLOG.TXT";
/// Log text taken from the buffer per round (what one round of sectors can hold).
const TEXT_CAP: usize = OUT_SECTORS * PAYLOAD;
/// How long without new text before the log counts as saved (ms).
const QUIET_MS: u64 = 2000;
/// A flush that fails this many times in a row stops the card log.
const MAX_FAILS: u32 = 3;
/// Locating may fail to read this many times before giving up.
const MAX_PROBES: u32 = 3;
/// Tries per write (and per read) before a flush counts as failed.
const TRIES: u32 = 4;
/// The system report is written this often (ms of uptime).
const REPORT_EVERY_MS: u64 = 5 * 60 * 1000;
/// The header is written at least this often while there is new text (ms).
const HEADER_EVERY_MS: u64 = 60 * 1000;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Phase {
    /// The card has not been looked at yet.
    Unprobed = 0,
    /// The file is found; the log is being written.
    Active = 1,
    /// No file, a file we do not trust, or a file holding something else: nothing is written (the normal case for a
    /// card without the log file).
    Off = 2,
    /// Writes failed repeatedly: nothing more is written this boot.
    Failed = 3,
}

struct State {
    phase: Phase,
    probes: u32,
    file: LogFile,
    /// Sectors of the file in use (whole sectors of its size).
    usable: u32,
    log: Option<Log>,
    /// How the previous boot ended, as the header said (None: unknown or first boot).
    prev_boot: u32,
    prev_state: Option<u32>,
    recovered_how: &'static str,
    /// The boot's banner is in the file / the header with this boot in it is.
    banner_written: bool,
    boot_started: bool,
    fails: u32,
    /// Text taken from the buffer and not written yet (a write failed): `text[carry_off..carry_off + carry_len]`.
    text: [u8; TEXT_CAP],
    carry_off: usize,
    carry_len: usize,
    out: [u8; OUT_SECTORS * SECTOR],
    scratch: [u8; SCRATCH_SECTORS * SECTOR],
    header: [u8; SECTOR],
    last_header_ms: u64,
    last_flush_ms: u64,
    first_write_done: bool,
}

struct Cell(UnsafeCell<State>);
// SAFETY: access is serialised by BUSY (and, in a crash, deliberately not: see `crash_flush`).
unsafe impl Sync for Cell {}

static STATE: Cell = Cell(UnsafeCell::new(State {
    phase: Phase::Unprobed,
    probes: 0,
    file: LogFile::empty(),
    usable: 0,
    log: None,
    prev_boot: 0,
    prev_state: None,
    recovered_how: "",
    banner_written: false,
    boot_started: false,
    fails: 0,
    text: [0; TEXT_CAP],
    carry_off: 0,
    carry_len: 0,
    out: [0; OUT_SECTORS * SECTOR],
    scratch: [0; SCRATCH_SECTORS * SECTOR],
    header: [0; SECTOR],
    last_header_ms: 0,
    last_flush_ms: 0,
    first_write_done: false,
}));
static BUSY: AtomicBool = AtomicBool::new(false);
/// Set once the kernel is dying: the thread and milestones stop, the crash flush owns the file.
static CRASHING: AtomicBool = AtomicBool::new(false);
/// The CPU (id + 1) that is flushing for a crash.
static CRASH_CPU: AtomicU32 = AtomicU32::new(0);

// What other contexts may ask without the state (all plain atomics).
static PHASE: AtomicU8 = AtomicU8::new(Phase::Unprobed as u8);
/// Microseconds on the counter when the last sector was written.
static LAST_WRITE_US: AtomicU64 = AtomicU64::new(0);
/// 0 never failed; else the number of failed flushes in a row.
static FAILS: AtomicU32 = AtomicU32::new(0);
static BOOT_NO: AtomicU32 = AtomicU32::new(0);
static FILE_BYTES: AtomicU64 = AtomicU64::new(0);
static RING_SECTORS: AtomicU32 = AtomicU32::new(0);
static POS: AtomicU64 = AtomicU64::new(0);
static LAPS: AtomicU64 = AtomicU64::new(0);
static FLUSHES: AtomicU64 = AtomicU64::new(0);
static SECTORS_WRITTEN: AtomicU64 = AtomicU64::new(0);
static HEADER_WRITES: AtomicU64 = AtomicU64::new(0);
static WRITE_ERRORS: AtomicU64 = AtomicU64::new(0);
static READ_ERRORS: AtomicU64 = AtomicU64::new(0);
static LAST_OK_MS: AtomicU64 = AtomicU64::new(0);
static REPORTS: AtomicU64 = AtomicU64::new(0);
static LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);
static REPORTED_BOOT_END: AtomicBool = AtomicBool::new(false);

fn set_phase(st: &mut State, p: Phase) {
    st.phase = p;
    PHASE.store(p as u8, Ordering::Release);
    if matches!(p, Phase::Off | Phase::Failed) {
        logbuf::disable();
    }
}

/// Is the card log being written?
pub fn active() -> bool {
    PHASE.load(Ordering::Acquire) == Phase::Active as u8
}

/// Did writes fail so often that the card log gave up?
pub fn failed() -> bool {
    PHASE.load(Ordering::Acquire) == Phase::Failed as u8
}

struct Card {
    crash: bool,
}

impl Sectors for Card {
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> bool {
        read_retry(lba, buf, self.crash)
    }
}

fn pause(crash: bool) {
    if !crash {
        crate::timer::delay_ms(3);
    }
}

fn read_retry(lba: u64, buf: &mut [u8], crash: bool) -> bool {
    for t in 0..if crash { 2 } else { TRIES } {
        if sdhci::raw_read(lba, buf, crash) {
            return true;
        }
        READ_ERRORS.fetch_add(1, Ordering::Relaxed);
        if t + 1 < TRIES {
            pause(crash);
        }
    }
    false
}

fn write_retry(lba: u64, data: &[u8], crash: bool) -> bool {
    for t in 0..if crash { 2 } else { TRIES } {
        if sdhci::raw_write(lba, data, crash) {
            return true;
        }
        WRITE_ERRORS.fetch_add(1, Ordering::Relaxed);
        if t + 1 < TRIES {
            pause(crash);
        }
    }
    false
}

/// The log file's sectors, by file sector number, for [`milklog::recover`].
struct FileReader<'a> {
    file: &'a LogFile,
    usable: u32,
    crash: bool,
}

impl Source for FileReader<'_> {
    fn read(&mut self, sector: u32, out: &mut [u8]) -> bool {
        let count = (out.len() / SECTOR) as u32;
        if sector.checked_add(count).is_none_or(|e| e > self.usable) {
            return false;
        }
        let mut done = 0;
        while done < count {
            let Some((lba, n)) = self.file.run(sector + done, count - done) else {
                return false;
            };
            let (a, b) = (done as usize * SECTOR, (done + n) as usize * SECTOR);
            if !read_retry(lba, &mut out[a..b], self.crash) {
                return false;
            }
            done += n;
        }
        true
    }
}

/// Write `count` file sectors from `idx` (the data is `count * 512` bytes). Every run is checked against the file's
/// chain right before it is written.
fn write_file_sectors(file: &LogFile, idx: u32, count: u32, data: &[u8], crash: bool) -> bool {
    let mut done = 0u32;
    while done < count {
        let Some((lba, n)) = file.run(idx + done, count - done) else {
            return false;
        };
        if !file.contains(lba, n) {
            return false;
        }
        let (a, b) = (done as usize * SECTOR, (done + n) as usize * SECTOR);
        if !write_retry(lba, &data[a..b], crash) {
            return false;
        }
        done += n;
    }
    LAST_WRITE_US.store(mailbox::now_us(), Ordering::Relaxed);
    SECTORS_WRITTEN.fetch_add(u64::from(count), Ordering::Relaxed);
    true
}

/// Run `f` with the state if nobody else has it.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    if BUSY
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    // SAFETY: BUSY is ours.
    let r = f(unsafe { &mut *STATE.0.get() });
    BUSY.store(false, Ordering::Release);
    Some(r)
}

/// Like [`with_state`], but waits (up to about `ms`) for a flush in progress.
fn with_state_wait<R>(ms: u64, mut f: impl FnMut(&mut State) -> R) -> Option<R> {
    let t = super::Timeout::ms(ms);
    loop {
        if let Some(r) = with_state(&mut f) {
            return Some(r);
        }
        if t.expired() {
            return None;
        }
        if crate::sched::started() && !CRASHING.load(Ordering::Relaxed) {
            crate::sched::sleep_ms(5);
        } else {
            core::hint::spin_loop();
        }
    }
}

fn boot_id_ticks() -> (u64, u64) {
    let (c, f): (u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) c, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    (c, f)
}

/// Look at the card, if there is one and we have not yet. Logs what it finds.
fn probe(st: &mut State, crash: bool) {
    if st.phase != Phase::Unprobed || sdhci::raw_blocks().is_none() {
        return;
    }
    st.probes += 1;
    let Some(name) = fat::short_name(NAME) else {
        set_phase(st, Phase::Off);
        return;
    };
    match fat::locate(&mut Card { crash }, &name) {
        Ok(found) => {
            st.file = found.file;
            let v = &found.volume;
            kprintln!(
                "milklog: {NAME} found: partition at sector {} ({} sectors), FAT32 with {} sectors per cluster, file of {} bytes in {} piece(s), first sector {}",
                found.partition.start,
                found.partition.sectors,
                v.sectors_per_cluster,
                st.file.size(),
                st.file.extent_count(),
                st.file.lba(0).unwrap_or(0)
            );
            open_log(st, crash);
        }
        Err(fat::Error::Read(lba)) if st.probes < MAX_PROBES => {
            kprintln!(
                "milklog: could not read sector {lba} of the card (try {} of {MAX_PROBES})",
                st.probes
            );
        }
        Err(e) => {
            set_phase(st, Phase::Off);
            // A card that simply has no log file (a milkfs card, a card without the file) is the normal case:
            // one line, no alarm.
            kprintln!("milklog: not writing a log file: {}", e.what());
        }
    }
}

/// The file is there: find where the earlier boots' text ends and get ready to start this boot.
fn open_log(st: &mut State, crash: bool) {
    st.usable = (st.file.size() / SECTOR as u32).min(st.file.sectors());
    FILE_BYTES.store(u64::from(st.file.size()), Ordering::Relaxed);
    let State {
        file, usable, scratch, ..
    } = st;
    let mut rd = FileReader {
        file,
        usable: *usable,
        crash,
    };
    let rec = match milklog::recover(&mut rd, *usable, scratch) {
        Ok(r) => r,
        Err(Refuse::Read(s)) if st.probes < MAX_PROBES => {
            kprintln!("milklog: could not read sector {s} of the log file (try {} of {MAX_PROBES})", st.probes);
            return;
        }
        Err(e) => {
            kprintln!("milklog: not writing the log file: {}", e.what());
            set_phase(st, Phase::Off);
            return;
        }
    };
    let Some(mut log) = Log::resume(st.usable, &rec) else {
        kprintln!("milklog: not writing the log file: {}", Refuse::TooSmall.what());
        set_phase(st, Phase::Off);
        return;
    };
    let Some(boot) = log.begin_boot() else {
        kprintln!("milklog: not writing the log file: {}", Refuse::BootCounter.what());
        set_phase(st, Phase::Off);
        return;
    };
    st.prev_boot = rec.boot_count;
    st.prev_state = rec
        .header
        .as_ref()
        .and_then(|h| h.boots().last().copied())
        .filter(|b| b.boot == rec.boot_count)
        .map(|b| b.state);
    st.recovered_how = match rec.how {
        milklog::How::Fresh => "a blank file",
        milklog::How::Header => "end of the text found from the header",
        milklog::How::Scan => "end of the text found by scanning the ring (the header was missing or wrong)",
    };
    BOOT_NO.store(boot, Ordering::Relaxed);
    RING_SECTORS.store(log.ring_sectors(), Ordering::Relaxed);
    POS.store(log.pos(), Ordering::Relaxed);
    LAPS.store(log.lap(), Ordering::Relaxed);
    kprintln!(
        "milklog: log format 2: ring of {} sectors ({} KiB of text); {}; {} bytes of text so far (lap {}); this is boot {boot} of this file; the log is written into it from now on",
        log.ring_sectors(),
        log.ring_bytes() / 1024,
        st.recovered_how,
        rec.pos,
        log.lap()
    );
    st.log = Some(log);
    set_phase(st, Phase::Active);
}

fn state_name(s: Option<u32>) -> &'static str {
    match s {
        Some(milklog::STATE_SHUTDOWN) => "shutdown",
        Some(milklog::STATE_RESTART) => "restart",
        Some(milklog::STATE_CRASH) => "crash",
        Some(_) => "no end recorded (power cut, reset or still running)",
        None => "unknown (first boot in this file, or the header was lost)",
    }
}

/// The first text of a boot.
fn banner(st: &State, out: &mut [u8]) -> usize {
    use core::fmt::Write;
    struct W<'a>(&'a mut [u8], usize);
    impl Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let n = s.len().min(self.0.len() - self.1);
            self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
            self.1 += n;
            Ok(())
        }
    }
    let (c, f) = boot_id_ticks();
    let boot = st.log.as_ref().map_or(0, Log::boot);
    let mut w = W(out, 0);
    let _ = write!(
        w,
        "\n{}{boot} === milkOS {} | build {} | git {} | log format 2, file {} bytes | uptime {} ms when the log was opened (counter {c} at {f} Hz)\n",
        milklog::BANNER_PREFIX,
        crate::VERSION,
        env!("MILKOS_BUILD"),
        env!("MILKOS_GIT"),
        st.file.size(),
        logbuf::uptime_ms()
    );
    if boot > 1 || st.prev_boot > 0 {
        let _ = writeln!(
            w,
            "previous boot {}: {}",
            st.prev_boot,
            state_name(st.prev_state)
        );
    }
    w.1
}

/// Write the sectors of `p` (built in `st.out`).
fn write_prepared(st: &mut State, p: &milklog::Prepared, crash: bool) -> bool {
    let Some(log) = st.log.as_ref() else {
        return false;
    };
    let mut i = 0u32;
    while i < p.count {
        let idx = log.file_sector(p.first_abs.wrapping_add(i));
        let mut j = 1u32;
        while i + j < p.count && log.file_sector(p.first_abs.wrapping_add(i + j)) == idx + j {
            j += 1;
        }
        let (a, b) = (i as usize * SECTOR, (i + j) as usize * SECTOR);
        if !write_file_sectors(&st.file, idx, j, &st.out[a..b], crash) {
            return false;
        }
        i += j;
    }
    true
}

/// Write the header (the next copy) if it is due; `force`: if anything is new since the last one.
fn checkpoint(st: &mut State, force: bool, crash: bool) -> bool {
    let now = logbuf::uptime_ms();
    let due = match st.log.as_ref() {
        Some(l) => {
            l.header_due() || (l.header_stale() && (force || now.saturating_sub(st.last_header_ms) >= HEADER_EVERY_MS))
        }
        None => false,
    };
    if !due {
        return true;
    }
    write_header(st, crash)
}

fn write_header(st: &mut State, crash: bool) -> bool {
    let State {
        log, header, file, last_header_ms, ..
    } = st;
    let Some(log) = log.as_mut() else {
        return false;
    };
    let at = log.make_header(header);
    if !write_file_sectors(file, at, 1, header, crash) {
        return false;
    }
    log.header_written(header);
    HEADER_WRITES.store(u64::from(log.header_writes()), Ordering::Relaxed);
    *last_header_ms = logbuf::uptime_ms();
    true
}

/// Write the boot banner, once, and the header with this boot in it (each step is retried by itself if it failed).
fn start_boot(st: &mut State, crash: bool) -> bool {
    if !st.banner_written {
        let mut b = [0u8; 400];
        let n = banner(st, &mut b);
        let Some(log) = st.log.as_ref() else {
            return false;
        };
        let Some(p) = log.prepare(&b[..n], &mut st.out) else {
            return false;
        };
        if !write_prepared(st, &p, crash) {
            return false;
        }
        if let Some(log) = st.log.as_mut() {
            log.commit(&p);
        }
        st.banner_written = true;
    }
    let ok = write_header(st, crash);
    st.boot_started = ok;
    ok
}

/// Save everything logged since the last flush, in as many rounds as it takes (at most `max_rounds`). Returns true if
/// the file is now up to date.
fn flush(st: &mut State, crash: bool, max_rounds: u32) -> bool {
    if st.phase != Phase::Active {
        return false;
    }
    if !st.boot_started && !start_boot(st, crash) {
        return fail(st);
    }
    for _ in 0..max_rounds {
        if st.carry_len == 0 {
            let Some(n) = logbuf::take(&mut st.text, crash) else {
                return false; // the buffer is busy: next time
            };
            if n == 0 {
                st.fails = 0;
                FAILS.store(0, Ordering::Relaxed);
                return checkpoint(st, false, crash) || fail(st);
            }
            st.carry_off = 0;
            st.carry_len = n;
        }
        let Some(log) = st.log.as_ref() else {
            return false;
        };
        let (off, len) = (st.carry_off, st.carry_len);
        let Some(p) = log.prepare(&st.text[off..off + len], &mut st.out) else {
            kprintln!("milklog: the log file cannot take more text: no more log writing this boot");
            set_phase(st, Phase::Failed);
            return false;
        };
        if p.count > 0 && !write_prepared(st, &p, crash) {
            return fail(st);
        }
        let first_ever = !st.first_write_done && p.count > 0;
        let lap_before = st.log.as_ref().map_or(0, Log::lap);
        if let Some(log) = st.log.as_mut() {
            log.commit(&p);
        }
        if first_ever {
            st.first_write_done = true;
            // Did the first sector really land? One read-back, logged once.
            let mut back = [0u8; SECTOR];
            let ok = st.log.as_ref().is_some_and(|l| {
                let idx = l.file_sector(p.first_abs);
                st.file.lba(idx).is_some_and(|lba| {
                    sdhci::raw_read(lba, &mut back, crash) && back[..] == st.out[..SECTOR]
                })
            });
            if !ok {
                kprintln!("milklog: WARNING: the first sector written does not read back the same");
            }
        }
        st.carry_off += p.consumed;
        st.carry_len -= p.consumed;
        FLUSHES.fetch_add(1, Ordering::Relaxed);
        if let Some(log) = st.log.as_ref() {
            POS.store(log.pos(), Ordering::Relaxed);
            LAPS.store(log.lap(), Ordering::Relaxed);
            if log.lap() != lap_before {
                logbuf::exempt_lines(
                    "[milklog]",
                    "the ring is full: from here the oldest text of the earliest boots is being overwritten",
                );
            }
        }
        st.fails = 0;
        FAILS.store(0, Ordering::Relaxed);
        st.last_flush_ms = logbuf::uptime_ms();
        LAST_OK_MS.store(st.last_flush_ms, Ordering::Relaxed);
        if st.log.as_ref().is_some_and(Log::header_due) && !write_header(st, crash) {
            return fail(st);
        }
        if p.count == 0 && p.consumed == 0 {
            return true;
        }
    }
    st.carry_len == 0 && logbuf::pending() == 0
}

/// A flush failed: count it, and give up on the card after [`MAX_FAILS`] in a row.
fn fail(st: &mut State) -> bool {
    st.fails += 1;
    FAILS.store(st.fails, Ordering::Relaxed);
    if st.fails >= MAX_FAILS {
        // The state is "Failed" before the line, which is printed to the screens and the serial port (and no longer
        // collected: the log has stopped).
        set_phase(st, Phase::Failed);
        kprintln!(
            "milklog: {} failed flushes in a row: no more log writing to the card this boot (the system keeps running; the LED blinks slowly, 1 s on / 1 s off)",
            st.fails
        );
    }
    false
}

/// A milestone (an LED stage, the boot being done): look at the card if it is newly up, and save what is logged.
pub fn milestone() {
    if CRASHING.load(Ordering::Relaxed) || sdhci::raw_blocks().is_none() {
        return;
    }
    logbuf::flush_partials(false);
    let _ = with_state(|st| {
        probe(st, false);
        flush(st, false, 80);
        super::serror_checkpoint("logfile-flush");
    });
}

/// The periodic flush (the low priority thread calls this).
pub fn tick() {
    if CRASHING.load(Ordering::Relaxed) {
        return;
    }
    logbuf::flush_partials(false);
    let _ = with_state(|st| {
        probe(st, false);
        let late = logbuf::uptime_ms().saturating_sub(st.last_flush_ms) >= 1000;
        if logbuf::pending() > 0 && (late || logbuf::pending() >= 4096 || !st.boot_started) {
            // A backlog (a burst of text) is cleared faster than the usual few rounds.
            flush(st, false, if logbuf::pending() > 32 * 1024 { 24 } else { 4 });
        } else if st.phase == Phase::Active {
            let _ = checkpoint(st, false, false);
        }
        super::serror_checkpoint("logfile-flush");
    });
}

/// `/dev/milklog`: read it for where the log stands; write `sync` to it to save everything now (the `logsync` command).
struct LogDev {
    dev: u64,
}

/// The last `sync` request (ms of uptime): at most one every two seconds does anything (each writes the header).
static LAST_SYNC_MS: AtomicU64 = AtomicU64::new(0);

impl Inode for LogDev {
    fn ino(&self) -> u64 {
        0x6d6c
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(self.dev, 0x6d6c, InodeType::CharDev, 0o666);
        st.rdev = makedev(10, 0x6c);
        st
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        let text = status();
        let b = text.as_bytes();
        let off = usize::try_from(off).unwrap_or(usize::MAX).min(b.len());
        let n = (b.len() - off).min(buf.len());
        buf[..n].copy_from_slice(&b[off..off + n]);
        Ok(n)
    }
    fn write_at(&self, _off: u64, data: &[u8]) -> fs::Result<usize> {
        if data.starts_with(b"sync") {
            let now = logbuf::uptime_ms();
            let last = LAST_SYNC_MS.load(Ordering::Relaxed);
            if last == 0 || now.saturating_sub(last) >= 2000 {
                LAST_SYNC_MS.store(now.max(1), Ordering::Relaxed);
                if !sync_now().ok {
                    return Err(Errno::EIO);
                }
            }
        }
        Ok(data.len())
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Ok(())
    }
}

/// Start the thread that saves the log every second or so, writes the system report and shows the "log saved" signal.
pub fn start_thread() {
    let dev = fs::devfs::ids().map_or(0, |ids| ids.dev);
    if let Err(e) = fs::devfs::add_device("milklog", Arc::new(LogDev { dev })) {
        kprintln!("milklog: /dev/milklog: {e:?}");
    }
    crate::sched::spawn_prio("milklog", crate::sched::Priority::Low, || {
        let mut showing = false;
        loop {
            tick();
            report_if_due();
            storm_if_asked();
            if failed() && led::saved_signal_allowed() {
                // The card log stopped: a slow even blink.
                showing = true;
                led::saved_signal(true);
                crate::sched::sleep_ms(1000);
                led::saved_signal(false);
                crate::sched::sleep_ms(1000);
            } else if saved_quiet() && led::saved_signal_allowed() {
                showing = true;
                // Two short blinks, then dark for the rest of the second.
                led::saved_signal(true);
                crate::sched::sleep_ms(70);
                led::saved_signal(false);
                crate::sched::sleep_ms(160);
                led::saved_signal(true);
                crate::sched::sleep_ms(70);
                led::saved_signal(false);
                crate::sched::sleep_ms(700);
            } else {
                if showing {
                    // More text came: back to solid on.
                    showing = false;
                    led::saved_signal(true);
                }
                crate::sched::sleep_ms(250);
            }
        }
    });
}

/// Write a system report when the boot has finished, and every five minutes after.
fn report_if_due() {
    if !active() || !led::is_finished() {
        return;
    }
    let now = logbuf::uptime_ms();
    let last = LAST_REPORT_MS.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < REPORT_EVERY_MS {
        return;
    }
    LAST_REPORT_MS.store(now.max(1), Ordering::Relaxed);
    let n = REPORTS.fetch_add(1, Ordering::Relaxed) + 1;
    logreport::emit(n, BOOT_NO.load(Ordering::Relaxed));
}

/// `milk.logstorm=N` on the command line: once the boot is over and the first system report is out, print N lines as fast as the
/// console takes them, to test the rate cap (scripts/pi-log-multiboot-test.sh). Once.
fn storm_if_asked() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) || REPORTS.load(Ordering::Relaxed) == 0 {
        return;
    }
    DONE.store(true, Ordering::Relaxed);
    let mut text = [0u8; 12];
    let Some(f) = crate::fwcfg::find_file("opt/milkos/logstorm")
        .filter(|f| (f.size as usize) <= text.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut text[..f.size as usize]))
    else {
        return;
    };
    let n: u32 = core::str::from_utf8(&text[..f.size as usize])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
        .min(1_000_000);
    kprintln!("milklog: storm: printing {n} lines (milk.logstorm)");
    for i in 0..n {
        kprintln!("storm line {i} of {n}: the quick brown fox jumps over the lazy dog");
    }
    kprintln!("milklog: storm: done, {n} lines printed");
}

/// Is every logged byte on the card, with nothing new for a moment, and was the latest flush good? (The "safe to power
/// off" state.)
pub fn saved_quiet() -> bool {
    if !active() || FAILS.load(Ordering::Relaxed) != 0 || logbuf::pending() != 0 {
        return false;
    }
    let last = LAST_WRITE_US.load(Ordering::Relaxed);
    last != 0 && mailbox::now_us().wrapping_sub(last) >= QUIET_MS * 1000
}

/// What the status text says.
pub struct Synced {
    pub ok: bool,
}

/// Save everything now (`logsync`): the shell's pending text included, the header too. Waits for a flush in progress.
pub fn sync_now() -> Synced {
    if CRASHING.load(Ordering::Relaxed) || !(active() || PHASE.load(Ordering::Acquire) == Phase::Unprobed as u8) {
        return Synced { ok: false };
    }
    logbuf::flush_partials(true);
    let ok = with_state_wait(3000, |st| {
        probe(st, false);
        let ok = flush(st, false, 400);
        ok && checkpoint(st, true, false)
    })
    .unwrap_or(false);
    Synced { ok }
}

/// A clean shutdown or restart is about to happen (the power manager): note the end of this boot and save everything,
/// the header with the boot's end in it. `restart`: false for a shutdown.
pub fn shutdown_flush(restart: bool) {
    if CRASHING.load(Ordering::Relaxed) || !active() {
        return;
    }
    let boot = BOOT_NO.load(Ordering::Relaxed);
    let kind = if restart { "restart" } else { "shutdown" };
    let mut line = [0u8; 80];
    let n = fmt_end(&mut line, boot, kind);
    // The shell prompt that is waiting for input first, then the end line: it is the last line of the boot.
    logbuf::flush_partials(true);
    logbuf::exempt_lines("", core::str::from_utf8(&line[..n]).unwrap_or(""));
    let _ = with_state_wait(3000, |st| {
        flush(st, false, 400);
        if let Some(l) = st.log.as_mut() {
            l.end_boot(if restart {
                milklog::STATE_RESTART
            } else {
                milklog::STATE_SHUTDOWN
            });
        }
        write_header(st, false)
    });
}

fn fmt_end(out: &mut [u8], boot: u32, kind: &str) -> usize {
    use core::fmt::Write;
    struct W<'a>(&'a mut [u8], usize);
    impl Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let n = s.len().min(self.0.len() - self.1);
            self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
            self.1 += n;
            Ok(())
        }
    }
    let mut w = W(out, 0);
    let _ = write!(w, "{}{boot} ended: {kind} ===", milklog::END_PREFIX);
    w.1
}

/// The kernel is dying (a panic or an exception handler, no allocator, interrupts off): save everything
/// logged, the crash dump included, within about a second. Never blocks for long: a flush in progress on
/// another CPU gets half a second, then its state is taken over and the unwritten text is written again from the buffer.
pub fn crash_flush() {
    logbuf::emergency();
    CRASHING.store(true, Ordering::Relaxed);
    // One CPU at a time owns the file in a crash; another that crashes meanwhile leaves it to the first.
    let me = crate::percpu::id() as u32 + 1;
    match CRASH_CPU.compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {}
        Err(owner) if owner == me => {}
        Err(_) => return,
    }
    if sdhci::raw_blocks().is_none() {
        return;
    }
    let boot = BOOT_NO.load(Ordering::Relaxed);
    // Partial lines first (the crash dump may end in one), then the end line: it is the last line of the boot.
    logbuf::flush_partials(true);
    if boot != 0 && !REPORTED_BOOT_END.swap(true, Ordering::Relaxed) {
        let mut line = [0u8; 80];
        let n = fmt_end(&mut line, boot, "crash");
        logbuf::exempt_lines("", core::str::from_utf8(&line[..n]).unwrap_or(""));
    }
    let t = super::Timeout::ms(500);
    let mut stolen = false;
    while BUSY
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        if t.expired() {
            stolen = true;
            break;
        }
        core::hint::spin_loop();
    }
    // SAFETY: BUSY is ours, or (stolen) whoever had it is stuck or on its way down; the card side re-checks every run.
    let st = unsafe { &mut *STATE.0.get() };
    if stolen && st.phase == Phase::Active {
        // The state may be torn: what was taken but not written is lost; carry on from the file as it is.
        st.carry_len = 0;
    }
    probe(st, true);
    // The crash dump was just logged: a few rounds are plenty.
    flush(st, true, 24);
    if st.phase == Phase::Active {
        if let Some(l) = st.log.as_mut() {
            l.end_boot(milklog::STATE_CRASH);
        }
        let _ = write_header(st, true);
    }
    if !stolen {
        BUSY.store(false, Ordering::Release);
    }
}

// ---- reporting ----------------------------------------------------------------------------------------

/// Where the log stands, as text (what `logsync` prints, and the end of every system report).
pub fn status() -> String {
    use core::fmt::Write;
    let mut s = String::new();
    let phase = PHASE.load(Ordering::Acquire);
    let (lines, capped, ring_full, misses) = logbuf::counters();
    let pos = POS.load(Ordering::Relaxed);
    let _ = writeln!(
        s,
        "milklog: {}",
        match phase {
            p if p == Phase::Active as u8 => "writing to the log file on the card",
            p if p == Phase::Off as u8 => "off: no usable MILKLOG.TXT on the card (see the milklog: lines of the boot log)",
            p if p == Phase::Failed as u8 => "STOPPED: writes to the card failed repeatedly (the LED blinks slowly)",
            _ => "not started yet (the card has not been looked at)",
        }
    );
    if phase == Phase::Active as u8 || phase == Phase::Failed as u8 {
        let ring = RING_SECTORS.load(Ordering::Relaxed);
        let _ = writeln!(
            s,
            "milklog: file {} bytes, format 2: ring of {ring} sectors x {PAYLOAD} bytes",
            FILE_BYTES.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            s,
            "milklog: boot {} (this one); cursor {pos} bytes of text ever written (lap {}); {} bytes logged but not yet on the card",
            BOOT_NO.load(Ordering::Relaxed),
            LAPS.load(Ordering::Relaxed),
            logbuf::pending()
        );
        let _ = writeln!(
            s,
            "milklog: {} flushes, {} sectors written, {} header writes; write errors {} (retried), read errors {}; failed flushes in a row {}",
            FLUSHES.load(Ordering::Relaxed),
            SECTORS_WRITTEN.load(Ordering::Relaxed),
            HEADER_WRITES.load(Ordering::Relaxed),
            WRITE_ERRORS.load(Ordering::Relaxed),
            READ_ERRORS.load(Ordering::Relaxed),
            FAILS.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            s,
            "milklog: last good flush at uptime {} ms (now {} ms); {}",
            LAST_OK_MS.load(Ordering::Relaxed),
            logbuf::uptime_ms(),
            if saved_quiet() {
                "everything is saved (LED: log saved signal)"
            } else {
                "not saved yet or still changing"
            }
        );
    }
    let _ = writeln!(
        s,
        "milklog: {lines} lines taken, {capped} dropped by the rate cap ({} lines/s, {} bytes/s on average), {ring_full} dropped because the buffer was full, {misses} lost to lock contention; {} system reports",
        rpikit::logintake::LINES_PER_SEC,
        rpikit::logintake::BYTES_PER_SEC,
        REPORTS.load(Ordering::Relaxed)
    );
    s
}
