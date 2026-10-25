//! Host tests for `fat` (finding the log file on a FAT32 card) and `logimg` (the file's layout): a FAT32
//! image built here, sparse so a card with 128-sector clusters costs nothing, with long-name entries mixed in,
//! fragmented chains, loops, out-of-range clusters, truncated chains and plain garbage.

use crate::fat::{self, Error, Extent, LogFile, Partition, SECTOR, Sectors, Volume, short_name};
use crate::logimg::{self, LogImage, MARKER_MAX, OUT_SECTORS};
use std::collections::BTreeMap;
use std::format;
use std::vec;
use std::vec::Vec;

const PART_START: u64 = 2048;
const CLUSTERS: u32 = 70_000;
const RESERVED: u32 = 32;
const FAT_SECTORS: u32 = (CLUSTERS + 2).div_ceil(128);

/// A sparse card: sectors never written read as zeros, up to `limit`.
#[derive(Clone)]
struct Img {
    s: BTreeMap<u64, [u8; SECTOR]>,
    limit: u64,
    reads: u64,
    fail_at: Option<u64>,
}

impl Img {
    fn sector(&mut self, lba: u64) -> &mut [u8; SECTOR] {
        self.s.entry(lba).or_insert([0; SECTOR])
    }
    fn put(&mut self, lba: u64, off: usize, bytes: &[u8]) {
        self.sector(lba)[off..off + bytes.len()].copy_from_slice(bytes);
    }
}

impl Sectors for Img {
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> bool {
        self.reads += 1;
        if lba >= self.limit || self.fail_at == Some(lba) {
            return false;
        }
        *buf = self.s.get(&lba).copied().unwrap_or([0; SECTOR]);
        true
    }
}

/// A volume under construction.
struct Fs {
    img: Img,
    spc: u32,
    next_dir_slot: usize,
}

impl Fs {
    fn new(spc: u32) -> Fs {
        let total = RESERVED + 2 * FAT_SECTORS + CLUSTERS * spc;
        let mut img = Img {
            s: BTreeMap::new(),
            limit: PART_START + u64::from(total) + 100,
            reads: 0,
            fail_at: None,
        };
        // MBR: partition 1 type 0x0c.
        let mut e = [0u8; 16];
        e[4] = 0x0c;
        e[8..12].copy_from_slice(&(PART_START as u32).to_le_bytes());
        e[12..16].copy_from_slice(&(total + 50).to_le_bytes());
        img.put(0, 446, &e);
        img.put(0, 510, &[0x55, 0xaa]);
        // Boot sector.
        let mut b = [0u8; SECTOR];
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = spc as u8;
        b[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
        b[16] = 2;
        b[32..36].copy_from_slice(&total.to_le_bytes());
        b[36..40].copy_from_slice(&FAT_SECTORS.to_le_bytes());
        b[44..48].copy_from_slice(&2u32.to_le_bytes());
        b[82..90].copy_from_slice(b"FAT32   ");
        b[510] = 0x55;
        b[511] = 0xaa;
        *img.sector(PART_START) = b;
        let mut fs = Fs {
            img,
            spc,
            next_dir_slot: 0,
        };
        fs.set_fat(0, 0x0fff_fff8);
        fs.set_fat(1, 0x0fff_ffff);
        fs.set_fat(2, 0x0fff_ffff); // the root directory: one cluster
        fs
    }

    fn set_fat(&mut self, c: u32, v: u32) {
        let lba = PART_START + u64::from(RESERVED) + u64::from(c / 128);
        self.img.put(lba, (c as usize % 128) * 4, &v.to_le_bytes());
    }

    fn cluster_lba(&self, c: u32) -> u64 {
        PART_START + u64::from(RESERVED + 2 * FAT_SECTORS) + u64::from(c - 2) * u64::from(self.spc)
    }

    /// Chain the clusters in `list` and end it.
    fn chain(&mut self, list: &[u32]) {
        for w in list.windows(2) {
            self.set_fat(w[0], w[1]);
        }
        self.set_fat(*list.last().unwrap(), 0x0fff_ffff);
    }

    /// A raw 32-byte entry into the next slot of the root directory (cluster 2).
    fn entry(&mut self, raw: [u8; 32]) {
        let per_cluster = 16 * self.spc as usize;
        assert!(self.next_dir_slot < per_cluster);
        let lba = self.cluster_lba(2) + (self.next_dir_slot / 16) as u64;
        self.img.put(lba, (self.next_dir_slot % 16) * 32, &raw);
        self.next_dir_slot += 1;
    }

    fn file(&mut self, name: &[u8; 11], attr: u8, cluster: u32, size: u32) {
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(name);
        e[11] = attr;
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        self.entry(e);
    }

    fn lfn(&mut self, seq: u8) {
        let mut e = [0x41u8; 32]; // 'A's, as a long name's UTF-16 would not look
        e[0] = seq;
        e[11] = 0x0f;
        e[12] = 0;
        e[26] = 0;
        e[27] = 0;
        self.entry(e);
    }

    /// A log file of `size` bytes in the clusters `list` (chained).
    fn log(&mut self, size: u32, list: &[u32]) {
        self.file(b"MILKLOG TXT", 0x20, list[0], size);
        self.chain(list);
    }

    fn contiguous(&self, size: u32, first: u32) -> Vec<u32> {
        let n = size.div_ceil(512).div_ceil(self.spc);
        (first..first + n).collect()
    }
}

fn name() -> [u8; 11] {
    short_name("MILKLOG.TXT").unwrap()
}

fn locate(fs: &mut Fs) -> Result<fat::Located, Error> {
    fat::locate(&mut fs.img, &name())
}

/// Every sector the file claims, as a list; checks the invariants every accepted file must have.
fn sectors_of(f: &LogFile, vol: &Volume) -> Vec<u64> {
    let mut v = Vec::new();
    let lo = vol.start + u64::from(vol.data_start);
    let hi = vol.start + u64::from(vol.total_sectors);
    for i in 0..f.sectors() {
        let l = f.lba(i).expect("a writable sector has an address");
        assert!(
            l >= lo && l < hi,
            "sector {l} outside the data area {lo}..{hi}"
        );
        assert!(f.contains(l, 1));
        v.push(l);
    }
    assert!(f.lba(f.sectors()).is_none());
    let mut sorted = v.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), v.len(), "a sector twice");
    v
}

#[test]
fn finds_a_contiguous_file() {
    for spc in [1u32, 2, 8, 64, 128] {
        let mut fs = Fs::new(spc);
        let clusters = fs.contiguous(262_144, 10);
        fs.log(262_144, &clusters);
        let found = locate(&mut fs).unwrap();
        assert_eq!(found.file.sectors(), 512);
        assert_eq!(found.file.size(), 262_144);
        assert_eq!(found.file.extent_count(), 1, "spc {spc}");
        assert_eq!(found.file.lba(0), Some(fs.cluster_lba(10)));
        assert_eq!(found.file.lba(511), Some(fs.cluster_lba(10) + 511));
        assert_eq!(found.file.lba(512), None);
        assert_eq!(sectors_of(&found.file, &found.volume).len(), 512);
        assert_eq!(found.partition.start, PART_START);
    }
}

#[test]
fn long_name_entries_and_other_files_are_skipped() {
    let mut fs = Fs::new(1);
    fs.file(b"BOOT-OLDTXT", 0x20, 5, 100);
    fs.file(b"MYDISK     ", 0x08, 0, 0); // volume label
    fs.file(b"MILKLOG TXT", 0x10, 6, 0); // a directory of that name
    fs.file(b"\xe5ILKLOG TXT", 0x20, 6, 4096); // deleted
    fs.lfn(0x42);
    fs.lfn(0x01);
    fs.file(b"MILKLOG TXT", 0x20, 20, 8192); // the real one, after its long-name entries
    fs.chain(&(20..36).collect::<Vec<_>>());
    let found = locate(&mut fs).unwrap();
    assert_eq!(found.file.size(), 8192);
    assert_eq!(found.file.lba(0), Some(fs.cluster_lba(20)));
}

#[test]
fn a_long_name_entry_is_never_taken_for_the_file() {
    // An LFN entry whose first 11 bytes happen to spell the name, attribute 0x0f.
    let mut fs = Fs::new(1);
    fs.file(b"MILKLOG TXT", 0x0f, 20, 8192);
    fs.chain(&(20..36).collect::<Vec<_>>());
    assert_eq!(locate(&mut fs).unwrap_err(), Error::NoFile);
}

#[test]
fn the_name_matches_in_any_case() {
    let mut fs = Fs::new(1);
    fs.log(4096, &[7, 8, 9, 10, 11, 12, 13, 14]);
    // Rewrite the entry in lower case.
    let lba = fs.cluster_lba(2);
    fs.img.put(lba, 0, b"milklog txt");
    assert!(locate(&mut fs).is_ok());
    assert_eq!(short_name("milklog.txt"), short_name("MILKLOG.TXT"));
    assert_eq!(short_name("toolongname.txt"), None);
    assert_eq!(short_name("a.b.c"), None);
    assert_eq!(short_name("noext").unwrap(), *b"NOEXT      ");
}

#[test]
fn follows_a_fragmented_chain() {
    let mut fs = Fs::new(1);
    // 8 sectors of 4096 bytes... use a 64-cluster file in 8 pieces of 8, with gaps.
    let mut list = Vec::new();
    for piece in 0..8u32 {
        for k in 0..8 {
            list.push(100 + piece * 20 + k);
        }
    }
    fs.log(64 * 512, &list);
    let found = locate(&mut fs).unwrap();
    assert_eq!(found.file.extent_count(), 8);
    let got = sectors_of(&found.file, &found.volume);
    let want: Vec<u64> = list.iter().map(|&c| fs.cluster_lba(c)).collect();
    assert_eq!(got, want);
    // A run never crosses the gap between two pieces.
    let (lba, n) = found.file.run(4, 100).unwrap();
    assert_eq!((lba, n), (fs.cluster_lba(104), 4));
    let (lba, n) = found.file.run(8, 100).unwrap();
    assert_eq!((lba, n), (fs.cluster_lba(120), 8));
}

#[test]
fn an_out_of_order_chain_works() {
    let mut fs = Fs::new(4);
    fs.log(16 * 512, &[50, 30, 31, 90]);
    let found = locate(&mut fs).unwrap();
    assert_eq!(found.file.extent_count(), 3);
    assert_eq!(found.file.sectors(), 16);
}

#[test]
fn a_size_that_is_not_a_whole_sector_writes_no_further() {
    let mut fs = Fs::new(8);
    // 4097 bytes -> 9 sectors -> 2 clusters of 8 sectors; the second cluster is mostly slack.
    fs.log(4097, &[40, 41]);
    let found = locate(&mut fs).unwrap();
    assert_eq!(found.file.sectors(), 9);
    assert_eq!(found.file.size(), 4097);
    let base = fs.cluster_lba(40);
    assert!(found.file.contains(base, 9));
    assert!(!found.file.contains(base, 10));
    assert!(!found.file.contains(base + 9, 1));
    assert_eq!(found.file.run(8, 50), Some((base + 8, 1)));
    assert_eq!(found.file.lba(9), None);
}

#[test]
fn contains_refuses_anything_outside_or_across_pieces() {
    let mut fs = Fs::new(1);
    let mut list: Vec<u32> = (100..108).collect();
    list.extend(200..208);
    fs.log(16 * 512, &list);
    let f = locate(&mut fs).unwrap().file;
    let a = fs.cluster_lba(100);
    let b = fs.cluster_lba(200);
    assert!(f.contains(a, 8) && f.contains(b, 8) && f.contains(a + 3, 2));
    assert!(!f.contains(a, 9), "across the gap");
    assert!(!f.contains(a - 1, 1));
    assert!(!f.contains(b + 8, 1));
    assert!(!f.contains(0, 1));
    assert!(!f.contains(PART_START, 1));
    assert!(!f.contains(a, 0));
    assert!(!f.contains(u64::MAX, 2));
    assert!(!f.contains(u64::MAX - 1, 5));
}

#[test]
fn refuses_a_loop() {
    let mut fs = Fs::new(1);
    fs.file(b"MILKLOG TXT", 0x20, 10, 8 * 512);
    for (a, b) in [(10, 11), (11, 12), (12, 11)] {
        fs.set_fat(a, b); // 10 -> 11 -> 12 -> 11 -> ...
    }
    let err = locate(&mut fs).unwrap_err();
    assert!(
        matches!(err, Error::BadFile(m) if m.contains("loop")),
        "{err:?}"
    );
    // A cluster that points at itself.
    let mut fs = Fs::new(1);
    fs.file(b"MILKLOG TXT", 0x20, 10, 8 * 512);
    fs.set_fat(10, 10);
    assert!(matches!(locate(&mut fs), Err(Error::BadFile(_))));
}

#[test]
fn refuses_clusters_that_are_not_data() {
    for bad in [
        0u32,
        1,
        0x0fff_fff7,
        0x0fff_fff0,
        CLUSTERS + 2,
        CLUSTERS + 100,
        0x0fff_ff00,
        0xffff_ffff,
    ] {
        let mut fs = Fs::new(1);
        fs.file(b"MILKLOG TXT", 0x20, 10, 8 * 512);
        fs.set_fat(10, 11);
        fs.set_fat(11, bad);
        assert!(
            matches!(locate(&mut fs), Err(Error::BadFile(_))),
            "next {bad:#x}"
        );
    }
    // The first cluster itself.
    for first in [0u32, 1, CLUSTERS + 2, 0x0fff_ffff] {
        let mut fs = Fs::new(1);
        fs.file(b"MILKLOG TXT", 0x20, first, 8 * 512);
        assert!(
            matches!(locate(&mut fs), Err(Error::BadFile(_))),
            "first {first:#x}"
        );
    }
    // The last valid cluster is fine.
    let mut fs = Fs::new(1);
    fs.log(
        8 * 512,
        &[
            CLUSTERS + 1 - 7,
            CLUSTERS + 1 - 6,
            CLUSTERS + 1 - 5,
            CLUSTERS + 1 - 4,
            CLUSTERS + 1 - 3,
            CLUSTERS + 1 - 2,
            CLUSTERS + 1 - 1,
            CLUSTERS + 1,
        ],
    );
    assert!(locate(&mut fs).is_ok());
}

#[test]
fn refuses_a_chain_that_is_too_short_or_too_long() {
    let mut fs = Fs::new(1);
    fs.file(b"MILKLOG TXT", 0x20, 10, 8 * 512);
    fs.chain(&[10, 11, 12, 13]); // 4 clusters, 8 needed
    let err = locate(&mut fs).unwrap_err();
    assert!(
        matches!(err, Error::BadFile(m) if m.contains("shorter")),
        "{err:?}"
    );
    let mut fs = Fs::new(1);
    fs.file(b"MILKLOG TXT", 0x20, 10, 8 * 512);
    fs.chain(&(10..22).collect::<Vec<_>>()); // 12 clusters, 8 needed
    let err = locate(&mut fs).unwrap_err();
    assert!(
        matches!(err, Error::BadFile(m) if m.contains("longer")),
        "{err:?}"
    );
    // Exactly right.
    let mut fs = Fs::new(1);
    fs.file(b"MILKLOG TXT", 0x20, 10, 8 * 512);
    fs.chain(&(10..18).collect::<Vec<_>>());
    assert!(locate(&mut fs).is_ok());
}

#[test]
fn refuses_files_of_a_bad_size() {
    for size in [0u32, 1, 511, 4095, 4 * 1024 * 1024 + 1, u32::MAX] {
        let mut fs = Fs::new(1);
        fs.file(b"MILKLOG TXT", 0x20, 10, size);
        fs.chain(&(10..30).collect::<Vec<_>>());
        assert!(
            matches!(locate(&mut fs), Err(Error::BadFile(_))),
            "size {size}"
        );
    }
    let mut fs = Fs::new(1);
    fs.log(4 * 1024 * 1024, &(10..10 + 8192).collect::<Vec<_>>());
    assert!(locate(&mut fs).is_ok(), "exactly 4 MiB is taken");
}

#[test]
fn refuses_a_file_in_too_many_pieces() {
    let mut fs = Fs::new(1);
    let list: Vec<u32> = (0..(fat::MAX_EXTENTS as u32 + 1) * 2)
        .step_by(2)
        .map(|i| 100 + i)
        .collect();
    fs.log(list.len() as u32 * 512, &list);
    let err = locate(&mut fs).unwrap_err();
    assert!(
        matches!(err, Error::BadFile(m) if m.contains("pieces")),
        "{err:?}"
    );
    let mut fs = Fs::new(1);
    let list: Vec<u32> = (0..fat::MAX_EXTENTS as u32 * 2)
        .step_by(2)
        .map(|i| 100 + i)
        .collect();
    fs.log(list.len() as u32 * 512, &list);
    assert!(locate(&mut fs).is_ok(), "exactly the limit is fine");
}

#[test]
fn a_missing_file_is_a_clean_no() {
    let mut fs = Fs::new(1);
    assert_eq!(locate(&mut fs).unwrap_err(), Error::NoFile);
    fs.file(b"OTHER   TXT", 0x20, 9, 5000);
    assert_eq!(locate(&mut fs).unwrap_err(), Error::NoFile);
}

#[test]
fn the_root_directory_may_span_clusters_and_a_loop_in_it_ends() {
    // The file is the 20th entry: in the second sector of a 1-sector-cluster directory, which is
    // cluster 3 of the directory chain 2 -> 3.
    let mut fs = Fs::new(1);
    fs.set_fat(2, 3);
    fs.set_fat(3, 0x0fff_ffff);
    for i in 0..16 {
        fs.file(
            format!("F{i:07}TXT").as_bytes().try_into().unwrap(),
            0x20,
            9,
            5000,
        );
    }
    // Slots 0..16 filled cluster 2; the next goes to... the helper only fills cluster 2, so write by hand.
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(b"MILKLOG TXT");
    e[11] = 0x20;
    e[26] = 40;
    e[28..32].copy_from_slice(&4096u32.to_le_bytes());
    let lba = fs.cluster_lba(3);
    fs.img.put(lba, 0, &e);
    fs.chain(&(40..48).collect::<Vec<_>>());
    // chain() rewrote only 40..; cluster 2 -> 3 is still there.
    let found = locate(&mut fs).unwrap();
    assert_eq!(found.file.lba(0), Some(fs.cluster_lba(40)));

    // A directory chain that loops and never has an end entry or the name: the search stops.
    let mut fs = Fs::new(1);
    fs.set_fat(2, 3);
    fs.set_fat(3, 2);
    for c in [2u32, 3] {
        let lba = fs.cluster_lba(c);
        for slot in 0..16 {
            let mut e = [b'X'; 32];
            e[11] = 0x20;
            fs.img.put(lba, slot * 32, &e);
        }
    }
    fs.img.reads = 0;
    assert_eq!(locate(&mut fs).unwrap_err(), Error::NoFile);
    assert!(
        fs.img.reads <= u64::from(fat::MAX_DIR_SECTORS) + 20,
        "{} reads",
        fs.img.reads
    );
}

#[test]
fn read_failures_are_errors_not_panics() {
    let mut fs = Fs::new(1);
    let list = fs.contiguous(16384, 10);
    fs.log(16384, &list);
    let good = locate(&mut fs).unwrap();
    for lba in [
        0,
        PART_START,
        PART_START + u64::from(RESERVED),
        fs.cluster_lba(2),
    ] {
        let mut broken = fs.img.clone();
        broken.fail_at = Some(lba);
        assert!(
            matches!(fat::locate(&mut broken, &name()), Err(Error::Read(l)) if l == lba),
            "lba {lba}"
        );
    }
    let _ = good;
    // A card smaller than its partition table says.
    let mut small = fs.img.clone();
    small.limit = 10;
    assert!(matches!(
        fat::locate(&mut small, &name()),
        Err(Error::Read(_))
    ));
}

#[test]
fn the_partition_table_is_checked() {
    let mut fs = Fs::new(1);
    let list = fs.contiguous(16384, 10);
    fs.log(16384, &list);
    let mbr = *fs.img.sector(0);
    assert!(fat::first_partition(&mbr).is_ok());
    let mut m = mbr;
    m[510] = 0;
    assert!(matches!(fat::first_partition(&m), Err(Error::NotFat(_))));
    let mut m = mbr;
    m[446 + 4] = 0x83; // Linux
    assert!(matches!(fat::first_partition(&m), Err(Error::NotFat(_))));
    let mut m = mbr;
    m[446 + 4] = 0x0b;
    assert_eq!(fat::first_partition(&m).unwrap().kind, 0x0b);
    let mut m = mbr;
    m[446] = 0x55; // not a status byte
    assert!(matches!(fat::first_partition(&m), Err(Error::NotFat(_))));
    let mut m = mbr;
    m[446 + 8..446 + 12].fill(0); // starts at 0
    assert!(fat::first_partition(&m).is_err());
    // The first FAT partition wins, not the first entry: a Linux partition 2 and a FAT partition 1 in the usual order.
    let mut m = mbr;
    let e1: [u8; 16] = m[446..462].try_into().unwrap();
    m[462..478].copy_from_slice(&e1);
    m[446 + 4] = 0x83;
    assert_eq!(fat::first_partition(&m).unwrap().start, PART_START);
    // A milkfs-style sector 0 (no signature): refused.
    assert!(matches!(
        fat::first_partition(&[0x41; SECTOR]),
        Err(Error::NotFat(_))
    ));
}

#[test]
fn the_boot_sector_is_checked() {
    let fs = Fs::new(1);
    let mut img = fs.img.clone();
    let part = Partition {
        start: PART_START,
        sectors: 10_000_000,
        kind: 0x0c,
    };
    let mut good = [0u8; SECTOR];
    img.read(PART_START, &mut good);
    assert!(Volume::parse(&good, &part).is_ok());
    let edit = |f: &dyn Fn(&mut [u8; SECTOR])| {
        let mut b = good;
        f(&mut b);
        Volume::parse(&b, &part)
    };
    assert!(edit(&|b| b[510] = 0).is_err());
    assert!(edit(&|b| b[11..13].copy_from_slice(&4096u16.to_le_bytes())).is_err());
    assert!(edit(&|b| b[13] = 3).is_err());
    assert!(edit(&|b| b[13] = 0).is_err());
    assert!(edit(&|b| b[14..16].fill(0)).is_err());
    assert!(edit(&|b| b[16] = 0).is_err());
    assert!(edit(&|b| b[16] = 3).is_err());
    assert!(edit(&|b| b[17] = 2).is_err(), "FAT16 root entries");
    assert!(edit(&|b| b[19] = 1).is_err(), "FAT16 sector count");
    assert!(edit(&|b| b[22] = 1).is_err(), "FAT16 FAT size");
    assert!(edit(&|b| b[32..36].fill(0)).is_err());
    assert!(edit(&|b| b[36..40].fill(0)).is_err());
    assert!(
        edit(&|b| b[36..40].copy_from_slice(&1u32.to_le_bytes())).is_err(),
        "a FAT too small for its clusters"
    );
    assert!(edit(&|b| b[44..48].fill(0)).is_err());
    assert!(edit(&|b| b[44..48].copy_from_slice(&0x0fff_fff0u32.to_le_bytes())).is_err());
    assert!(
        edit(&|b| b[32..36].copy_from_slice(&0xffff_ffffu32.to_le_bytes())).is_err(),
        "bigger than the partition"
    );
    assert!(
        edit(&|b| b[32..36].copy_from_slice(&100u32.to_le_bytes())).is_err(),
        "too few clusters"
    );
}

/// A small deterministic generator.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn garbage_never_panics_and_never_names_a_sector_outside_the_data_area() {
    // Pure noise.
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..300 {
        let mut img = Img {
            s: BTreeMap::new(),
            limit: 1 << 20,
            reads: 0,
            fail_at: None,
        };
        // Noise that looks a little like a card: an MBR signature, a plausible entry, a boot sector signature.
        for lba in 0..4 {
            let sec = img.sector(lba);
            for b in sec.iter_mut() {
                *b = rng.next() as u8;
            }
        }
        img.put(0, 510, &[0x55, 0xaa]);
        let mut e = [0u8; 16];
        e[4] = 0x0c;
        e[8..12].copy_from_slice(&1u32.to_le_bytes());
        e[12..16].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
        img.put(0, 446, &e);
        img.put(1, 510, &[0x55, 0xaa]);
        if let Ok(found) = fat::locate(&mut img, &name()) {
            sectors_of(&found.file, &found.volume);
        }
    }
    // A good card with bytes flipped anywhere in the sectors that matter.
    let mut base = Fs::new(1);
    let list: Vec<u32> = (10..26).chain(40..56).collect();
    base.log(32 * 512, &list);
    base.file(b"OTHER   TXT", 0x20, 70, 9999);
    let mut accepted = 0;
    for round in 0..4000 {
        let mut fs = Fs::new(1);
        fs.img = base.img.clone();
        let targets = [
            0u64,
            PART_START,
            fs.cluster_lba(2),
            PART_START + u64::from(RESERVED),
            PART_START + u64::from(RESERVED),
        ];
        for _ in 0..1 + round % 4 {
            let lba = targets[(rng.next() % targets.len() as u64) as usize];
            let off = if lba == PART_START || lba == 0 {
                (rng.next() % 512) as usize
            } else {
                (rng.next() % 256) as usize
            };
            fs.img.sector(lba)[off] = rng.next() as u8;
        }
        let mut img = fs.img.clone();
        if let Ok(found) = fat::locate(&mut img, &name()) {
            accepted += 1;
            let v = sectors_of(&found.file, &found.volume);
            assert_eq!(v.len() as u32, found.file.sectors());
            assert!(found.file.extent_count() <= fat::MAX_EXTENTS);
        }
    }
    assert!(
        accepted > 100,
        "the mutations should leave some cards valid ({accepted})"
    );
}

// ---- the log file's layout ----------------------------------------------------------------------------

/// A file as the card holds it: spaces after a header line, like scripts/pi-logfile.sh makes.
struct Model {
    bytes: Vec<u8>,
}

impl Model {
    fn new(sectors: u32) -> Model {
        let mut bytes = vec![b' '; sectors as usize * SECTOR];
        let head = b"MILKLOG.TXT: not written yet\n";
        bytes[..head.len()].copy_from_slice(head);
        Model { bytes }
    }
    fn apply(&mut self, first: u32, count: u32, out: &[u8]) {
        let a = first as usize * SECTOR;
        let n = count as usize * SECTOR;
        self.bytes[a..a + n].copy_from_slice(&out[..n]);
    }
}

/// Offer `text` to the image until it is all taken; apply each round to the model.
fn feed(img: &mut LogImage, m: &mut Model, text: &[u8]) -> usize {
    let sectors = (m.bytes.len() / SECTOR) as u32;
    let mut out = vec![0u8; OUT_SECTORS * SECTOR];
    let mut rest = text;
    let mut rounds = 0;
    loop {
        let p = img.prepare(rest, sectors, &mut out).expect("prepare");
        assert!(p.count as usize <= OUT_SECTORS);
        assert!(p.first + p.count <= sectors, "write beyond the file");
        m.apply(p.first, p.count, &out);
        img.commit(&p);
        rounds += 1;
        rest = &rest[p.consumed.min(rest.len())..];
        if rest.is_empty() {
            return rounds;
        }
        assert!(rounds < 1000);
    }
}

#[test]
fn the_text_and_one_marker_end_up_in_the_file() {
    let mut img = LogImage::new(0xabcd);
    let mut m = Model::new(64);
    let mut want = Vec::new();
    for i in 0..300 {
        let line = format!("line {i}: the quick brown fox jumps over the lazy dog\n");
        want.extend_from_slice(line.as_bytes());
        feed(&mut img, &mut m, line.as_bytes());
        let (end, boot, flush, bytes, full) = logimg::find_marker(&m.bytes).expect("a marker");
        assert_eq!(&m.bytes[..end], &want[..], "after line {i}");
        assert_eq!(
            (boot, flush, bytes, full),
            (0xabcd, i as u32 + 1, want.len() as u64, false)
        );
        // Exactly one marker for this boot, at the end.
        let n = m
            .bytes
            .windows(logimg::MARKER_START.len())
            .filter(|w| *w == logimg::MARKER_START)
            .count();
        assert_eq!(n, 1);
    }
    assert!(want.len() > 10_000);
    assert_eq!(img.pos(), want.len() as u64);
}

#[test]
fn a_big_chunk_goes_in_rounds_and_a_flush_with_no_text_just_refreshes_the_marker() {
    let mut img = LogImage::new(1);
    let mut m = Model::new(64);
    let text: Vec<u8> = (0..20_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let rounds = feed(&mut img, &mut m, &text);
    assert!(rounds >= 3, "{rounds}");
    let (end, _, _, bytes, _) = logimg::find_marker(&m.bytes).unwrap();
    assert_eq!(&m.bytes[..end], &text[..]);
    assert_eq!(bytes, 20_000);
    feed(&mut img, &mut m, b"");
    let (end2, _, flush, _, _) = logimg::find_marker(&m.bytes).unwrap();
    assert_eq!(end2, end);
    assert_eq!(flush, rounds as u32 + 1);
}

#[test]
fn a_flush_writes_only_the_sectors_at_the_end() {
    let mut img = LogImage::new(2);
    let mut m = Model::new(64);
    feed(&mut img, &mut m, &vec![b'x'; 3000]);
    let mut out = vec![0u8; OUT_SECTORS * SECTOR];
    let p = img.prepare(b"more\n", 64, &mut out).unwrap();
    // 3000 bytes of text end in sector 5 (3000 / 512): the next flush starts there.
    assert_eq!(p.first, 5);
    assert_eq!(p.count, 1);
    // The text before is unchanged where it is not rewritten: the first rewritten sector starts with the old tail.
    assert_eq!(&out[..3000 % 512], &vec![b'x'; 3000 % 512][..]);
}

#[test]
fn a_full_file_keeps_its_marker_and_says_so() {
    let mut img = LogImage::new(3);
    let mut m = Model::new(8); // 4096 bytes
    let text = vec![b'y'; 10_000];
    feed(&mut img, &mut m, &text);
    let (end, _, _, bytes, full) = logimg::find_marker(&m.bytes).unwrap();
    assert!(full && img.is_full());
    assert_eq!(bytes as usize, end);
    assert!(
        end + MARKER_MAX <= 4096,
        "the marker fits inside the file ({end})"
    );
    assert!(m.bytes[..end].iter().all(|&b| b == b'y'));
    // More text is dropped, never written.
    feed(&mut img, &mut m, b"later\n");
    assert_eq!(m.bytes.len(), 4096);
    let (end2, _, _, _, full2) = logimg::find_marker(&m.bytes).unwrap();
    assert!(full2);
    assert_eq!(end2, end);
}

#[test]
fn a_stale_marker_from_an_earlier_boot_is_behind_ours() {
    let mut m = Model::new(64);
    let mut old = LogImage::new(0x1111);
    feed(&mut old, &mut m, &vec![b'o'; 9000]);
    let mut new = LogImage::new(0x2222);
    feed(&mut new, &mut m, b"new boot\n");
    let (end, boot, ..) = logimg::find_marker(&m.bytes).unwrap();
    assert_eq!(boot, 0x2222, "the first marker is the new boot's");
    assert_eq!(&m.bytes[..end], b"new boot\n");
    // The old marker is still further down the file: the boot ids tell them apart.
    let rest = &m.bytes[end + 10..];
    assert!(logimg::find_marker(rest).is_some_and(|(_, b, ..)| b == 0x1111));
}

#[test]
fn prepare_refuses_what_cannot_work() {
    let img = LogImage::new(4);
    let mut out = vec![0u8; OUT_SECTORS * SECTOR];
    assert!(
        img.prepare(b"x", 1, &mut out).is_none(),
        "a file of one sector"
    );
    assert!(img.prepare(b"x", 0, &mut out).is_none());
    assert!(
        img.prepare(b"x", 64, &mut out[..SECTOR]).is_none(),
        "a one-sector buffer"
    );
    assert!(img.prepare(b"x", 64, &mut []).is_none());
    assert!(img.prepare(b"x", 64, &mut out).is_some());
    // Arbitrary text, arbitrary file sizes, small buffers: never writes outside, never panics.
    let mut rng = Rng(77);
    for _ in 0..500 {
        let sectors = 2 + (rng.next() % 40) as u32;
        let mut m = Model::new(sectors);
        let mut img = LogImage::new(rng.next() as u32);
        for _ in 0..30 {
            let n = (rng.next() % 3000) as usize;
            let text: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            let mut out = vec![0u8; (2 + rng.next() % 15) as usize * SECTOR];
            let Some(p) = img.prepare(&text, sectors, &mut out) else {
                continue;
            };
            assert!(
                p.first + p.count <= sectors,
                "{sectors} {} {}",
                p.first,
                p.count
            );
            assert!(p.count as usize * SECTOR <= out.len());
            m.apply(p.first, p.count, &out);
            img.commit(&p);
        }
    }
}

#[test]
fn extents_of_a_default_card_are_listed() {
    let mut fs = Fs::new(1);
    let list = fs.contiguous(65_536, 33);
    fs.log(65_536, &list);
    let f = locate(&mut fs).unwrap().file;
    assert_eq!(
        f.extents(),
        &[Extent {
            lba: fs.cluster_lba(33),
            sectors: 128
        }]
    );
    assert_eq!(LogFile::empty().extent_count(), 0);
    assert_eq!(LogFile::empty().run(0, 1), None);
}
