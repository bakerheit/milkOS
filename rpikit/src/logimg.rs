//! What the boot log looks like inside `MILKLOG.TXT`, and how it grows (pure: the kernel's
//! `rpi/logfile.rs` supplies text and writes the sectors this builds).
//!
//! The file is written from its first byte, in place, never longer than it was made. The log text is followed
//! by one **end marker** line, always the last thing written:
//!
//! ```text
//! <<milklog end boot=0000abcd flush=7 bytes=12345>>        (" full" before the >> when the file ran out)
//! ```
//!
//! and the rest of the last sector is spaces. A later flush starts again where the text ended, which
//! overwrites the old marker, so the file holds exactly one marker for this boot, at the end of the text.
//! `boot` is a number picked per boot (the first line of the file carries it too), so a marker left
//! over from an earlier boot further down the file cannot be mistaken for this boot's. A flush rewrites only
//! the sector the text ended in, the new ones and the one the marker ends in: at most [`OUT_SECTORS`] at a time.
//! A power cut in the middle of a flush can only lose log text (and the marker): nothing else is touched.
//!
//! The Mac side (`scripts/pi-readlog.sh`) reads the same format; [`find_marker`] is the reference.

use crate::fat::SECTOR;

/// Sectors one [`LogImage::prepare`] may fill at most.
pub const OUT_SECTORS: usize = 16;
/// The longest end marker.
pub const MARKER_MAX: usize = 96;
/// What every marker starts with.
pub const MARKER_START: &[u8] = b"\n<<milklog end ";

/// A byte sink with a fixed size: a marker is written with `write!`.
struct Cursor<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for Cursor<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// What a flush would write; give it to [`LogImage::commit`] once the sectors are on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// First file sector to write.
    pub first: u32,
    /// How many sectors, from the start of the `out` buffer.
    pub count: u32,
    /// Bytes of the offered text that are taken (more than were written when the file is full:
    /// the rest is dropped).
    pub consumed: usize,
    pos: u64,
    tail: [u8; SECTOR],
    full: bool,
}

/// How much of the log is in the file.
#[derive(Clone, Debug)]
pub struct LogImage {
    boot_id: u32,
    /// Bytes of text in the file (not counting the marker).
    pos: u64,
    flushes: u32,
    /// The text of the sector the text ends in (the first `pos % 512` bytes mean anything).
    tail: [u8; SECTOR],
    full: bool,
}

impl LogImage {
    pub const fn new(boot_id: u32) -> LogImage {
        LogImage {
            boot_id,
            pos: 0,
            flushes: 0,
            tail: [0; SECTOR],
            full: false,
        }
    }

    pub fn boot_id(&self) -> u32 {
        self.boot_id
    }

    /// Bytes of text in the file so far.
    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn flushes(&self) -> u32 {
        self.flushes
    }

    /// The file ran out of room (text is dropped).
    pub fn is_full(&self) -> bool {
        self.full
    }

    /// Build the sectors that append `text` (maybe none) and the marker, in `out` (whole sectors,
    /// at least two, at most [`OUT_SECTORS`] are used) for a file of `file_sectors` sectors. None if the
    /// buffer or the file is too small to do anything sensible.
    pub fn prepare(&self, text: &[u8], file_sectors: u32, out: &mut [u8]) -> Option<Prepared> {
        let cap = u64::from(file_sectors) * SECTOR as u64;
        let out_len = out.len().min(OUT_SECTORS * SECTOR) / SECTOR * SECTOR;
        if out_len < 2 * SECTOR || cap < (MARKER_MAX + SECTOR) as u64 {
            return None;
        }
        let limit = cap - MARKER_MAX as u64; // the text may end here at the latest
        if self.pos > limit {
            return None;
        }
        let off = (self.pos % SECTOR as u64) as usize;
        let first = (self.pos / SECTOR as u64) as u32;
        let room_out = out_len - off - MARKER_MAX;
        let room_file = (limit - self.pos) as usize;
        let (n, consumed, full) = if text.len() > room_file && room_file <= room_out {
            (room_file, text.len(), true) // the file is full: the rest of the text is dropped
        } else if text.len() > room_out {
            (room_out, room_out, self.full)
        } else {
            (text.len(), text.len(), self.full)
        };
        let new_pos = self.pos + n as u64;
        out[..off].copy_from_slice(&self.tail[..off]);
        out[off..off + n].copy_from_slice(&text[..n]);
        let mut cur = Cursor {
            buf: &mut out[off + n..out_len],
            len: 0,
        };
        use core::fmt::Write;
        // MARKER_MAX is big enough for every number; a failure here would only cut the marker short.
        let _ = writeln!(
            cur,
            "{}boot={:08x} flush={} bytes={}{}>>",
            core::str::from_utf8(MARKER_START).unwrap_or(""),
            self.boot_id,
            self.flushes.wrapping_add(1),
            new_pos,
            if full { " full" } else { "" }
        );
        let total = off + n + cur.len;
        let count = total.div_ceil(SECTOR);
        out[total..count * SECTOR].fill(b' ');
        let mut tail = [0u8; SECTOR];
        let ts = (off + n) / SECTOR;
        let tl = (off + n) % SECTOR;
        tail[..tl].copy_from_slice(&out[ts * SECTOR..ts * SECTOR + tl]);
        Some(Prepared {
            first,
            count: count as u32,
            consumed,
            pos: new_pos,
            tail,
            full,
        })
    }

    /// The sectors of `p` are on the card.
    pub fn commit(&mut self, p: &Prepared) {
        self.pos = p.pos;
        self.tail = p.tail;
        self.full = p.full;
        self.flushes = self.flushes.wrapping_add(1);
    }
}

/// The first end marker in `file`: (offset where the text ends, boot id, flush number, text bytes, full).
/// The text ends at the marker's leading newline: `file[..offset]`.
pub fn find_marker(file: &[u8]) -> Option<(usize, u32, u32, u64, bool)> {
    let at = file
        .windows(MARKER_START.len())
        .position(|w| w == MARKER_START)?;
    let rest = &file[at + MARKER_START.len()..];
    let end = rest.iter().position(|&b| b == b'\n')?;
    let line = core::str::from_utf8(&rest[..end]).ok()?;
    let line = line.strip_suffix(">>")?;
    let mut boot = None;
    let mut flush = None;
    let mut bytes = None;
    let mut full = false;
    for w in line.split(' ') {
        if let Some(v) = w.strip_prefix("boot=") {
            boot = u32::from_str_radix(v, 16).ok();
        } else if let Some(v) = w.strip_prefix("flush=") {
            flush = v.parse().ok();
        } else if let Some(v) = w.strip_prefix("bytes=") {
            bytes = v.parse().ok();
        } else if w == "full" {
            full = true;
        }
    }
    Some((at, boot?, flush?, bytes?, full))
}
