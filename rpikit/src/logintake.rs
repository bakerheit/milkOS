//! What goes into the log before it reaches the card (pure: the kernel's `rpi/logbuf.rs` feeds it text and keeps the ring):
//!
//! * [`Assembler`]: turns the pieces a console or terminal writes (`write_str` fragments, whole screens of escape
//!   sequences, backspaces, carriage returns) into plain log lines.
//! * [`Gate`]: the **rate cap**. A runaway loop that prints all day must not starve the system or wear the card out.
//!   Two token buckets, one for lines and one for bytes: a burst of up to [`LINE_BURST`] lines / [`BYTE_BURST`] bytes
//!   (a whole boot fits), then [`LINES_PER_SEC`] lines and [`BYTES_PER_SEC`] bytes a second. Lines over the cap are
//!   dropped and counted; the next line that gets through (at most once a second) is preceded by `[milklog: N lines dropped ...]`.
//! * [`stamp`]: the `[  123.456] ` uptime prefix every line gets.

use core::fmt::Write;

/// Longest log line kept whole; a longer one is cut into several.
pub const LINE_MAX: usize = 1000;
/// Sustained lines per second the log takes.
pub const LINES_PER_SEC: u32 = 25;
/// Lines that can arrive in one burst (a boot).
pub const LINE_BURST: u32 = 3000;
/// The note about dropped lines is written at most this often (ms).
pub const NOTE_EVERY_MS: u64 = 1000;
/// Sustained bytes per second.
pub const BYTES_PER_SEC: u32 = 4096;
/// Bytes that can arrive in one burst.
pub const BYTE_BURST: u32 = 256 * 1024;

// ---- line assembly ------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    None,
    /// After ESC.
    Seen,
    /// In a CSI sequence (ESC [ ...), until its final byte.
    Csi,
    /// In an OSC string (ESC ] ...), until BEL or ESC \.
    Osc,
    /// ESC inside an OSC string.
    OscEsc,
}

/// One partial line, with escape sequences taken out.
#[derive(Clone)]
pub struct Assembler {
    buf: [u8; LINE_MAX],
    len: usize,
    esc: Mode,
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembler {
    pub const fn new() -> Assembler {
        Assembler {
            buf: [0; LINE_MAX],
            len: 0,
            esc: Mode::None,
        }
    }

    /// The line so far.
    pub fn line(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Add one byte. True if a line is complete (read it with [`Assembler::line`], then [`Assembler::clear`]): a newline
    /// ended it, or the buffer is full.
    pub fn feed(&mut self, b: u8) -> bool {
        match self.esc {
            Mode::Seen => {
                self.esc = match b {
                    b'[' => Mode::Csi,
                    b']' => Mode::Osc,
                    // Two-byte sequences (ESC c, ESC 7, ESC ( B ...): the byte after ESC ends it (ESC ( B takes one more,
                    // which prints as a stray letter at worst).
                    _ => Mode::None,
                };
                return false;
            }
            Mode::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    self.esc = Mode::None;
                }
                return false;
            }
            Mode::Osc => {
                match b {
                    0x07 => self.esc = Mode::None,
                    0x1b => self.esc = Mode::OscEsc,
                    _ => {}
                }
                return false;
            }
            Mode::OscEsc => {
                self.esc = if b == b'\\' { Mode::None } else { Mode::Osc };
                return false;
            }
            Mode::None => {}
        }
        match b {
            0x1b => {
                self.esc = Mode::Seen;
                false
            }
            b'\n' => true,
            b'\r' => false,
            // Backspace takes back the last character (a whole UTF-8 sequence).
            0x08 | 0x7f => {
                while self.len > 0 {
                    self.len -= 1;
                    if self.buf[self.len] & 0xc0 != 0x80 {
                        break;
                    }
                }
                false
            }
            b'\t' => self.push(b' '),
            0..=0x1f => false,
            _ => self.push(b),
        }
    }

    fn push(&mut self, b: u8) -> bool {
        self.buf[self.len] = b;
        self.len += 1;
        self.len == LINE_MAX
    }
}

// ---- the rate cap -------------------------------------------------------------------------------------

/// What the gate says about one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Log it.
    Pass,
    /// Log it, after a note that this many lines were dropped before it.
    PassAfterNote(u32),
    /// Over the cap: drop it.
    Drop,
}

/// Token buckets in thousandths (a line costs 1000, a byte 1000).
#[derive(Clone, Debug)]
pub struct Gate {
    lines: i64,
    bytes: i64,
    last_ms: u64,
    last_note_ms: u64,
    pending: u32,
    dropped_total: u64,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub const fn new() -> Gate {
        Gate {
            lines: LINE_BURST as i64 * 1000,
            bytes: BYTE_BURST as i64 * 1000,
            last_ms: 0,
            last_note_ms: 0,
            pending: 0,
            dropped_total: 0,
        }
    }

    /// Lines dropped by the cap since the start.
    pub fn dropped_total(&self) -> u64 {
        self.dropped_total
    }

    /// Lines dropped since the last note.
    pub fn pending(&self) -> u32 {
        self.pending
    }

    /// A line of `bytes` bytes arrives at time `now_ms` (any monotonic millisecond clock).
    pub fn admit(&mut self, now_ms: u64, bytes: usize) -> Verdict {
        let dt = now_ms.saturating_sub(self.last_ms).min(3_600_000) as i64;
        self.last_ms = self.last_ms.max(now_ms);
        self.lines = (self.lines + dt * i64::from(LINES_PER_SEC)).min(i64::from(LINE_BURST) * 1000);
        self.bytes = (self.bytes + dt * i64::from(BYTES_PER_SEC)).min(i64::from(BYTE_BURST) * 1000);
        if self.lines < 1000 || self.bytes <= 0 {
            self.pending = self.pending.saturating_add(1);
            self.dropped_total += 1;
            return Verdict::Drop;
        }
        self.lines -= 1000;
        self.bytes -= bytes as i64 * 1000;
        // A note about dropped lines at most once a second, so a storm does not double its own lines with notes.
        if self.pending > 0 && now_ms.saturating_sub(self.last_note_ms) >= NOTE_EVERY_MS {
            let n = self.pending;
            self.pending = 0;
            self.last_note_ms = now_ms;
            Verdict::PassAfterNote(n)
        } else {
            Verdict::Pass
        }
    }
}

// ---- the uptime stamp ---------------------------------------------------------------------------------

/// Write `[  123.456] ` (uptime in seconds, at least 5 digits before the point) for `ms` into `out`; returns the length.
pub fn stamp(ms: u64, out: &mut [u8]) -> usize {
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
    let _ = write!(w, "[{:5}.{:03}] ", ms / 1000, ms % 1000);
    w.1
}

/// The uptime in a stamp at the start of `line` (`[  123.456] text`), in milliseconds.
pub fn parse_stamp(line: &str) -> Option<u64> {
    let rest = line.strip_prefix('[')?;
    let (t, _) = rest.split_once("] ")?;
    let (s, ms) = t.trim_start().split_once('.')?;
    if ms.len() != 3 {
        return None;
    }
    Some(s.parse::<u64>().ok()?.checked_mul(1000)? + ms.parse::<u64>().ok()?)
}
