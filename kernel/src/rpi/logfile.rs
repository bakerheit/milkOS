//! The boot log in a file on the SD card's FAT boot partition (docs/raspberry-pi.md, "The boot log in a file").
//!
//! A board with no serial cable and no screen yet still has an SD card that the person can put in a Mac. The Mac
//! makes a file `MILKLOG.TXT` on the boot partition before the run (scripts/pi-logfile.sh); this module writes the
//! kernel log (`klog`: the text that goes to the serial port and the screens, from the first line) into it:
//!
//! * The kernel **never creates a file and never changes the FAT or a directory.** It reads the partition table, the
//!   FAT32 boot sector, the root directory and the file's cluster chain (rpikit::fat, with all the checks that a
//!   hostile card needs), and then only overwrites the data sectors of that one existing file, in place. A power cut
//!   in the middle can only lose log text.
//! * Every sector written is first proved to be inside the file's chain ([`LogFile::contains`]); a card that does not
//!   have the file, or has it in a shape we do not trust, is left alone (one log line says why).
//! * It does not use the block device, milkfs, the VFS or `milk.sdwrite=1`. `milk.sdwrite=1` protects `/dev/mmcblk0`
//!   from ordinary writes (a card that boots another system must not be written by accident); this is a separate path
//!   (`sdhci::raw_write`) that only this module calls, only for the log file's sectors, and it works with interrupts
//!   off and no allocator, so a crash can still flush.
//! * When: as soon as the card is up (everything logged so far), after each LED stage (a milestone), every
//!   second or so from a low priority thread ([`start_thread`]), and in the crash path ([`crash_flush`]) with the crash
//!   dump in it. After the last flush, in a quiet state, the LED shows the "log saved" signal (see led.rs).
//!
//! The layout of the text and the end marker is rpikit::logimg.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use rpikit::fat::{self, LogFile, SECTOR, Sectors};
use rpikit::logimg::{LogImage, MARKER_MAX, OUT_SECTORS};

use super::{led, mailbox, sdhci};
use crate::kprintln;

/// The file's name in the root directory of the boot partition.
const NAME: &str = "MILKLOG.TXT";
/// Bytes of sectors one write round builds.
const CHUNK: usize = OUT_SECTORS * SECTOR;
/// Room before the log text for the "bytes lost" note.
const NOTE_MAX: usize = 96;
/// Log text taken from the ring per round (what `prepare` can always take in one go).
const RING_CAP: usize = CHUNK - SECTOR - MARKER_MAX - NOTE_MAX;
/// How long without new text before the log counts as saved (ms).
const QUIET_MS: u64 = 2000;
/// A card that fails this many writes in a row is given up on.
const MAX_FAILS: u32 = 3;
/// Locating may fail to read this many times before giving up.
const MAX_PROBES: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The card has not been looked at yet.
    Unprobed,
    /// The file is found; the log is being written.
    Active,
    /// No file, a file we do not trust, or too many failures: nothing is written.
    Off,
}

struct State {
    phase: Phase,
    probes: u32,
    file: LogFile,
    image: LogImage,
    /// Log bytes (klog positions) saved up to here.
    ring_pos: u64,
    header_done: bool,
    fails: u32,
    text: [u8; CHUNK],
    out: [u8; CHUNK],
}

struct Cell(UnsafeCell<State>);
// SAFETY: access is serialised by BUSY (and, in a crash, deliberately not: see `crash_flush`).
unsafe impl Sync for Cell {}

static STATE: Cell = Cell(UnsafeCell::new(State {
    phase: Phase::Unprobed,
    probes: 0,
    file: LogFile::empty(),
    image: LogImage::new(0),
    ring_pos: 0,
    header_done: false,
    fails: 0,
    text: [0; CHUNK],
    out: [0; CHUNK],
}));
static BUSY: AtomicBool = AtomicBool::new(false);
/// Set once the kernel is dying: the thread and milestones stop, the crash flush owns the file.
static CRASHING: AtomicBool = AtomicBool::new(false);
/// The CPU (id + 1) that is flushing for a crash.
static CRASH_CPU: AtomicU32 = AtomicU32::new(0);

// What other contexts may ask without the state (all plain atomics).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Log position saved (mirror of `ring_pos`).
static SAVED_POS: AtomicU64 = AtomicU64::new(0);
/// Microseconds on the counter when the last sector was written.
static LAST_WRITE_US: AtomicU64 = AtomicU64::new(0);
/// 0 never failed; else the number of failed writes in a row.
static FAILS: AtomicU32 = AtomicU32::new(0);

struct Card {
    crash: bool,
}

impl Sectors for Card {
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> bool {
        sdhci::raw_read(lba, buf, self.crash)
    }
}

/// Run `f` with the state if nobody else has it. IRQs stay off while it runs (a flush is a few
/// sectors; the first one is the whole boot log so far).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let _irq = crate::sync::IrqGuard::new();
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

fn boot_id() -> u32 {
    let c: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) c, options(nomem, nostack)) };
    (c ^ (c >> 32)) as u32 | 1
}

/// Look at the card, if there is one and we have not yet. Logs what it finds.
fn probe(st: &mut State, crash: bool) {
    if st.phase != Phase::Unprobed || sdhci::raw_blocks().is_none() {
        return;
    }
    st.probes += 1;
    let Some(name) = fat::short_name(NAME) else {
        st.phase = Phase::Off;
        return;
    };
    match fat::locate(&mut Card { crash }, &name) {
        Ok(found) => {
            st.file = found.file;
            st.image = LogImage::new(boot_id());
            st.ring_pos = 0;
            st.header_done = false;
            st.phase = Phase::Active;
            ACTIVE.store(true, Ordering::Release);
            let v = &found.volume;
            kprintln!(
                "milklog: {NAME} found: partition at sector {} ({} sectors), FAT32 with {} sectors per cluster, file of {} bytes in {} piece(s), first sector {}; the boot log is written into it from now on",
                found.partition.start,
                found.partition.sectors,
                v.sectors_per_cluster,
                st.file.size(),
                st.file.extent_count(),
                st.file.lba(0).unwrap_or(0)
            );
        }
        Err(fat::Error::Read(lba)) if st.probes < MAX_PROBES => {
            kprintln!(
                "milklog: could not read sector {lba} of the card (try {} of {MAX_PROBES})",
                st.probes
            );
        }
        Err(e) => {
            st.phase = Phase::Off;
            // A card that simply has no log file (a milkfs card, a card without the file) is the normal case:
            // one line, no alarm.
            kprintln!("milklog: not writing a boot log file: {}", e.what());
        }
    }
}

/// The header that starts the file.
fn header(boot: u32, size: u32, pieces: usize, out: &mut [u8]) -> usize {
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
    let _ = write!(
        w,
        "MILKLOG 1 boot={:08x}\nmilkOS {} (build {}) for the Raspberry Pi 4\nlog file: {} bytes in {} piece(s) on the card; saved by the kernel in place (docs/raspberry-pi.md)\n--------\n",
        boot,
        crate::VERSION,
        env!("MILKOS_BUILD"),
        size,
        pieces
    );
    w.1
}

/// Write the sectors of `p` (built in `st.out`) to the file, run by run. Every run is checked against the
/// file's chain right before it is written.
fn write_prepared(st: &mut State, p: &rpikit::logimg::Prepared, crash: bool) -> bool {
    let mut idx = p.first;
    let mut done = 0u32;
    while done < p.count {
        let Some((lba, n)) = st.file.run(idx, p.count - done) else {
            return false;
        };
        if !st.file.contains(lba, n) {
            return false;
        }
        let a = done as usize * SECTOR;
        let b = a + n as usize * SECTOR;
        if !sdhci::raw_write(lba, &st.out[a..b], crash) {
            return false;
        }
        idx += n;
        done += n;
    }
    LAST_WRITE_US.store(mailbox::now_us(), Ordering::Relaxed);
    true
}

/// Save everything logged since the last flush, in as many rounds as it takes (at most `max_rounds`). Returns true if
/// the file is now up to date.
fn flush(st: &mut State, crash: bool, max_rounds: u32) -> bool {
    if st.phase != Phase::Active {
        return false;
    }
    for _ in 0..max_rounds {
        let (len, ring_start, note_len, ring_n);
        if !st.header_done {
            len = header(
                st.image.boot_id(),
                st.file.size(),
                st.file.extent_count(),
                &mut st.text,
            );
            (ring_start, note_len, ring_n) = (st.ring_pos, 0, 0);
        } else {
            let Some(c) = crate::klog::copy_since(
                st.ring_pos,
                &mut st.text[NOTE_MAX..NOTE_MAX + RING_CAP],
                crash,
            ) else {
                return false; // the ring is busy: next time
            };
            if c.n == 0 && c.lost == 0 {
                return true; // nothing new
            }
            ring_start = c.next - c.n as u64;
            ring_n = c.n;
            note_len = if c.lost > 0 {
                let mut tmp = [0u8; NOTE_MAX];
                let mut w = NoteWriter(&mut tmp, 0);
                let _ = core::fmt::Write::write_fmt(
                    &mut w,
                    format_args!(
                        "[milklog: {} bytes of log were overwritten in the ring before they were saved]\n",
                        c.lost
                    ),
                );
                let n = w.1;
                st.text[NOTE_MAX - n..NOTE_MAX].copy_from_slice(&tmp[..n]);
                n
            } else {
                0
            };
            len = note_len + c.n;
        }
        let was_header = !st.header_done;
        let start = if was_header { 0 } else { NOTE_MAX - note_len };
        let text = &st.text[start..start + len];
        let Some(p) = st.image.prepare(text, st.file.sectors(), &mut st.out) else {
            kprintln!("milklog: the log file is too small to use");
            st.phase = Phase::Off;
            ACTIVE.store(false, Ordering::Release);
            return false;
        };
        let first_ever = st.image.flushes() == 0;
        if !write_prepared(st, &p, crash) {
            st.fails += 1;
            FAILS.store(st.fails, Ordering::Relaxed);
            if st.fails >= MAX_FAILS {
                kprintln!(
                    "milklog: {} failed writes in a row: no more log writing this boot",
                    st.fails
                );
                st.phase = Phase::Off;
                ACTIVE.store(false, Ordering::Release);
            }
            return false;
        }
        st.fails = 0;
        FAILS.store(0, Ordering::Relaxed);
        if first_ever {
            // Did the first sector really land? One read-back, logged once.
            let mut back = [0u8; SECTOR];
            let ok = st.file.lba(p.first).is_some_and(|l| {
                sdhci::raw_read(l, &mut back, crash) && back[..] == st.out[..SECTOR]
            });
            if !ok {
                kprintln!("milklog: WARNING: the first sector written does not read back the same");
            }
        }
        st.image.commit(&p);
        if !st.header_done {
            st.header_done = true;
        } else {
            let from_ring = p.consumed.saturating_sub(note_len).min(ring_n);
            st.ring_pos = ring_start + from_ring as u64;
        }
        SAVED_POS.store(st.ring_pos, Ordering::Release);
        if !was_header && ring_n < RING_CAP && p.consumed >= note_len + ring_n {
            return true; // took everything the ring had
        }
    }
    false
}

struct NoteWriter<'a>(&'a mut [u8], usize);
impl core::fmt::Write for NoteWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.0.len() - self.1);
        self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
        self.1 += n;
        Ok(())
    }
}

/// A milestone (an LED stage, the boot being done): look at the card if it is newly up, and save what is logged.
pub fn milestone() {
    if CRASHING.load(Ordering::Relaxed) || sdhci::raw_blocks().is_none() {
        return;
    }
    let _ = with_state(|st| {
        probe(st, false);
        flush(st, false, 8);
        super::serror_checkpoint("logfile-flush");
    });
}

/// The periodic flush (the low priority thread calls this).
pub fn tick() {
    if CRASHING.load(Ordering::Relaxed) {
        return;
    }
    let _ = with_state(|st| {
        probe(st, false);
        flush(st, false, 4);
        super::serror_checkpoint("logfile-flush");
    });
}

/// Start the thread that saves the log every second or so and shows the "log saved" signal.
pub fn start_thread() {
    crate::sched::spawn_prio("milklog", crate::sched::Priority::Low, || {
        let mut showing = false;
        loop {
            tick();
            if saved_quiet() && led::saved_signal_allowed() {
                showing = true;
                // Two short blinks, then about a second of dark, over and over.
                led::saved_signal(true);
                crate::sched::sleep_ms(70);
                led::saved_signal(false);
                crate::sched::sleep_ms(160);
                led::saved_signal(true);
                crate::sched::sleep_ms(70);
                led::saved_signal(false);
                crate::sched::sleep_ms(1100);
            } else {
                if showing {
                    // More text came: back to solid on.
                    showing = false;
                    led::saved_signal(true);
                }
                crate::sched::sleep_ms(400);
            }
        }
    });
}

/// Is every logged byte on the card, with nothing new for a moment? (The "safe to power off" state.)
pub fn saved_quiet() -> bool {
    if !ACTIVE.load(Ordering::Acquire) || FAILS.load(Ordering::Relaxed) != 0 {
        return false;
    }
    match crate::klog::total() {
        Some(t) if t == SAVED_POS.load(Ordering::Acquire) => {
            let last = LAST_WRITE_US.load(Ordering::Relaxed);
            last != 0 && mailbox::now_us().wrapping_sub(last) >= QUIET_MS * 1000
        }
        _ => false,
    }
}

/// The kernel is dying (a panic or an exception handler, no allocator, interrupts off): save everything
/// logged, the crash dump included, within about a second. Never blocks for long: a flush in progress on
/// another CPU gets half a second, then its state is taken over and the whole log (what the ring still has) is written
/// again from the start of the file.
pub fn crash_flush() {
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
        // The state may be torn: start the file again from the ring.
        st.image = LogImage::new(st.image.boot_id());
        st.ring_pos = 0;
        st.header_done = false;
    }
    probe(st, true);
    // The crash dump was just logged: a few rounds are plenty.
    flush(st, true, 16);
    if !stolen {
        BUSY.store(false, Ordering::Release);
    }
}
