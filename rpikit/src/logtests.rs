//! Host tests for `milklog` (the log file's layout, the writer, finding the end of the text after a power cut,
//! torn and hostile headers and sectors) and `logintake` (line assembly, the rate cap, the uptime stamp).

use crate::fat::SECTOR;
use crate::logintake::{self, Assembler, Gate, Verdict};
use crate::milklog::{
    self, BootEntry, Content, HEADER_SECTORS, Header, Log, MAX_BOOTS, OUT_SECTORS, PAYLOAD, Prepared, Recovered,
    Refuse, SCRATCH_SECTORS, Source, crc32, open_sector, recover, seal_sector,
};
use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

/// A small deterministic generator.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The log file as the card holds it.
#[derive(Clone)]
struct Disk {
    bytes: Vec<u8>,
    reads: u32,
}

impl Disk {
    /// A blank file made by scripts/pi-logfile.sh.
    fn blank(sectors: u32) -> Disk {
        let mut bytes = vec![b' '; sectors as usize * SECTOR];
        let head = b"MILKLOG.TXT: the milkOS log goes here (the kernel overwrites this file in place).\n";
        bytes[..head.len()].copy_from_slice(head);
        Disk { bytes, reads: 0 }
    }
    fn sectors(&self) -> u32 {
        (self.bytes.len() / SECTOR) as u32
    }
    fn put(&mut self, sector: u32, data: &[u8]) {
        let a = sector as usize * SECTOR;
        self.bytes[a..a + data.len()].copy_from_slice(data);
    }
    fn sector(&self, sector: u32) -> &[u8] {
        &self.bytes[sector as usize * SECTOR..(sector as usize + 1) * SECTOR]
    }
}

impl Source for Disk {
    fn read(&mut self, sector: u32, out: &mut [u8]) -> bool {
        self.reads += 1;
        let a = sector as usize * SECTOR;
        if !out.len().is_multiple_of(SECTOR) || a + out.len() > self.bytes.len() {
            return false;
        }
        out.copy_from_slice(&self.bytes[a..a + out.len()]);
        true
    }
}

fn scratch() -> Vec<u8> {
    vec![0u8; SCRATCH_SECTORS * SECTOR]
}

/// Write what `p` built (in `out`) to the disk, the way the kernel does: sector by sector into the ring slots.
fn apply(d: &mut Disk, log: &Log, p: &Prepared, out: &[u8]) {
    for i in 0..p.count {
        let at = log.file_sector(p.first_abs + i);
        d.put(at, &out[i as usize * SECTOR..(i as usize + 1) * SECTOR]);
    }
}

/// What the kernel does for one flush of `text`: rounds until it is all taken, a header when it is due.
fn flush(d: &mut Disk, log: &mut Log, mut text: &[u8]) {
    let mut out = vec![0u8; OUT_SECTORS * SECTOR];
    while !text.is_empty() {
        let p = log.prepare(text, &mut out).expect("prepare");
        assert!(p.count as usize <= OUT_SECTORS);
        assert!(p.consumed > 0);
        apply(d, log, &p, &out);
        log.commit(&p);
        text = &text[p.consumed..];
        if log.header_due() {
            header(d, log);
        }
    }
}

fn header(d: &mut Disk, log: &mut Log) {
    let mut h = [0u8; SECTOR];
    let at = log.make_header(&mut h);
    assert!(at < HEADER_SECTORS);
    d.put(at, &h);
    log.header_written(&h);
}

/// Boot: recover, begin a boot, write the header and the banner. Returns the log.
fn boot(d: &mut Disk) -> Log {
    let mut sc = scratch();
    let mut dd = d.clone();
    let r = recover(&mut dd, d.sectors(), &mut sc).expect("recover");
    let mut log = Log::resume(d.sectors(), &r).expect("resume");
    let n = log.begin_boot().expect("boot number");
    let banner = format!("=== milkOS boot {n} === test build\n");
    flush(d, &mut log, banner.as_bytes());
    header(d, &mut log);
    log
}

/// Everything readable in the file, as one string, and the gaps found.
fn text_of(d: &Disk) -> (String, Vec<u32>) {
    let mut sc = scratch();
    let mut dd = d.clone();
    let mut text = Vec::new();
    let mut gaps = Vec::new();
    let got = milklog::read_text(&mut dd, d.sectors(), &mut sc, |t| text.extend_from_slice(t), |g| gaps.push(g));
    got.expect("read_text");
    (String::from_utf8_lossy(&text).into_owned(), gaps)
}

/// `got` is the last bytes of `all`: the ring's worth (less the unfilled part of the last sector).
fn assert_is_ring_suffix(got: &str, all: &str, ring_bytes: u64, what: &str) {
    assert!(got.len() as u64 <= ring_bytes && got.len() as u64 + PAYLOAD as u64 > ring_bytes, "{what}: {} of {ring_bytes}", got.len());
    assert_eq!(got.as_bytes(), &all.as_bytes()[all.len() - got.len()..], "{what}");
}

fn line(i: usize) -> String {
    format!("[{:5}.{:03}] line {i}: the quick brown fox jumps over the lazy dog\n", i / 7, i % 1000)
}

// ---- sectors and headers ------------------------------------------------------------------------------

#[test]
fn crc32_is_the_standard_one() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn golden_sector_and_header_match_the_python_reader() {
    // The same bytes are built in scripts/pi-log-test.sh with zlib.crc32 (the Mac side's reader).
    let mut sec = [0u8; SECTOR];
    sec[..11].copy_from_slice(b"hello milk\n");
    seal_sector(&mut sec, 7, 11, 3);
    assert_eq!(&sec[500..508], &[7, 0, 0, 0, 11, 0, 3, 0]);
    assert_eq!(u32::from_le_bytes(sec[508..512].try_into().unwrap()), 0x17fb_a60e);
    let mut log = Log::fresh(130).unwrap();
    // Build the same boot table through the writer's own calls is not possible (the writer numbers boots), so go
    // through encode / decode of a header with a table made by begin_boot.
    for _ in 0..3 {
        log.begin_boot().unwrap();
    }
    let mut out = [0u8; SECTOR];
    log.make_header(&mut out);
    let back = Header::decode(&out).unwrap();
    assert_eq!(back.boots().len(), 3);
    assert_eq!(back.boot_count, 3);
    // And the fixed one from the Python test.
    let mut fixed = [0u8; SECTOR];
    fixed[..8].copy_from_slice(b"MILKLOG2");
    for (o, v) in [(8, 2u32), (12, 5), (16, 130), (20, 128), (24, 500), (28, 3)] {
        fixed[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fixed[32..40].copy_from_slice(&12345u64.to_le_bytes());
    fixed[40..44].copy_from_slice(&3u32.to_le_bytes());
    for (i, (b, s, st)) in [(1u32, 1u32, 0u64), (2, 3, 500), (3, 0, 9000)].into_iter().enumerate() {
        fixed[48 + 16 * i..52 + 16 * i].copy_from_slice(&b.to_le_bytes());
        fixed[52 + 16 * i..56 + 16 * i].copy_from_slice(&s.to_le_bytes());
        fixed[56 + 16 * i..64 + 16 * i].copy_from_slice(&st.to_le_bytes());
    }
    let c = crc32(&fixed[..508]);
    assert_eq!(c, 0x91f9_e996);
    fixed[508..].copy_from_slice(&c.to_le_bytes());
    let dec = Header::decode(&fixed).unwrap();
    assert_eq!((dec.seq, dec.file_sectors, dec.ring_sectors, dec.boot_count, dec.cursor), (5, 130, 128, 3, 12345));
    assert_eq!(
        dec.boots(),
        &[
            BootEntry { boot: 1, state: 1, start: 0 },
            BootEntry { boot: 2, state: 3, start: 500 },
            BootEntry { boot: 3, state: 0, start: 9000 },
        ]
    );
    let mut again = [0u8; SECTOR];
    dec.encode(&mut again);
    assert_eq!(again, fixed);
}

#[test]
fn every_single_bit_flip_in_a_sector_or_header_is_caught() {
    let mut sec = [0u8; SECTOR];
    for (i, b) in sec[..PAYLOAD].iter_mut().enumerate() {
        *b = (i * 7) as u8;
    }
    seal_sector(&mut sec, 1234, 500, 9);
    assert_eq!(open_sector(&sec).map(|s| (s.abs, s.len, s.boot)), Some((1234, 500, 9)));
    for bit in 0..SECTOR * 8 {
        let mut s = sec;
        s[bit / 8] ^= 1 << (bit % 8);
        assert!(open_sector(&s).is_none(), "bit {bit}");
    }
    let mut log = Log::fresh(100).unwrap();
    log.begin_boot().unwrap();
    let mut h = [0u8; SECTOR];
    log.make_header(&mut h);
    assert!(Header::decode(&h).is_some());
    for bit in 0..SECTOR * 8 {
        let mut s = h;
        s[bit / 8] ^= 1 << (bit % 8);
        // Bytes past the table that the format does not use may change without a CRC change only if the CRC still
        // matches, which a single flip never does.
        assert!(Header::decode(&s).is_none(), "bit {bit}");
    }
}

#[test]
fn header_numbers_are_checked() {
    let mut log = Log::fresh(100).unwrap();
    let mut h = [0u8; SECTOR];
    log.make_header(&mut h);
    let reseal = |h: &mut [u8; SECTOR]| {
        let c = crc32(&h[..508]);
        h[508..].copy_from_slice(&c.to_le_bytes());
    };
    // A table count over the limit, a ring that is too small, a payload size that is not ours, a version from the future.
    for (off, val) in [(40usize, 29u32), (40, u32::MAX), (20, 4), (24, 400), (8, 3), (16, 99)] {
        let mut x = h;
        x[off..off + 4].copy_from_slice(&val.to_le_bytes());
        reseal(&mut x);
        assert!(Header::decode(&x).is_none(), "off {off} val {val}");
    }
    assert!(Header::decode(&h[..100]).is_none());
    assert!(Header::decode(&[]).is_none());
}

#[test]
fn the_newer_header_copy_wins_even_across_a_sequence_wrap() {
    let mk = |seq: u32| {
        let mut h = Header::new(100, 98);
        h.seq = seq;
        let mut b = [0u8; SECTOR];
        h.encode(&mut b);
        b
    };
    let (a, b) = (mk(5), mk(6));
    assert_eq!(milklog::best_header(&a, &b).unwrap().1, 1);
    assert_eq!(milklog::best_header(&b, &a).unwrap().1, 0);
    let (a, b) = (mk(u32::MAX), mk(0));
    assert_eq!(milklog::best_header(&a, &b).unwrap().1, 1, "0 follows u32::MAX");
    assert_eq!(milklog::best_header(&a, &[0; SECTOR]).unwrap().1, 0);
    assert!(milklog::best_header(&[0; SECTOR], &[0; SECTOR]).is_none());
}

#[test]
fn blank_and_foreign_content_is_told_apart() {
    let d = Disk::blank(64);
    assert_eq!(milklog::classify(d.sector(0), d.sector(1)), Content::Blank);
    assert_eq!(milklog::classify(&[0; SECTOR], &[0; SECTOR]), Content::Blank);
    assert_eq!(milklog::classify(&[b' '; SECTOR], &[b' '; SECTOR]), Content::Blank);
    let mut v1 = [b' '; SECTOR];
    v1[..30].copy_from_slice(b"MILKLOG 1 boot=0000abcd\nmilkOS");
    assert_eq!(milklog::classify(&v1, &[b' '; SECTOR]), Content::V1);
    let mut other = [b' '; SECTOR];
    other[..5].copy_from_slice(b"hello");
    assert_eq!(milklog::classify(&other, &[b' '; SECTOR]), Content::Other);
    let mut blank_then_text = *d.sector(0).first_chunk::<SECTOR>().unwrap();
    blank_then_text[300] = b'x';
    assert_eq!(milklog::classify(&blank_then_text, &[b' '; SECTOR]), Content::Other);
    assert_eq!(milklog::classify(&[b' '; SECTOR], &other), Content::Other);
}

// ---- writing and reading back -------------------------------------------------------------------------

#[test]
fn a_blank_file_becomes_a_log_that_reads_back() {
    let mut d = Disk::blank(64);
    let mut sc = scratch();
    let r = recover(&mut d.clone(), 64, &mut sc).unwrap();
    assert_eq!((r.pos, r.how, r.boot_count), (0, milklog::How::Fresh, 0));
    let mut log = boot(&mut d);
    assert_eq!(log.boot(), 1);
    let mut want = String::from("=== milkOS boot 1 === test build\n");
    for i in 0..200 {
        let l = line(i);
        want.push_str(&l);
        flush(&mut d, &mut log, l.as_bytes());
    }
    assert!(want.len() > 10_000 && (want.len() as u64) < log.ring_bytes(), "fits the ring");
    let (got, gaps) = text_of(&d);
    assert_eq!(got, want);
    assert!(gaps.is_empty());
    assert_eq!(log.pos(), want.len() as u64);
    assert_eq!(log.lap(), 0);
}

#[test]
fn boots_follow_each_other_and_nothing_older_is_erased() {
    let mut d = Disk::blank(512);
    let mut want = String::new();
    let mut n = 0;
    for b in 1..=5u32 {
        let mut log = boot(&mut d);
        assert_eq!(log.boot(), b);
        want.push_str(&format!("=== milkOS boot {b} === test build\n"));
        for _ in 0..40 {
            let l = line(n);
            n += 1;
            want.push_str(&l);
            flush(&mut d, &mut log, l.as_bytes());
        }
        // Boots 2 and 4 end with an end line and a state.
        if b % 2 == 0 {
            let e = format!("=== milkOS boot {b} ended: shutdown ===\n");
            want.push_str(&e);
            flush(&mut d, &mut log, e.as_bytes());
            log.end_boot(milklog::STATE_SHUTDOWN);
            header(&mut d, &mut log);
        }
        // (boots 1, 3, 5: no header after the last flush: a power cut)
    }
    let (got, gaps) = text_of(&d);
    assert_eq!(got, want);
    assert!(gaps.is_empty());
    let h = milklog::best_header(d.sector(0), d.sector(1)).unwrap().0;
    assert_eq!(h.boot_count, 5);
    let boots: Vec<(u32, u32)> = h.boots().iter().map(|b| (b.boot, b.state)).collect();
    assert_eq!(boots, vec![(1, 0), (2, 1), (3, 0), (4, 1), (5, 0)]);
    // Starts are text positions of each boot's banner.
    for b in h.boots() {
        let at = got[b.start as usize..].lines().next().unwrap();
        assert_eq!(milklog::parse_banner(at), Some(b.boot), "{at}");
    }
}

#[test]
fn a_header_that_is_behind_still_finds_the_end() {
    // The header was last written 60 KB ago; the scan walks forward to the real end.
    let mut d = Disk::blank(2048);
    let mut log = boot(&mut d);
    let mut want = String::from("=== milkOS boot 1 === test build\n");
    for i in 0..4000 {
        let l = line(i);
        want.push_str(&l);
        // Write without the kernel's checkpoint: header only at boot.
        let mut out = vec![0u8; OUT_SECTORS * SECTOR];
        let mut rest = l.as_bytes();
        while !rest.is_empty() {
            let p = log.prepare(rest, &mut out).unwrap();
            apply(&mut d, &log, &p, &out);
            log.commit(&p);
            rest = &rest[p.consumed..];
        }
    }
    assert!(log.pos() > 200_000);
    let mut sc = scratch();
    let r = recover(&mut d.clone(), 2048, &mut sc).unwrap();
    assert_eq!(r.how, milklog::How::Header);
    assert_eq!(r.pos, log.pos());
    assert_eq!(r.boot_count, 1);
    let (got, _) = text_of(&d);
    assert_eq!(got, want);
    // A new boot appends right after.
    let mut log2 = boot(&mut d);
    assert_eq!(log2.boot(), 2);
    flush(&mut d, &mut log2, b"second\n");
    let (got2, _) = text_of(&d);
    assert!(got2.starts_with(&want) && got2.ends_with("second\n"));
    assert_eq!(&got2[want.len()..want.len() + 30], "=== milkOS boot 2 === test bui");
}

#[test]
fn the_ring_wraps_and_the_oldest_text_goes() {
    // A 64 KiB file: 126 ring sectors = 63 000 bytes of text.
    let mut d = Disk::blank(128);
    let mut log = boot(&mut d);
    let ring_bytes = log.ring_bytes();
    assert_eq!(ring_bytes, 126 * 500);
    let mut all = String::from("=== milkOS boot 1 === test build\n");
    let mut i = 0;
    while (all.len() as u64) < ring_bytes * 3 + 1234 {
        let l = line(i);
        i += 1;
        all.push_str(&l);
        flush(&mut d, &mut log, l.as_bytes());
        // The cursor stays consistent at every step.
        let mut sc = scratch();
        let r = recover(&mut d.clone(), 128, &mut sc).unwrap();
        assert_eq!(r.pos, log.pos());
    }
    assert_eq!(log.pos(), all.len() as u64);
    assert_eq!(log.lap(), 3);
    let (got, gaps) = text_of(&d);
    assert!(gaps.is_empty());
    // The file holds the last ring's worth of the text.
    assert_is_ring_suffix(&got, &all, ring_bytes, "after three laps");
    assert!(!got.starts_with("=== milkOS boot 1"), "the first boot's banner is gone");
    // And every ring sector that is not the partial last one is full, with the right lap number.
    let mut slots = 0;
    for s in HEADER_SECTORS..128 {
        let info = open_sector(d.sector(s)).expect("every ring sector is real after three laps");
        assert_eq!((info.abs % 126) + HEADER_SECTORS, s);
        assert!(info.abs >= (all.len() as u32 / 500).saturating_sub(126));
        slots += 1;
    }
    assert_eq!(slots, 126);
}

#[test]
fn a_boot_after_a_wrap_carries_on_from_the_cursor() {
    let mut d = Disk::blank(128);
    let mut want = String::new();
    for b in 1..=6u32 {
        let mut log = boot(&mut d);
        assert_eq!(log.boot(), b);
        want.push_str(&format!("=== milkOS boot {b} === test build\n"));
        for i in 0..1200 {
            let l = line(i + b as usize * 10_000);
            want.push_str(&l);
            flush(&mut d, &mut log, l.as_bytes());
        }
    }
    assert!(want.len() as u64 > 4 * 63_000);
    let (got, gaps) = text_of(&d);
    assert!(gaps.is_empty());
    assert_is_ring_suffix(&got, &want, 63_000, "six boots in a small ring");
}

#[test]
fn a_big_chunk_goes_in_rounds_and_is_never_written_outside_the_file() {
    let mut d = Disk::blank(40);
    let before = d.bytes.len();
    let mut log = boot(&mut d);
    let text: Vec<u8> = (0..300_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    flush(&mut d, &mut log, &text);
    assert_eq!(d.bytes.len(), before);
    // 38 ring sectors hold 19 000 bytes.
    let (got, _) = text_of(&d);
    assert!(got.len() <= 19_000 && got.len() > 18_500);
    assert_eq!(got.as_bytes(), &text[text.len() - got.len()..]);
}

#[test]
fn prepare_refuses_what_cannot_work_and_never_overruns() {
    assert!(Log::fresh(0).is_none());
    assert!(Log::fresh(17).is_none(), "ring under 16");
    assert!(Log::fresh(18).is_some());
    let log = Log::fresh(100).unwrap();
    assert!(log.prepare(b"x", &mut []).is_none());
    assert!(log.prepare(b"x", &mut [0u8; SECTOR - 1]).is_none());
    let mut out = [0u8; SECTOR];
    let p = log.prepare(b"x", &mut out).unwrap();
    assert_eq!((p.count, p.consumed), (1, 1));
    // The buffer is the limit: bigger offers are taken in parts.
    let mut out = vec![0u8; 3 * SECTOR];
    let big = vec![b'z'; 5000];
    let p = log.prepare(&big, &mut out).unwrap();
    assert_eq!((p.count, p.consumed), (3, 1500));
    let p = log.prepare(b"", &mut out).unwrap();
    assert_eq!((p.count, p.consumed), (0, 0));
    // Random sizes: never past the buffer, never more than a half ring.
    let mut rng = Rng(5);
    for _ in 0..500 {
        let file = 18 + rng.below(60) as u32;
        let mut log = Log::fresh(file).unwrap();
        log.begin_boot().unwrap();
        for _ in 0..40 {
            let n = rng.below(4000) as usize;
            let text: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            let mut out = vec![0u8; rng.below(40) as usize * SECTOR];
            if let Some(p) = log.prepare(&text, &mut out) {
                assert!(p.count as usize * SECTOR <= out.len());
                assert!(p.count <= OUT_SECTORS as u32 && p.count <= log.ring_sectors() / 2);
                assert!(p.consumed <= text.len());
                log.commit(&p);
            }
        }
    }
}

// ---- power cuts, torn and hostile files ---------------------------------------------------------------

#[test]
fn a_torn_header_copy_leaves_the_other_one() {
    let mut d = Disk::blank(256);
    let mut log = boot(&mut d);
    for i in 0..300 {
        flush(&mut d, &mut log, line(i).as_bytes());
    }
    header(&mut d, &mut log);
    let (want, _) = text_of(&d);
    for torn in 0..2 {
        let mut x = d.clone();
        let mut junk = vec![0u8; SECTOR];
        let mut rng = Rng(9 + torn);
        for b in junk.iter_mut() {
            *b = rng.next() as u8;
        }
        x.put(torn as u32, &junk);
        let mut sc = scratch();
        let r = recover(&mut x.clone(), 256, &mut sc).unwrap();
        assert_eq!(r.pos, log.pos(), "copy {torn} torn");
        assert_eq!(text_of(&x).0, want);
    }
}

#[test]
fn both_header_copies_gone_is_found_by_scanning_the_ring() {
    let mut d = Disk::blank(256);
    let mut log = boot(&mut d);
    for i in 0..300 {
        flush(&mut d, &mut log, line(i).as_bytes());
    }
    let (want, _) = text_of(&d);
    // Headers that look blank (zeros, spaces) on a ring with text in it are a log that lost its header, not a blank file.
    for fill in [0u8, b' '] {
        let mut x = d.clone();
        x.put(0, &[fill; SECTOR]);
        x.put(1, &[fill; SECTOR]);
        let r = recover(&mut x.clone(), 256, &mut scratch()).unwrap();
        assert_eq!((r.how, r.pos), (milklog::How::Scan, log.pos()), "fill {fill}");
    }
    d.put(0, &[0u8; SECTOR]);
    d.put(1, &[0xffu8; SECTOR]);
    let mut sc = scratch();
    let r = recover(&mut d.clone(), 256, &mut sc).unwrap();
    assert_eq!((r.how, r.pos, r.boot_count), (milklog::How::Scan, log.pos(), 1));
    assert!(r.header.is_none());
    assert_eq!(text_of(&d).0, want);
    // The next boot is boot 2 and appends.
    let mut l2 = boot(&mut d);
    assert_eq!(l2.boot(), 2);
    flush(&mut d, &mut l2, b"after\n");
    assert!(text_of(&d).0.starts_with(&want));
}

#[test]
fn a_header_that_lies_about_the_cursor_is_not_trusted() {
    let mut d = Disk::blank(256);
    let mut log = boot(&mut d);
    for i in 0..300 {
        flush(&mut d, &mut log, line(i).as_bytes());
    }
    header(&mut d, &mut log);
    let (want, _) = text_of(&d);
    let ring_bytes = log.ring_bytes();
    // Valid headers (right CRC) with a cursor in the future, in the past, mid-sector, at the end of the ring, absurd.
    let end = log.pos();
    for cursor in [0u64, 1, 499, 500, 70_000, end - 1, end, end + 1, end + 5000, ring_bytes, ring_bytes * 9, u64::MAX / 2, u64::MAX] {
        let mut h = milklog::best_header(d.sector(0), d.sector(1)).unwrap().0;
        h.cursor = cursor;
        h.seq = h.seq.wrapping_add(10);
        let mut b = [0u8; SECTOR];
        h.encode(&mut b);
        let mut x = d.clone();
        x.put(0, &b);
        let mut sc = scratch();
        let r = recover(&mut x.clone(), 256, &mut sc).unwrap_or_else(|e| panic!("cursor {cursor}: {e:?}"));
        assert_eq!(r.pos, end, "cursor {cursor}");
        assert_eq!(text_of(&x).0, want, "cursor {cursor}");
    }
}

#[test]
fn a_header_for_another_size_is_ignored() {
    let mut d = Disk::blank(256);
    let mut log = boot(&mut d);
    for i in 0..100 {
        flush(&mut d, &mut log, line(i).as_bytes());
    }
    // The file is "replaced" by a copy that is a different size: header geometry no longer fits.
    let mut small = Disk::blank(128);
    small.put(0, d.sector(0));
    small.put(1, d.sector(1));
    let mut sc = scratch();
    let e = recover(&mut small.clone(), 128, &mut sc).unwrap_err();
    assert_eq!(e, Refuse::Geometry);
}

#[test]
fn old_and_foreign_files_are_refused_untouched() {
    let mut sc = scratch();
    let mut v1 = Disk::blank(128);
    v1.put(0, b"MILKLOG 1 boot=0000abcd\nmilkOS 0.2.0\n--------\nboot: entered at EL2\n");
    let before = v1.bytes.clone();
    assert_eq!(recover(&mut v1.clone(), 128, &mut sc).unwrap_err(), Refuse::V1);
    assert_eq!(v1.bytes, before);
    let mut other = Disk::blank(128);
    other.put(0, b"just some text somebody keeps here\n");
    assert_eq!(recover(&mut other.clone(), 128, &mut sc).unwrap_err(), Refuse::Foreign);
    // A file under the ring size.
    assert_eq!(recover(&mut Disk::blank(10), 10, &mut sc).unwrap_err(), Refuse::TooSmall);
    assert_eq!(recover(&mut Disk::blank(17), 17, &mut sc).unwrap_err(), Refuse::TooSmall);
    // A scratch buffer that is too small.
    assert_eq!(recover(&mut Disk::blank(64), 64, &mut sc[..SECTOR]).unwrap_err(), Refuse::TooSmall);
    // A read error anywhere is reported, not hidden.
    struct Bad(u32);
    impl Source for Bad {
        fn read(&mut self, sector: u32, out: &mut [u8]) -> bool {
            out.fill(b' ');
            sector + (out.len() / SECTOR) as u32 <= self.0
        }
    }
    assert_eq!(recover(&mut Bad(0), 64, &mut sc).unwrap_err(), Refuse::Read(0));
}

#[test]
fn a_flush_cut_part_way_loses_only_that_flush() {
    let mut rng = Rng(0x5eed);
    for round in 0..300 {
        let file = 40 + rng.below(100) as u32;
        let mut d = Disk::blank(file);
        let mut log = boot(&mut d);
        let mut kept = String::from("=== milkOS boot 1 === test build\n");
        // Some history, flushed completely.
        for i in 0..rng.below(400) as usize {
            let l = line(i);
            kept.push_str(&l);
            flush(&mut d, &mut log, l.as_bytes());
        }
        let ring_bytes = log.ring_bytes() as usize;
        let acked = log.pos();
        // One more flush of up to 8 KB, of which only some sectors reach the card, in any order.
        let n = 1 + rng.below(8000) as usize;
        let text: Vec<u8> = (0..n).map(|_| b'a' + rng.below(26) as u8).collect();
        let mut out = vec![0u8; OUT_SECTORS * SECTOR];
        let p = log.prepare(&text, &mut out).unwrap();
        let mut order: Vec<u32> = (0..p.count).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i as u64 + 1) as usize);
        }
        let landed = rng.below(p.count as u64 + 1) as usize;
        for &i in &order[..landed] {
            let at = log.file_sector(p.first_abs + i);
            d.put(at, &out[i as usize * SECTOR..(i as usize + 1) * SECTOR]);
        }
        // Power cut. A new boot starts: it must find a consistent end at or after everything acknowledged.
        let mut sc = scratch();
        let r = recover(&mut d.clone(), file, &mut sc).unwrap();
        assert!(r.pos >= acked, "everything acknowledged is kept (round {round})");
        let full: Vec<u8> = {
            let mut v = kept.clone().into_bytes();
            v.extend_from_slice(&text[..p.consumed]);
            v
        };
        // The recovered end is the acked end or somewhere inside the landed flush, never past the flush.
        assert!(r.pos <= acked + p.consumed as u64, "round {round}");
        // What reads back is a suffix-consistent prefix of what was written.
        let (got, gaps) = text_of(&d);
        let upto = r.pos as usize;
        let full = String::from_utf8_lossy(&full[..upto]).into_owned();
        if upto <= ring_bytes && p.first_abs + p.count <= log.ring_sectors() {
            assert!(gaps.is_empty(), "round {round}: gaps {gaps:?}");
            assert_eq!(got, full, "round {round}");
        } else {
            // The sectors of the flush that landed beyond the end sit on the oldest text of the ring: that much is
            // damaged (reported as gaps); the newer text is exactly what was written.
            assert!(gaps.iter().sum::<u32>() <= p.count, "round {round}: gaps {gaps:?}");
            let safe = ((log.ring_sectors() as usize).saturating_sub(p.count as usize + 2) * PAYLOAD).min(got.len());
            let tail = &got.as_bytes()[got.len() - safe..];
            assert!(full.as_bytes().ends_with(tail), "round {round}: the newest text is what was written");
        }
        // The next boot carries on after it, and the log reads back as old text + the new boot.
        let mut l2 = boot(&mut d);
        flush(&mut d, &mut l2, b"next boot line\n");
        let (got2, gaps2) = text_of(&d);
        assert!(gaps2.iter().sum::<u32>() <= p.count, "ghosts only on the oldest text: {gaps2:?}");
        assert!(got2.ends_with("next boot line\n"));
        assert!(got2.contains(&format!("=== milkOS boot {} ===", l2.boot())));
    }
}

#[test]
fn a_ghost_sector_from_a_torn_write_is_not_resurrected() {
    // Boot 1 gets sectors N and N+2 of a flush onto the card but not N+1 (a reordered write), then the power goes.
    let mut d = Disk::blank(256);
    let mut log = boot(&mut d);
    flush(&mut d, &mut log, &vec![b'a'; 1000]);
    let mut out = vec![0u8; OUT_SECTORS * SECTOR];
    let p = log.prepare(&vec![b'b'; 1800], &mut out).unwrap();
    assert!(p.count >= 4);
    for i in [0u32, 2, 3] {
        let at = log.file_sector(p.first_abs + i);
        d.put(at, &out[i as usize * SECTOR..(i as usize + 1) * SECTOR]);
    }
    // Boot 2: finds the end at the gap, writes a short line (rewriting part of the gap), power cut again.
    let mut l2 = boot(&mut d);
    let end2 = l2.pos();
    assert!(end2 < log.pos() + 1800);
    flush(&mut d, &mut l2, b"boot two\n");
    // Boot 3 must not see boot 1's ghost sectors beyond boot 2's text as a continuation.
    let mut sc = scratch();
    let r = recover(&mut d.clone(), 256, &mut sc).unwrap();
    assert_eq!(r.pos, l2.pos(), "ghosts beyond the end are not text");
    let (got, _) = text_of(&d);
    assert!(got.ends_with("boot two\n"), "{:?}", &got[got.len().saturating_sub(40)..]);
}

#[test]
fn garbage_never_panics_and_never_claims_a_position_it_cannot_read() {
    let mut rng = Rng(0xfeed_f00d);
    let mut sc = scratch();
    for round in 0..400 {
        let file = 18 + rng.below(300) as u32;
        let mut d = Disk::blank(file);
        match round % 4 {
            0 => {
                // Pure noise.
                for b in d.bytes.iter_mut() {
                    *b = rng.next() as u8;
                }
            }
            1 => {
                // A good log with random bytes flipped anywhere.
                let mut log = boot(&mut d);
                for i in 0..rng.below(300) as usize {
                    flush(&mut d, &mut log, line(i).as_bytes());
                }
                for _ in 0..1 + rng.below(20) {
                    let at = rng.below(d.bytes.len() as u64) as usize;
                    d.bytes[at] = rng.next() as u8;
                }
            }
            2 => {
                // Good headers and sectors with random abs / len / boot fields, correctly sealed.
                let mut log = boot(&mut d);
                for i in 0..100 {
                    flush(&mut d, &mut log, line(i).as_bytes());
                }
                for _ in 0..30 {
                    let s = HEADER_SECTORS + rng.below(u64::from(file - HEADER_SECTORS)) as u32;
                    let mut sec = [0u8; SECTOR];
                    for b in sec[..PAYLOAD].iter_mut() {
                        *b = rng.next() as u8;
                    }
                    seal_sector(&mut sec, rng.next() as u32, rng.below(501) as usize, rng.next() as u16);
                    d.put(s, &sec);
                }
            }
            _ => {
                // A header with a random cursor and boot table on top of noise.
                for b in d.bytes.iter_mut() {
                    *b = rng.next() as u8;
                }
                let mut h = Header::new(file, file - 2);
                h.cursor = rng.next();
                h.boot_count = rng.next() as u32;
                let mut b = [0u8; SECTOR];
                h.encode(&mut b);
                d.put(0, &b);
            }
        }
        let before = d.bytes.clone();
        let res = recover(&mut d.clone(), file, &mut sc);
        if let Ok(r) = res {
            assert!(r.tail[(r.pos % PAYLOAD as u64) as usize..].iter().all(|&b| b == 0));
            // Resuming and writing from here never panics and never leaves the file.
            if let Some(mut log) = Log::resume(file, &r)
                && log.begin_boot().is_some()
            {
                let mut out = vec![0u8; OUT_SECTORS * SECTOR];
                if let Some(p) = log.prepare(b"=== milkOS boot x ===\nline\n", &mut out) {
                    apply(&mut d, &log, &p, &out);
                    log.commit(&p);
                }
            }
        }
        let mut dd = d.clone();
        let _ = milklog::read_text(&mut dd, file, &mut sc, |_| {}, |_| {});
        assert_eq!(d.bytes.len(), before.len());
    }
}

#[test]
fn what_is_written_never_leaves_the_ring_and_the_header_sectors() {
    // A write of any shape touches only file sectors 0..file, and ring text only at 2.. .
    let mut rng = Rng(3);
    for _ in 0..200 {
        let file = 18 + rng.below(100) as u32;
        let log = {
            let mut l = Log::fresh(file).unwrap();
            l.begin_boot().unwrap();
            l
        };
        let mut log = log;
        for _ in 0..50 {
            let text = vec![b'q'; rng.below(5000) as usize];
            let mut out = vec![0u8; OUT_SECTORS * SECTOR];
            let p = log.prepare(&text, &mut out).unwrap();
            for i in 0..p.count {
                let s = log.file_sector(p.first_abs + i);
                assert!(s >= HEADER_SECTORS && s < file, "sector {s} of {file}");
            }
            log.commit(&p);
            let mut h = [0u8; SECTOR];
            let at = log.make_header(&mut h);
            assert!(at < HEADER_SECTORS);
            log.header_written(&h);
        }
    }
}

#[test]
fn the_header_index_keeps_the_latest_boots() {
    let mut log = Log::fresh(200).unwrap();
    for i in 1..=(MAX_BOOTS as u32 + 7) {
        assert_eq!(log.begin_boot(), Some(i));
        log.end_boot(milklog::STATE_RESTART);
    }
    let mut b = [0u8; SECTOR];
    log.make_header(&mut b);
    let h = Header::decode(&b).unwrap();
    assert_eq!(h.boots().len(), MAX_BOOTS);
    assert_eq!(h.boots()[0].boot, 8);
    assert_eq!(h.boots().last().unwrap().boot, MAX_BOOTS as u32 + 7);
    assert!(h.boots().iter().all(|b| b.state == milklog::STATE_RESTART));
    assert_eq!(h.boot_count, MAX_BOOTS as u32 + 7);
    // The counter has a limit.
    let r = Recovered {
        pos: 0,
        tail: [0; PAYLOAD],
        boot_count: milklog::MAX_BOOT,
        header: None,
        how: milklog::How::Fresh,
        reads: 0,
    };
    let mut l = Log::resume(200, &r).unwrap();
    assert_eq!(l.begin_boot(), None);
}

#[test]
fn header_writes_alternate_and_are_due_at_checkpoints() {
    let mut d = Disk::blank(1024);
    let mut log = boot(&mut d);
    assert!(!log.header_stale() || log.pos() > 0);
    let mut seen = [0u32; 2];
    for k in 0..6 {
        let mut h = [0u8; SECTOR];
        let at = log.make_header(&mut h);
        d.put(at, &h);
        log.header_written(&h);
        seen[at as usize] += 1;
        assert!(!log.header_stale(), "round {k}");
        flush(&mut d, &mut log, &vec![b'x'; 1000]);
        assert!(log.header_stale());
        assert!(!log.header_due());
    }
    assert_eq!(seen[0] + seen[1], 6);
    assert!(seen[0] >= 2 && seen[1] >= 2, "{seen:?}");
    flush(&mut d, &mut log, &vec![b'x'; 70_000]);
    // flush() wrote a header at the 64 KiB mark, so none is due now.
    assert!(!log.header_due());
    assert!(log.header_writes() >= 7);
}

#[test]
fn banner_and_end_lines_parse() {
    assert_eq!(milklog::parse_banner("=== milkOS boot 12 === build abc1234, git abc1234"), Some(12));
    assert_eq!(milklog::parse_banner("=== milkOS boot 12 ended: crash ==="), None);
    assert_eq!(milklog::parse_banner("=== milkOS boot x === build"), None);
    assert_eq!(milklog::parse_banner("=== milkOS boot 99999999999 ==="), None);
    assert_eq!(milklog::parse_banner("[    1.000] === milkOS boot 3 ==="), None);
    assert_eq!(milklog::parse_banner(""), None);
    assert_eq!(milklog::parse_end("=== milkOS boot 12 ended: crash ==="), Some((12, "crash")));
    assert_eq!(milklog::parse_end("=== milkOS boot 7 ended: shutdown (clean) ==="), Some((7, "shutdown (clean)")));
    assert_eq!(milklog::parse_end("=== milkOS boot 7 ended: shutdown"), None);
    assert_eq!(milklog::parse_end("=== milkOS boot 7 === x"), None);
}

// ---- line assembly, the rate cap, stamps --------------------------------------------------------------

fn lines_of(bytes: &[u8]) -> Vec<String> {
    let mut a = Assembler::new();
    let mut out = Vec::new();
    for &b in bytes {
        if a.feed(b) {
            out.push(String::from_utf8_lossy(a.line()).into_owned());
            a.clear();
        }
    }
    out
}

#[test]
fn the_assembler_makes_plain_lines() {
    assert_eq!(lines_of(b"hello\nworld\n"), ["hello", "world"]);
    assert_eq!(lines_of(b"a\r\nb\r\n"), ["a", "b"]);
    assert_eq!(lines_of(b"\x1b[31mred\x1b[0m text\n"), ["red text"]);
    assert_eq!(lines_of(b"\x1b[2J\x1b[Hclear\n"), ["clear"]);
    assert_eq!(lines_of(b"\x1b]0;title\x07after\n"), ["after"]);
    assert_eq!(lines_of(b"\x1b]0;title\x1b\\after\n"), ["after"]);
    assert_eq!(lines_of(b"ab\x08\x08xy\n"), ["xy"]);
    assert_eq!(lines_of(b"ls\x08 \x08s\n"), ["ls"]);
    assert_eq!(lines_of("h\u{e9}\x08o\n".as_bytes()), ["ho"], "a backspace takes a whole UTF-8 character");
    assert_eq!(lines_of(b"a\tb\n"), ["a b"]);
    assert_eq!(lines_of(b"\x07bell\x00nul\n"), ["bellnul"]);
    assert_eq!(lines_of(b"\x1b(Bplain\n"), ["Bplain"]);
    assert_eq!(lines_of(b"\n\n"), ["", ""]);
    assert_eq!(lines_of(b"half"), Vec::<String>::new());
    // A long line is cut into pieces of at most LINE_MAX.
    let long = vec![b'x'; logintake::LINE_MAX * 2 + 10];
    let mut v = long.clone();
    v.push(b'\n');
    let ls = lines_of(&v);
    assert_eq!(ls.len(), 3);
    assert!(ls.iter().all(|l| l.len() <= logintake::LINE_MAX));
    assert_eq!(ls.iter().map(|l| l.len()).sum::<usize>(), long.len());
}

#[test]
fn the_assembler_survives_any_bytes() {
    let mut rng = Rng(77);
    let mut a = Assembler::new();
    for _ in 0..2_000_000 {
        let b = if rng.below(4) == 0 { 0x1b } else { rng.next() as u8 };
        if a.feed(b) {
            assert!(a.line().len() <= logintake::LINE_MAX);
            assert!(a.line().iter().all(|&c| c != b'\n' && (c >= 0x20 || c == b'\t')));
            a.clear();
        }
        assert!(a.line().len() <= logintake::LINE_MAX);
    }
    assert!(a.is_empty() || !a.line().is_empty());
}

#[test]
fn the_rate_cap_lets_a_boot_through_and_stops_a_storm() {
    let mut g = Gate::new();
    // A boot: 2500 lines in the first few seconds all pass.
    for i in 0..2500u64 {
        assert_eq!(g.admit(i, 80), Verdict::Pass, "line {i}");
    }
    // A storm: 100 000 lines in two seconds. Few pass, the rest are counted.
    let mut passed = 0u64;
    let mut notes = 0u64;
    let mut dropped_in_notes = 0u64;
    for i in 0..100_000u64 {
        match g.admit(2500 + i / 50, 80) {
            Verdict::Pass => passed += 1,
            Verdict::PassAfterNote(n) => {
                passed += 1;
                notes += 1;
                dropped_in_notes += u64::from(n);
            }
            Verdict::Drop => {}
        }
    }
    assert!(passed < 1200, "{passed}");
    assert_eq!(passed + g.dropped_total() - g.pending() as u64 - dropped_in_notes, passed);
    // Two seconds of storm: a note about once a second, not one per line that gets through.
    assert!((1..=3).contains(&notes), "{notes}");
    assert_eq!(g.dropped_total() + passed, 100_000);
    // After a quiet minute the sustained rate is back, and the next line carries the note.
    let v = g.admit(2500 + 2000 + 60_000, 80);
    assert!(matches!(v, Verdict::PassAfterNote(n) if n > 0) || g.pending() == 0, "{v:?}");
}

#[test]
fn the_sustained_rate_is_the_cap() {
    let mut g = Gate::new();
    // Use up the burst (short lines: the line bucket is the one that runs dry).
    while g.dropped_total() == 0 {
        g.admit(0, 10);
    }
    // Ten seconds at 100 lines a second: about 25 lines a second get through.
    let mut passed = 0;
    for i in 0..1000u64 {
        if !matches!(g.admit(10 + i * 10, 10), Verdict::Drop) {
            passed += 1;
        }
    }
    assert!((200..=300).contains(&passed), "{passed}");
    // By bytes: long lines hit the 4 KiB / s cap first.
    let mut g = Gate::new();
    let mut passed_bytes = 0u64;
    for i in 0..4000u64 {
        if !matches!(g.admit(i * 5, 800), Verdict::Drop) {
            passed_bytes += 800;
        }
    }
    // 20 seconds: the 256 KiB burst plus about 4 KiB a second.
    assert!(passed_bytes < 256 * 1024 + 20 * 4096 + 2000, "{passed_bytes}");
    assert!(passed_bytes > 256 * 1024, "{passed_bytes}");
}

#[test]
fn the_gate_handles_any_clock() {
    let mut rng = Rng(11);
    let mut g = Gate::new();
    for _ in 0..100_000 {
        let _ = g.admit(rng.next() >> rng.below(64), rng.below(5000) as usize);
    }
    let mut g = Gate::new();
    assert_eq!(g.admit(u64::MAX, usize::MAX / 2000), Verdict::Pass);
    let _ = g.admit(0, 10);
    let _ = g.admit(u64::MAX, 10);
}

#[test]
fn stamps_round_trip() {
    let mut b = [0u8; 32];
    for ms in [0u64, 1, 999, 1000, 123_456, 99_999_999, 123_456_789_012] {
        let n = logintake::stamp(ms, &mut b);
        let s = core::str::from_utf8(&b[..n]).unwrap();
        assert!(s.starts_with('[') && s.ends_with("] "));
        let text = format!("{s}hello");
        assert_eq!(logintake::parse_stamp(&text), Some(ms), "{text}");
    }
    assert_eq!(logintake::stamp(5, &mut b[..4]), 4, "a short buffer is filled, not overrun");
    assert_eq!(logintake::parse_stamp("no stamp"), None);
    assert_eq!(logintake::parse_stamp("[abc] x"), None);
    assert_eq!(logintake::parse_stamp("[    1.5] x"), None);
}

// ---- the whole story ----------------------------------------------------------------------------------

#[test]
fn three_boots_and_a_cut_read_back_in_order() {
    // The qemu test's story in miniature: boot 1 runs and is cut, boot 2 crashes, boot 3 shuts down.
    let mut d = Disk::blank(16 * 1024); // 8 MiB
    let mut story = String::new();
    {
        let mut l = boot(&mut d);
        story.push_str("=== milkOS boot 1 === test build\n");
        for i in 0..500 {
            story.push_str(&line(i));
            flush(&mut d, &mut l, line(i).as_bytes());
        }
    }
    {
        let mut l = boot(&mut d);
        story.push_str("=== milkOS boot 2 === test build\n");
        for i in 0..300 {
            story.push_str(&line(i));
            flush(&mut d, &mut l, line(i).as_bytes());
        }
        let e = "*** KERNEL PANIC ***\n=== milkOS boot 2 ended: crash ===\n";
        story.push_str(e);
        flush(&mut d, &mut l, e.as_bytes());
        l.end_boot(milklog::STATE_CRASH);
        header(&mut d, &mut l);
    }
    {
        let mut l = boot(&mut d);
        story.push_str("=== milkOS boot 3 === test build\n");
        for i in 0..100 {
            story.push_str(&line(i));
            flush(&mut d, &mut l, line(i).as_bytes());
        }
        let e = "=== milkOS boot 3 ended: shutdown ===\n";
        story.push_str(e);
        flush(&mut d, &mut l, e.as_bytes());
        l.end_boot(milklog::STATE_SHUTDOWN);
        header(&mut d, &mut l);
    }
    let (got, gaps) = text_of(&d);
    assert_eq!(got, story);
    assert!(gaps.is_empty());
    // Only ring sectors that were written differ from a blank file: header sectors and sectors up to the end.
    let blank = Disk::blank(16 * 1024);
    let end = 2 + (story.len() / PAYLOAD + 1) as u32;
    for s in end..16 * 1024 {
        assert_eq!(d.sector(s), blank.sector(s), "sector {s} beyond the text changed");
    }
}

#[allow(dead_code)]
fn unused(_: Recovered) {}
