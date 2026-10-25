//! What the log looks like inside `MILKLOG.TXT` on the SD card (format 2), and how it grows (pure: the kernel's
//! `rpi/logfile.rs` supplies the text and writes the sectors this builds, `scripts/pi_readlog.py` reads the same
//! layout back on the Mac).
//!
//! The file is a **circular log of sectors** that holds the log of many boots, one after the other:
//!
//! ```text
//! file sector 0, 1     the header, two copies (written alternately, so a torn write leaves the other intact)
//! file sector 2 ..     the ring: R sectors, each = 500 bytes of text + a 12-byte trailer
//! ```
//!
//! * **Text position.** The log text of all boots is one byte stream; `pos` is the number of bytes ever written.
//!   Byte `p` lives in absolute sector `p / 500` (at offset `p % 500`), which is ring slot `(p / 500) % R`,
//!   file sector `2 + slot`. Nothing is erased: a new boot appends after the previous boot's text, until the ring
//!   wraps and the oldest sectors are overwritten.
//! * **The trailer** of every ring sector: `abs` (u32, the absolute sector number), `len` (u16, how many of
//!   the 500 text bytes are real: 500 except in the last sector), `boot` (u16, the number of the boot that wrote it)
//!   and a CRC-32 of everything before it in the sector. So any sector says whether it is real, which lap of the ring
//!   it belongs to and how far it was filled, whatever the header says. A sector left over from the previous lap has
//!   the wrong `abs`; a half-written one has the wrong CRC; a sector from an earlier boot that sits behind newer text
//!   (a "ghost" left by a torn write) has a boot number that goes backwards.
//! * **The header** is a hint and an index, never the truth: magic, version, a sequence number, the geometry, the
//!   boot counter, the write cursor at the time it was written, and the last [`MAX_BOOTS`] boots (number, state,
//!   start position). It is written at the start of a boot, every [`CHECKPOINT_BYTES`] of text, about once a minute,
//!   and when a boot ends (clean shutdown, restart, crash). [`recover`] starts at its cursor and scans forward
//!   over the sectors to find where the text really ends; if the header is torn or hostile it scans the whole ring.
//! * **Boot sections.** The first text of every boot is a banner line `=== milkOS boot N ===` (build, git hash, ...);
//!   a boot that ends cleanly writes `=== milkOS boot N ended: shutdown ===` (or `restart` / `crash`). A section
//!   with neither an end line nor a crash dump ended in a power cut (or is the boot still running).
//!
//! A power cut in the middle of a flush can only lose the text of that flush: a sector is written whole, the ones
//! beyond a damaged one are ignored by the scan and overwritten by the next write at that place.
//! Everything here is bounded and none of it panics on any input.

use crate::fat::SECTOR;

/// Text bytes in a ring sector.
pub const PAYLOAD: usize = 500;
/// Bytes after them: abs, len, boot, crc.
pub const TRAILER: usize = SECTOR - PAYLOAD;
/// Header sectors at the start of the file (two copies).
pub const HEADER_SECTORS: u32 = 2;
/// The ring must have at least this many sectors.
pub const MIN_RING: u32 = 16;
/// Boots the header keeps.
pub const MAX_BOOTS: usize = 28;
/// Sectors one [`Log::prepare`] fills at most.
pub const OUT_SECTORS: usize = 16;
/// The header is rewritten after this much new text at the latest (less in a small ring: [`Log::checkpoint_bytes`]).
pub const CHECKPOINT_BYTES: u64 = 64 * 1024;
/// The header's magic.
pub const MAGIC: &[u8; 8] = b"MILKLOG2";
/// Format version.
pub const VERSION: u32 = 2;
/// What the first line of a boot starts with.
pub const BANNER_PREFIX: &str = "=== milkOS boot ";
/// What the line that ends a boot starts with (then the boot number, ` ended: `, the kind, ` ===`).
pub const END_PREFIX: &str = "=== milkOS boot ";
/// The text of the old (format 1) file's first line.
pub const V1_MAGIC: &[u8] = b"MILKLOG 1 boot=";
/// The first line of a blank file made by `scripts/pi-logfile.sh`.
pub const BLANK_HEAD: &[u8] = b"MILKLOG.TXT:";
/// Most boots whose number fits the sector trailer.
pub const MAX_BOOT: u32 = 0xfff0;

/// How a boot ended, as recorded in the header.
pub const STATE_RUNNING: u32 = 0;
pub const STATE_SHUTDOWN: u32 = 1;
pub const STATE_RESTART: u32 = 2;
pub const STATE_CRASH: u32 = 3;

// ---- CRC-32 -------------------------------------------------------------------------------------------

const fn make_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = make_table();

/// The IEEE CRC-32 (the one zlib and Python's `zlib.crc32` compute).
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c = TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
fn get32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn get16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn get64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

// ---- ring sectors -------------------------------------------------------------------------------------

/// What a valid ring sector says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectorInfo {
    pub abs: u32,
    pub len: u16,
    pub boot: u16,
}

/// Fill in the trailer of `sec` (512 bytes; the first 500 are the text, zero padded after `len`).
pub fn seal_sector(sec: &mut [u8], abs: u32, len: usize, boot: u16) {
    debug_assert!(sec.len() == SECTOR && len <= PAYLOAD);
    put32(sec, PAYLOAD, abs);
    put16(sec, PAYLOAD + 4, len as u16);
    put16(sec, PAYLOAD + 6, boot);
    let c = crc32(&sec[..PAYLOAD + 8]);
    put32(sec, PAYLOAD + 8, c);
}

/// The trailer of `sec`, if the sector is a real ring sector (CRC right, length sane).
pub fn open_sector(sec: &[u8]) -> Option<SectorInfo> {
    if sec.len() != SECTOR {
        return None;
    }
    let len = get16(sec, PAYLOAD + 4);
    if usize::from(len) > PAYLOAD || crc32(&sec[..PAYLOAD + 8]) != get32(sec, PAYLOAD + 8) {
        return None;
    }
    Some(SectorInfo {
        abs: get32(sec, PAYLOAD),
        len,
        boot: get16(sec, PAYLOAD + 6),
    })
}

// ---- the header ---------------------------------------------------------------------------------------

/// One boot in the header's index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootEntry {
    pub boot: u32,
    pub state: u32,
    /// Text position of the boot's first byte.
    pub start: u64,
}

/// The header (either copy).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub seq: u32,
    pub file_sectors: u32,
    pub ring_sectors: u32,
    pub boot_count: u32,
    pub cursor: u64,
    n: usize,
    boots: [BootEntry; MAX_BOOTS],
}

const H_VERSION: usize = 8;
const H_SEQ: usize = 12;
const H_FILE: usize = 16;
const H_RING: usize = 20;
const H_PAYLOAD: usize = 24;
const H_BOOTS: usize = 28;
const H_CURSOR: usize = 32;
const H_N: usize = 40;
const H_TABLE: usize = 48;
const H_CRC: usize = SECTOR - 4;

impl Header {
    pub fn new(file_sectors: u32, ring_sectors: u32) -> Header {
        Header {
            seq: 0,
            file_sectors,
            ring_sectors,
            boot_count: 0,
            cursor: 0,
            n: 0,
            boots: [BootEntry::default(); MAX_BOOTS],
        }
    }

    /// The boots the header remembers, oldest first.
    pub fn boots(&self) -> &[BootEntry] {
        &self.boots[..self.n]
    }

    pub fn encode(&self, out: &mut [u8]) {
        out[..SECTOR].fill(0);
        out[..8].copy_from_slice(MAGIC);
        put32(out, H_VERSION, VERSION);
        put32(out, H_SEQ, self.seq);
        put32(out, H_FILE, self.file_sectors);
        put32(out, H_RING, self.ring_sectors);
        put32(out, H_PAYLOAD, PAYLOAD as u32);
        put32(out, H_BOOTS, self.boot_count);
        put64(out, H_CURSOR, self.cursor);
        put32(out, H_N, self.n as u32);
        for (i, b) in self.boots().iter().enumerate() {
            let o = H_TABLE + 16 * i;
            put32(out, o, b.boot);
            put32(out, o + 4, b.state);
            put64(out, o + 8, b.start);
        }
        let c = crc32(&out[..H_CRC]);
        put32(out, H_CRC, c);
    }

    /// A header from sector `sec`, if it is one (magic, version, CRC and the numbers all right).
    pub fn decode(sec: &[u8]) -> Option<Header> {
        if sec.len() != SECTOR || &sec[..8] != MAGIC || get32(sec, H_VERSION) != VERSION {
            return None;
        }
        if crc32(&sec[..H_CRC]) != get32(sec, H_CRC) || get32(sec, H_PAYLOAD) != PAYLOAD as u32 {
            return None;
        }
        let n = get32(sec, H_N) as usize;
        let ring = get32(sec, H_RING);
        let file = get32(sec, H_FILE);
        if n > MAX_BOOTS || ring < MIN_RING || file != ring.checked_add(HEADER_SECTORS)? {
            return None;
        }
        let mut h = Header::new(file, ring);
        h.seq = get32(sec, H_SEQ);
        h.boot_count = get32(sec, H_BOOTS);
        h.cursor = get64(sec, H_CURSOR);
        h.n = n;
        for (i, slot) in h.boots.iter_mut().enumerate().take(n) {
            let o = H_TABLE + 16 * i;
            *slot = BootEntry {
                boot: get32(sec, o),
                state: get32(sec, o + 4),
                start: get64(sec, o + 8),
            };
        }
        Some(h)
    }
}

/// Is `a` a later header sequence number than `b`?
pub fn seq_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

/// The better of the two header copies (the one with the later sequence number), and which copy it is.
pub fn best_header(s0: &[u8], s1: &[u8]) -> Option<(Header, u32)> {
    match (Header::decode(s0), Header::decode(s1)) {
        (Some(a), Some(b)) => Some(if seq_newer(b.seq, a.seq) { (b, 1) } else { (a, 0) }),
        (Some(a), None) => Some((a, 0)),
        (None, Some(b)) => Some((b, 1)),
        (None, None) => None,
    }
}

// ---- blank and foreign files --------------------------------------------------------------------------

/// What the first two sectors of a file that has no header look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    /// Spaces, zeros and (in the first line) the blank-file text: nothing worth keeping.
    Blank,
    /// A format 1 log (`MILKLOG 1 boot=`): somebody's earlier log, not touched.
    V1,
    /// Anything else.
    Other,
}

/// Classify sectors 0 and 1 of a file without a valid header.
pub fn classify(s0: &[u8], s1: &[u8]) -> Content {
    if s0.starts_with(V1_MAGIC) {
        return Content::V1;
    }
    let plain = |b: &u8| matches!(*b, b' ' | 0 | b'\n' | b'\r');
    let body = if s0.starts_with(BLANK_HEAD) {
        let nl = s0.iter().position(|&b| b == b'\n').unwrap_or(s0.len());
        &s0[nl..]
    } else {
        s0
    };
    if body.iter().all(plain) && s1.iter().all(plain) {
        Content::Blank
    } else {
        Content::Other
    }
}

// ---- the writer ---------------------------------------------------------------------------------------

/// What a flush would write; give it to [`Log::commit`] once the sectors are on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// Absolute sector number of the first sector built (the others follow).
    pub first_abs: u32,
    /// How many sectors, from the start of the `out` buffer.
    pub count: u32,
    /// Bytes of the offered text that are taken.
    pub consumed: usize,
    pos: u64,
    tail: [u8; PAYLOAD],
}

/// Where the text of earlier boots ends and ours goes on (what [`recover`] found).
#[derive(Clone, Debug)]
pub struct Recovered {
    pub pos: u64,
    pub tail: [u8; PAYLOAD],
    pub boot_count: u32,
    pub header: Option<Header>,
    pub how: How,
    /// Sector reads it took.
    pub reads: u32,
}

/// How [`recover`] found the end of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// A blank file: a fresh log.
    Fresh,
    /// From the header's cursor, scanning forward.
    Header,
    /// The header was missing or wrong: the whole ring was scanned.
    Scan,
}

/// The log being written (the kernel keeps one).
#[derive(Clone, Debug)]
pub struct Log {
    ring: u32,
    pos: u64,
    tail: [u8; PAYLOAD],
    hdr: Header,
    hdr_pos: u64,
    hdr_writes: u32,
    boot: u32,
    flushes: u32,
}

impl Log {
    /// A log in a file of `file_sectors` sectors, with no text yet. None if the file is too small.
    pub fn fresh(file_sectors: u32) -> Option<Log> {
        let ring = file_sectors.checked_sub(HEADER_SECTORS).filter(|&r| r >= MIN_RING)?;
        Some(Log {
            ring,
            pos: 0,
            tail: [0; PAYLOAD],
            hdr: Header::new(file_sectors, ring),
            hdr_pos: 0,
            hdr_writes: 0,
            boot: 0,
            flushes: 0,
        })
    }

    /// Carry on after the text [`recover`] found.
    pub fn resume(file_sectors: u32, r: &Recovered) -> Option<Log> {
        let mut l = Log::fresh(file_sectors)?;
        l.pos = r.pos;
        l.tail = r.tail;
        l.hdr_pos = r.pos;
        l.hdr.boot_count = r.boot_count;
        l.boot = r.boot_count;
        if let Some(h) = &r.header {
            l.hdr.seq = h.seq;
            l.hdr.n = h.n;
            l.hdr.boots = h.boots;
        }
        Some(l)
    }

    /// Ring sectors.
    pub fn ring_sectors(&self) -> u32 {
        self.ring
    }

    /// Text bytes the ring holds.
    pub fn ring_bytes(&self) -> u64 {
        u64::from(self.ring) * PAYLOAD as u64
    }

    /// Text bytes ever written (all boots).
    pub fn pos(&self) -> u64 {
        self.pos
    }

    /// Times the ring has gone round (0: still in the first lap).
    pub fn lap(&self) -> u64 {
        self.pos / self.ring_bytes()
    }

    /// The current boot's number (0 before [`Log::begin_boot`]).
    pub fn boot(&self) -> u32 {
        self.boot
    }

    pub fn flushes(&self) -> u32 {
        self.flushes
    }

    pub fn header_writes(&self) -> u32 {
        self.hdr_writes
    }

    pub fn header(&self) -> &Header {
        &self.hdr
    }

    /// Start a new boot: the next number, recorded in the header's index with the current text position.
    /// None if the counter ran out (65 000 boots on one file).
    pub fn begin_boot(&mut self) -> Option<u32> {
        let b = self.hdr.boot_count.checked_add(1).filter(|&b| b <= MAX_BOOT)?;
        self.hdr.boot_count = b;
        self.boot = b;
        if self.hdr.n == MAX_BOOTS {
            self.hdr.boots.copy_within(1.., 0);
            self.hdr.n -= 1;
        }
        self.hdr.boots[self.hdr.n] = BootEntry {
            boot: b,
            state: STATE_RUNNING,
            start: self.pos,
        };
        self.hdr.n += 1;
        Some(b)
    }

    /// Note how this boot ended (the next header carries it).
    pub fn end_boot(&mut self, state: u32) {
        if self.hdr.n > 0 && self.hdr.boots[self.hdr.n - 1].boot == self.boot {
            self.hdr.boots[self.hdr.n - 1].state = state;
        }
    }

    /// File sector of the ring sector with absolute number `abs`.
    pub fn file_sector(&self, abs: u32) -> u32 {
        HEADER_SECTORS + abs % self.ring
    }

    /// Build the sectors that append `text` in `out` (a whole number of sectors; at most [`OUT_SECTORS`] and half the
    /// ring are used). Takes as much of `text` as fits; `count` 0 if there is nothing to write. None if the ring
    /// cannot take more (the absolute sector number would not fit the trailer) or the buffer is too small.
    pub fn prepare(&self, text: &[u8], out: &mut [u8]) -> Option<Prepared> {
        let max = (out.len() / SECTOR).min(OUT_SECTORS).min((self.ring / 2) as usize);
        if max == 0 {
            return None;
        }
        let first = self.pos / PAYLOAD as u64;
        if first + max as u64 >= u64::from(u32::MAX) {
            return None;
        }
        let off = (self.pos % PAYLOAD as u64) as usize;
        let n = text.len().min(max * PAYLOAD - off);
        if n == 0 {
            return Some(Prepared {
                first_abs: first as u32,
                count: 0,
                consumed: 0,
                pos: self.pos,
                tail: self.tail,
            });
        }
        let total = off + n;
        let count = total.div_ceil(PAYLOAD);
        let boot = self.boot as u16;
        let mut tail = [0u8; PAYLOAD];
        for i in 0..count {
            let sec = &mut out[i * SECTOR..(i + 1) * SECTOR];
            sec.fill(0);
            let a = i * PAYLOAD; // start in the combined stream (old tail + text)
            let b = (a + PAYLOAD).min(total);
            for (k, dst) in sec[..b - a].iter_mut().enumerate() {
                let s = a + k;
                *dst = if s < off { self.tail[s] } else { text[s - off] };
            }
            seal_sector(sec, (first + i as u64) as u32, b - a, boot);
            if i == count - 1 && !total.is_multiple_of(PAYLOAD) {
                tail[..b - a].copy_from_slice(&sec[..b - a]);
            }
        }
        Some(Prepared {
            first_abs: first as u32,
            count: count as u32,
            consumed: n,
            pos: self.pos + n as u64,
            tail,
        })
    }

    /// The sectors of `p` are on the card.
    pub fn commit(&mut self, p: &Prepared) {
        self.pos = p.pos;
        self.tail = p.tail;
        if p.count > 0 {
            self.flushes = self.flushes.wrapping_add(1);
        }
    }

    /// Bytes of new text after which the header is due: [`CHECKPOINT_BYTES`], or a quarter of a small ring (the header
    /// must never fall a whole lap behind, or the scan has nothing to start from).
    pub fn checkpoint_bytes(&self) -> u64 {
        CHECKPOINT_BYTES.min(self.ring_bytes() / 4)
    }

    /// Is a header write due on text grounds (a checkpoint's worth of new text)?
    pub fn header_due(&self) -> bool {
        self.pos.saturating_sub(self.hdr_pos) >= self.checkpoint_bytes()
    }

    /// Has any text been written since the header was?
    pub fn header_stale(&self) -> bool {
        self.pos != self.hdr_pos
    }

    /// Build the next header copy into `out` (512 bytes) and say which header sector (0 or 1) it goes to.
    /// Call [`Log::header_written`] once it is on the card.
    pub fn make_header(&mut self, out: &mut [u8]) -> u32 {
        let mut h = self.hdr.clone();
        h.seq = self.hdr.seq.wrapping_add(1);
        h.cursor = self.pos;
        h.encode(out);
        h.seq & 1
    }

    /// The header built by [`Log::make_header`] is on the card.
    pub fn header_written(&mut self, out: &[u8]) {
        if let Some(h) = Header::decode(out) {
            self.hdr.seq = h.seq;
            self.hdr.cursor = h.cursor;
            self.hdr_pos = h.cursor;
            self.hdr_writes = self.hdr_writes.wrapping_add(1);
        }
    }
}

// ---- finding the end of the text ----------------------------------------------------------------------

/// Where sectors come from: the log file, by file sector number.
pub trait Source {
    /// Read `out.len() / 512` sectors starting at file sector `sector`. False if they cannot be read.
    fn read(&mut self, sector: u32, out: &mut [u8]) -> bool;
}

/// Why a file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refuse {
    /// A sector could not be read.
    Read(u32),
    /// The file is too small to hold a ring.
    TooSmall,
    /// The file holds a format 1 log.
    V1,
    /// The file holds something else (not blank, not ours).
    Foreign,
    /// A format 2 file of another size, with nothing in it we can use.
    Geometry,
    /// More boots than the counter holds.
    BootCounter,
}

impl Refuse {
    /// A phrase for the log.
    pub fn what(&self) -> &'static str {
        match self {
            Refuse::Read(_) => "a sector of the log file could not be read",
            Refuse::TooSmall => "the log file is too small for a ring (under 17 KiB)",
            Refuse::V1 => "the file holds an old format 1 boot log: copy it off, then use a fresh blank MILKLOG.TXT (scripts/pi-logfile.sh)",
            Refuse::Foreign => "the file holds something that is not a milkOS log or a blank log file: use a fresh blank MILKLOG.TXT (scripts/pi-logfile.sh)",
            Refuse::Geometry => "the file is a milkOS log made for a file of another size: use a fresh blank MILKLOG.TXT (scripts/pi-logfile.sh)",
            Refuse::BootCounter => "the log has seen 65000 boots: use a fresh blank MILKLOG.TXT",
        }
    }
}

/// Sectors of the scratch buffer [`recover`] wants at least.
pub const SCRATCH_SECTORS: usize = 32;

struct Walk<'a, S: Source> {
    src: &'a mut S,
    ring: u32,
    scratch: &'a mut [u8],
    reads: u32,
}

impl<S: Source> Walk<'_, S> {
    /// Read `n` (at most the scratch size) ring slots from `slot` on, wrapping at the end of the ring.
    fn fetch(&mut self, slot: u32, n: u32) -> Result<(), Refuse> {
        let first = n.min(self.ring - slot);
        let a = first as usize * SECTOR;
        self.reads += 1;
        if !self.src.read(HEADER_SECTORS + slot, &mut self.scratch[..a]) {
            return Err(Refuse::Read(HEADER_SECTORS + slot));
        }
        if first < n {
            let b = n as usize * SECTOR;
            self.reads += 1;
            if !self.src.read(HEADER_SECTORS, &mut self.scratch[a..b]) {
                return Err(Refuse::Read(HEADER_SECTORS));
            }
        }
        Ok(())
    }

    fn sector(&self, i: usize) -> &[u8] {
        &self.scratch[i * SECTOR..(i + 1) * SECTOR]
    }
}

/// The result of following the text forward from a sector.
struct Forward {
    /// Absolute sector the text ends in (or would continue in), and the bytes of it that are real.
    abs: u32,
    len: usize,
    boot: u16,
    tail: [u8; PAYLOAD],
    /// Whether the sector at the start was valid.
    started: bool,
}

fn boot_not_before(a: u16, b: u16) -> bool {
    // a comes after b: its boot number must not be smaller (16-bit wrap aware).
    a.wrapping_sub(b) < 0x8000
}

/// Follow valid sectors forward from absolute sector `abs`, as long as each is the expected one with a boot number
/// that does not go back (the first one may be from any boot), to the first sector that is not (or a partial one).
/// At most one lap.
fn forward<S: Source>(w: &mut Walk<'_, S>, abs: u32) -> Result<Forward, Refuse> {
    let batch = (w.scratch.len() / SECTOR).min(SCRATCH_SECTORS) as u32;
    let mut f = Forward {
        abs,
        len: 0,
        boot: 0,
        tail: [0; PAYLOAD],
        started: false,
    };
    let mut done = 0u32;
    while done < w.ring {
        let cur = abs.wrapping_add(done);
        let n = batch.min(w.ring - done);
        w.fetch(cur % w.ring, n)?;
        for i in 0..n as usize {
            let expect = abs.wrapping_add(done + i as u32);
            let ok = open_sector(w.sector(i))
                .filter(|s| s.abs == expect && (!f.started || boot_not_before(s.boot, f.boot)));
            let Some(s) = ok else {
                return Ok(f);
            };
            f.started = true;
            f.abs = expect;
            f.len = usize::from(s.len);
            f.boot = s.boot;
            f.tail = [0; PAYLOAD];
            f.tail[..f.len].copy_from_slice(&w.sector(i)[..f.len]);
            if f.len < PAYLOAD {
                return Ok(f);
            }
            // A full sector: the text goes on in the next one.
            f.abs = expect.wrapping_add(1);
            f.len = 0;
            f.tail = [0; PAYLOAD];
        }
        done += n;
    }
    Ok(f)
}

/// Find where the text of earlier boots ends, in a file of `file_sectors` sectors.
///
/// `scratch` is a buffer of at least [`SCRATCH_SECTORS`] sectors. Never trusts the header: it starts at its cursor and
/// walks forward over sectors that check out; if the sector at the cursor is not what the header says, the whole
/// ring is scanned for the newest sector.
pub fn recover<S: Source>(src: &mut S, file_sectors: u32, scratch: &mut [u8]) -> Result<Recovered, Refuse> {
    let ring = file_sectors
        .checked_sub(HEADER_SECTORS)
        .filter(|&r| r >= MIN_RING)
        .ok_or(Refuse::TooSmall)?;
    if scratch.len() < SCRATCH_SECTORS * SECTOR {
        return Err(Refuse::TooSmall);
    }
    let mut w = Walk {
        src,
        ring,
        scratch,
        reads: 0,
    };
    // The two header sectors.
    w.reads += 1;
    let mut hdr = [0u8; 2 * SECTOR];
    if !w.src.read(0, &mut hdr) {
        return Err(Refuse::Read(0));
    }
    let best = best_header(&hdr[..SECTOR], &hdr[SECTOR..]).map(|(h, _)| h);
    let valid = best
        .clone()
        .filter(|h| h.file_sectors == file_sectors && h.ring_sectors == ring);
    if let Some(h) = valid {
        // The cursor's sector, or (cursor on a sector boundary, nothing written there yet) the one before.
        let abs = h.cursor / PAYLOAD as u64;
        if abs < u64::from(u32::MAX) / 2 {
            let abs = abs as u32;
            let f = forward(&mut w, abs)?;
            let want = (h.cursor % PAYLOAD as u64) as usize;
            let good = if f.started {
                // The first sector was valid; the cursor must not be past what it holds.
                f.abs > abs || f.len >= want
            } else {
                // Nothing valid at the cursor: only right if the cursor is exactly at a sector boundary and the
                // sector before it is full (or this is the very start of the log).
                want == 0 && (abs == 0 || previous_is_full(&mut w, abs)?)
            };
            if good {
                let pos = u64::from(f.abs) * PAYLOAD as u64 + f.len as u64;
                let boot = h.boot_count.max(if f.started { u32::from(f.boot) } else { 0 });
                if boot > MAX_BOOT {
                    return Err(Refuse::BootCounter);
                }
                return Ok(Recovered {
                    pos,
                    tail: f.tail,
                    boot_count: boot,
                    header: Some(h),
                    how: How::Header,
                    reads: w.reads,
                });
            }
        }
    }
    // No usable header. A blank file is a fresh log; a file with other content is not ours to overwrite.
    if best.is_none() {
        match classify(&hdr[..SECTOR], &hdr[SECTOR..]) {
            // Blank headers on a ring that is blank at its start too (the writer begins at slot 0, so a log that
            // ever had text has something there): a fresh file. A log whose header sectors were lost is not blank.
            Content::Blank if ring_start_is_blank(&mut w)? => {
                return Ok(Recovered {
                    pos: 0,
                    tail: [0; PAYLOAD],
                    boot_count: 0,
                    header: None,
                    how: How::Fresh,
                    reads: w.reads,
                });
            }
            Content::V1 => return Err(Refuse::V1),
            Content::Blank | Content::Other => {}
        }
    }
    // Look at the whole ring for the newest real sector.
    let newest = scan_newest(&mut w)?;
    match newest {
        Some((abs, info)) => {
            let f = forward(&mut w, abs)?;
            let boot = u32::from(f.boot).max(u32::from(info.boot));
            let pos = u64::from(f.abs) * PAYLOAD as u64 + f.len as u64;
            Ok(Recovered {
                pos,
                tail: f.tail,
                boot_count: boot,
                header: None,
                how: How::Scan,
                reads: w.reads,
            })
        }
        None if best.is_some() => Err(Refuse::Geometry),
        None => Err(Refuse::Foreign),
    }
}

/// Is the first ring sector (where the writer starts) blank: spaces, zeros, newlines?
fn ring_start_is_blank<S: Source>(w: &mut Walk<'_, S>) -> Result<bool, Refuse> {
    w.fetch(0, 1)?;
    Ok(w.sector(0).iter().all(|b| matches!(*b, b' ' | 0 | b'\n' | b'\r')))
}

/// Is the sector before absolute sector `abs` valid and full?
fn previous_is_full<S: Source>(w: &mut Walk<'_, S>, abs: u32) -> Result<bool, Refuse> {
    let prev = abs.wrapping_sub(1);
    w.fetch(prev % w.ring, 1)?;
    Ok(open_sector(w.sector(0)).is_some_and(|s| s.abs == prev && usize::from(s.len) == PAYLOAD))
}

/// The real ring sector with the highest absolute number, read over the whole ring.
fn scan_newest<S: Source>(w: &mut Walk<'_, S>) -> Result<Option<(u32, SectorInfo)>, Refuse> {
    let batch = (w.scratch.len() / SECTOR).min(SCRATCH_SECTORS) as u32;
    let mut best: Option<(u32, SectorInfo)> = None;
    let mut slot = 0u32;
    while slot < w.ring {
        let n = batch.min(w.ring - slot);
        w.fetch(slot, n)?;
        for i in 0..n as usize {
            if let Some(s) = open_sector(w.sector(i))
                && s.abs % w.ring == slot + i as u32
                && best.is_none_or(|(a, _)| s.abs > a)
            {
                best = Some((s.abs, s));
            }
        }
        slot += n;
    }
    Ok(best)
}

// ---- the text of a log, for the readers ---------------------------------------------------------------------

/// The log text that is in the file, oldest first: handed to `f` sector by sector (payload slices), with `gap(n)` called
/// where `n` sectors in a row are missing or damaged. Returns the absolute sector range `(first, end)` that was read
/// (`end` exclusive), or None for a file with no text. This is what `scripts/pi_readlog.py` does too.
pub fn read_text<S: Source>(
    src: &mut S,
    file_sectors: u32,
    scratch: &mut [u8],
    mut text: impl FnMut(&[u8]),
    mut gap: impl FnMut(u32),
) -> Result<Option<(u32, u32)>, Refuse> {
    let rec = recover(src, file_sectors, scratch)?;
    if rec.pos == 0 {
        return Ok(None);
    }
    let ring = file_sectors - HEADER_SECTORS;
    let end = (rec.pos.div_ceil(PAYLOAD as u64)) as u32; // exclusive
    let first = end.saturating_sub(ring);
    let mut missing = 0u32;
    let mut started = false;
    let mut a = first;
    let mut sec = [0u8; SECTOR];
    while a < end {
        let ok = src.read(HEADER_SECTORS + a % ring, &mut sec);
        match open_sector(&sec).filter(|s| ok && s.abs == a) {
            Some(s) => {
                if missing > 0 {
                    gap(missing);
                    missing = 0;
                }
                started = true;
                text(&sec[..usize::from(s.len)]);
            }
            // Damage before the first good sector is just the oldest text being gone.
            None if started || first == 0 => missing += 1,
            None => {}
        }
        a += 1;
    }
    if missing > 0 {
        gap(missing);
    }
    Ok(Some((first, end)))
}

// ---- banners ------------------------------------------------------------------------------------------

/// The boot number in a banner line (`=== milkOS boot 12 === ...`), if `line` is one.
pub fn parse_banner(line: &str) -> Option<u32> {
    let rest = line.strip_prefix(BANNER_PREFIX)?;
    let end = rest.find(' ')?;
    if !rest[end..].starts_with(" ===") {
        return None;
    }
    rest[..end].parse().ok()
}

/// The boot number and kind in an end line (`=== milkOS boot 12 ended: crash ===`).
pub fn parse_end(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix(END_PREFIX)?;
    let (n, rest) = rest.split_once(" ended: ")?;
    let kind = rest.split(" ===").next()?;
    if !rest.contains(" ===") {
        return None;
    }
    Some((n.parse().ok()?, kind))
}
