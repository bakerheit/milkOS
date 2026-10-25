//! The intake of the SD card log (docs/raspberry-pi.md, "Reading the log"): everything that should end up in the
//! log file is put into one RAM ring here, in order, as complete lines, and the low priority `milklog` thread
//! (rpi/logfile.rs) drains it to the card.
//!
//! **Sources** (each a [`rpikit::logintake::Assembler`] for the pieces they write, or whole lines):
//!
//! * the console: every byte that `kprintln!` or a program writing to `/dev/console` sends to the serial port and the
//!   screens (`sink::Sink`), which includes the drivers' diagnostics, the LED stage lines, the display and probe
//!   lines, panics and exception dumps, and the Browser's `[browser]` lines;
//! * the Terminal windows (`wm/tty.rs`, tty1..9) and pseudo-terminals (`pty.rs`): the shell and whatever runs in them;
//! * the journal's lines from user space (`journal::add`, which is where milkd sends the services' output) as
//!   `[journal unit/pid] text`, and the audit log (`audit::record_as`) as `[audit] ...`;
//! * the system report (rpi/logreport.rs), which is written as whole lines.
//!
//! Every line gets an uptime stamp `[  123.456] ` and (except the console's) a tag. **The cap**
//! (rpikit::logintake::Gate): at most 25 lines and 4 KiB a second on average, with room for a burst of 3000 lines /
//! 256 KiB (a whole boot); lines over it are dropped and counted, and the next line that gets through is preceded by
//! `[milklog: N lines dropped ...]`. A crash lifts the cap ([`emergency`]). If the ring is full because the card
//! writer is behind (or there is no card yet), new lines are dropped and counted rather than the oldest, so the start
//! of the boot survives; the count is written into the log as soon as there is room.
//!
//! The ring is behind a short spin lock (IRQs masked while it is held, a line is copied in a few microseconds); a
//! writer that cannot get it within a few thousand spins drops its text (counted) instead of waiting. Nothing here
//! allocates or prints, so it works from interrupt handlers and from a crash.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rpikit::logintake::{self, Assembler, Gate, LINE_MAX, Verdict};

use crate::sync::SpinLock;

/// Bytes of log text waiting for the card.
pub const RING: usize = 288 * 1024;
/// Source 0 is the console, 1..=9 the Terminal tabs, 10..=17 the pseudo-terminals.
const SOURCES: usize = 18;
/// Bytes kept of the hardware discovery lines for the system report.
const FACTS: usize = 6 * 1024;
/// A partial line (a shell prompt) older than this is written as a line of its own.
const STALE_MS: u64 = 1500;

/// Which lines the system report repeats as "hardware" (they are printed once, at boot or when a device appears).
const FACT_PREFIXES: &[&str] = &[
    "[pci]", "[pcie]", "[usb]", "[genet]", "[rng]", "[sdhci]", "rpi:", "mbox: board", "mbox: ARM", "mbox: VideoCore",
    "mbox: clock", "cpu: MIDR", "dtb: model", "dtb: memory", "spi:", "lcd: ",
];

struct Inner {
    buf: [u8; RING],
    /// Bytes ever put in; `buf[i % RING]` holds byte `i` while `tail <= i < head`.
    head: u64,
    /// Bytes taken out by the card writer.
    tail: u64,
    gate: Gate,
    asm: [Assembler; SOURCES],
    /// Time of the last byte of each partial line (ms), for [`flush_stale`].
    touched: [u64; SOURCES],
    /// Lines dropped because the ring was full, not yet written down.
    full_pending: u32,
    full_total: u64,
    lines: u64,
    facts: [u8; FACTS],
    facts_len: usize,
}

static INNER: SpinLock<Inner> = SpinLock::new(Inner {
    buf: [0; RING],
    head: 0,
    tail: 0,
    gate: Gate::new(),
    asm: [const { Assembler::new() }; SOURCES],
    touched: [0; SOURCES],
    full_pending: 0,
    full_total: 0,
    lines: 0,
    facts: [0; FACTS],
    facts_len: 0,
});

// What other contexts may ask without the lock.
static HEAD: AtomicU64 = AtomicU64::new(0);
static TAIL: AtomicU64 = AtomicU64::new(0);
static LOCK_MISSES: AtomicU64 = AtomicU64::new(0);
static EMERGENCY: AtomicBool = AtomicBool::new(false);
/// The card log is not going to happen (no log file, or it gave up): nothing is collected.
static DISABLED: AtomicBool = AtomicBool::new(false);

fn mmu_on() -> bool {
    let sctlr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    sctlr & 1 != 0
}

/// Milliseconds on the generic counter (since the counter started, which is the board's power-on on a Pi).
pub fn uptime_ms() -> u64 {
    let (c, f): (u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) c, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    if f == 0 {
        return 0;
    }
    ((u128::from(c) * 1000) / u128::from(f)) as u64
}

fn with_inner<R>(f: impl FnOnce(&mut Inner) -> R) -> Option<R> {
    if !mmu_on() || DISABLED.load(Ordering::Relaxed) {
        return None;
    }
    for _ in 0..4000 {
        if let Some(mut g) = INNER.try_lock() {
            let r = f(&mut g);
            HEAD.store(g.head, Ordering::Release);
            TAIL.store(g.tail, Ordering::Release);
            return Some(r);
        }
        core::hint::spin_loop();
    }
    LOCK_MISSES.fetch_add(1, Ordering::Relaxed);
    None
}

impl Inner {
    fn free(&self) -> usize {
        RING - (self.head - self.tail) as usize
    }

    fn put(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.buf[(self.head % RING as u64) as usize] = b;
            self.head += 1;
        }
    }

    /// One whole line (`tag` may be empty): stamp, tag, text, newline. `exempt`: not subject to the cap.
    fn line(&mut self, now: u64, tag: &str, text: &[u8], exempt: bool) {
        let text = &text[..text.len().min(LINE_MAX)];
        let cost = 14 + tag.len() + 1 + text.len() + 1;
        if !exempt && !EMERGENCY.load(Ordering::Relaxed) {
            match self.gate.admit(now, cost) {
                Verdict::Drop => return,
                Verdict::Pass => {}
                Verdict::PassAfterNote(n) => {
                    let mut note = [0u8; 120];
                    let len = note_text(&mut note, n);
                    self.raw_line(now, "", &note[..len]);
                }
            }
        }
        if self.full_pending > 0 && self.free() > 2 * (cost + 120) {
            let mut note = [0u8; 120];
            let len = full_text(&mut note, self.full_pending);
            self.full_pending = 0;
            self.raw_line(now, "", &note[..len]);
        }
        self.raw_line(now, tag, text);
    }

    fn raw_line(&mut self, now: u64, tag: &str, text: &[u8]) {
        let mut st = [0u8; 20];
        let sl = logintake::stamp(now, &mut st);
        let need = sl + if tag.is_empty() { 0 } else { tag.len() + 1 } + text.len() + 1;
        if self.free() < need {
            // Full: drop (counted), unless a crash is in progress, which makes room by dropping the oldest.
            if EMERGENCY.load(Ordering::Relaxed) && need < RING / 2 {
                let want = need - self.free();
                self.tail += want as u64;
            } else {
                self.full_pending = self.full_pending.saturating_add(1);
                self.full_total += 1;
                return;
            }
        }
        self.lines += 1;
        self.put(&st[..sl]);
        if !tag.is_empty() {
            self.put(tag.as_bytes());
            self.put(b" ");
        }
        self.put(text);
        self.put(b"\n");
        if FACT_PREFIXES.iter().any(|p| text.starts_with(p.as_bytes())) && tag.is_empty() {
            self.add_fact(text);
        }
    }

    fn add_fact(&mut self, text: &[u8]) {
        let n = text.len().min(200);
        if self.facts_len + n + 1 > FACTS {
            return;
        }
        let at = self.facts_len;
        self.facts[at..at + n].copy_from_slice(&text[..n]);
        self.facts[at + n] = b'\n';
        self.facts_len += n + 1;
    }

    /// Bytes from source `src`, assembled into lines.
    fn feed(&mut self, src: usize, now: u64, tag: &str, bytes: &[u8]) {
        for &b in bytes {
            if self.asm[src].feed(b) {
                let mut line = [0u8; LINE_MAX];
                let n = self.asm[src].line().len();
                line[..n].copy_from_slice(self.asm[src].line());
                self.asm[src].clear();
                self.line(now, tag, &line[..n], false);
            }
        }
        self.touched[src] = now;
    }
}

struct W<'a>(&'a mut [u8], usize);
impl core::fmt::Write for W<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.0.len() - self.1);
        self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
        self.1 += n;
        Ok(())
    }
}

fn note_text(out: &mut [u8], n: u32) -> usize {
    use core::fmt::Write;
    let mut w = W(out, 0);
    let _ = write!(
        w,
        "[milklog: {n} lines dropped by the rate cap ({} lines/s, {} bytes/s on average)]",
        logintake::LINES_PER_SEC,
        logintake::BYTES_PER_SEC
    );
    w.1
}

fn full_text(out: &mut [u8], n: u32) -> usize {
    use core::fmt::Write;
    let mut w = W(out, 0);
    let _ = write!(w, "[milklog: {n} lines lost: the ring waiting for the card was full]");
    w.1
}

fn tag_of(src: usize, out: &mut [u8; 12]) -> usize {
    use core::fmt::Write;
    let mut w = W(out, 0);
    match src {
        0 => {}
        1..=9 => {
            let _ = write!(w, "[tty{src}]");
        }
        _ => {
            let _ = write!(w, "[pty{}]", src - 10);
        }
    }
    w.1
}

// ---- writers --------------------------------------------------------------------------------------------

/// Console text (everything `kprintln!` and `/dev/console` write), called by `sink::Sink` under the print lock.
pub fn console(s: &str) {
    let now = uptime_ms();
    let _ = with_inner(|g| g.feed(0, now, "", s.as_bytes()));
}

/// Output of Terminal tab `n` (1..=9).
pub fn tty(n: usize, b: &[u8]) {
    stream(n.clamp(1, 9), b);
}

/// Output of pseudo-terminal `n` (0..=7).
pub fn pty(n: usize, b: &[u8]) {
    stream(10 + n.min(7), b);
}

fn stream(src: usize, b: &[u8]) {
    // At most 8 KiB of one write is looked at (a program dumping a file is rate capped anyway).
    let b = &b[..b.len().min(8192)];
    let mut tag = [0u8; 12];
    let n = tag_of(src, &mut tag);
    let tag = core::str::from_utf8(&tag[..n]).unwrap_or("");
    let now = uptime_ms();
    let _ = with_inner(|g| g.feed(src, now, tag, b));
}

/// One line from a source that has whole lines (the journal, the audit log): `[tag] text`.
pub fn tagged(tag: &str, text: &str) {
    let now = uptime_ms();
    let bytes = text.as_bytes();
    let mut clean = [0u8; LINE_MAX];
    let mut n = 0;
    for &b in bytes {
        if n == LINE_MAX {
            break;
        }
        match b {
            b'\n' | b'\r' | b'\t' => clean[n] = b' ',
            0..=0x1f => continue,
            _ => clean[n] = b,
        }
        n += 1;
    }
    let _ = with_inner(|g| g.line(now, tag, &clean[..n], false));
}

/// Lines of the system report or the boot markers: text with newlines, written as lines that are not subject to the cap.
pub fn exempt_lines(tag: &str, text: &str) {
    let now = uptime_ms();
    for l in text.split('\n') {
        if l.is_empty() {
            continue;
        }
        let bytes = l.as_bytes();
        let mut clean = [0u8; LINE_MAX];
        let mut n = 0;
        for &b in bytes {
            if n == LINE_MAX {
                break;
            }
            match b {
                b'\t' => clean[n] = b' ',
                0..=0x1f => continue,
                _ => clean[n] = b,
            }
            n += 1;
        }
        // One lock per line: a long report does not hold off the console.
        let _ = with_inner(|g| g.line(now, tag, &clean[..n], true));
    }
}

/// Stop collecting (the card log will not be written this boot) and forget what is waiting.
pub fn disable() {
    DISABLED.store(true, Ordering::Relaxed);
    HEAD.store(0, Ordering::Release);
    TAIL.store(0, Ordering::Release);
}

/// A crash is under way: the cap is lifted for the rest of this boot, and a full ring drops its oldest text.
pub fn emergency() {
    EMERGENCY.store(true, Ordering::Relaxed);
}

/// Write out the partial lines of every source whose last byte is older than a moment (`all`: every one, now): a
/// shell prompt has no newline.
pub fn flush_partials(all: bool) {
    let now = uptime_ms();
    let _ = with_inner(|g| {
        for src in 0..SOURCES {
            if g.asm[src].is_empty() || (!all && now.saturating_sub(g.touched[src]) < STALE_MS) {
                continue;
            }
            let mut line = [0u8; LINE_MAX];
            let n = g.asm[src].line().len();
            line[..n].copy_from_slice(g.asm[src].line());
            g.asm[src].clear();
            let mut tag = [0u8; 12];
            let tn = tag_of(src, &mut tag);
            let t = core::str::from_utf8(&tag[..tn]).unwrap_or("");
            g.line(now, t, &line[..n], false);
        }
    });
}

// ---- the card writer's side -----------------------------------------------------------------------------

/// Bytes waiting to be written to the card.
pub fn pending() -> usize {
    HEAD.load(Ordering::Acquire).saturating_sub(TAIL.load(Ordering::Acquire)) as usize
}

/// Take up to `out.len()` bytes of waiting log text (oldest first). `force` (a crash): read the ring even if a context
/// that died holds its lock.
pub fn take(out: &mut [u8], force: bool) -> Option<usize> {
    let mut f = |g: &mut Inner| {
        let n = ((g.head - g.tail) as usize).min(out.len());
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = g.buf[((g.tail + i as u64) % RING as u64) as usize];
        }
        g.tail += n as u64;
        n
    };
    if let Some(n) = with_inner(&mut f) {
        return Some(n);
    }
    if !force {
        return None;
    }
    // SAFETY: a crash: the lock holder is dead or about to be; the reader only copies bytes and bumps `tail`, and a
    // racing writer can at worst tear the last line.
    let g = unsafe { &mut *(INNER.data_unchecked() as *mut Inner) };
    let n = f(g);
    HEAD.store(g.head, Ordering::Release);
    TAIL.store(g.tail, Ordering::Release);
    Some(n)
}

/// (lines logged, lines dropped by the cap, lines dropped because the ring was full, lock misses)
pub fn counters() -> (u64, u64, u64, u64) {
    let (a, b, c) = with_inner(|g| (g.lines, g.gate.dropped_total(), g.full_total)).unwrap_or((0, 0, 0));
    (a, b, c, LOCK_MISSES.load(Ordering::Relaxed))
}

/// Hand the hardware discovery lines to `f` (for the system report).
pub fn facts(mut f: impl FnMut(&[u8])) {
    let mut copy = [0u8; FACTS];
    let n = with_inner(|g| {
        copy[..g.facts_len].copy_from_slice(&g.facts[..g.facts_len]);
        g.facts_len
    })
    .unwrap_or(0);
    f(&copy[..n]);
}
