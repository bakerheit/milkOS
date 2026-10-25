//! Finding one pre-created file on a FAT32 boot partition, for the boot log (`kernel/src/rpi/logfile.rs`,
//! docs/raspberry-pi.md, "The boot log in a file").
//!
//! The kernel never creates a file, never changes the FAT and never changes a directory: the person's Mac makes
//! `MILKLOG.TXT` (scripts/pi-logfile.sh) and the kernel only overwrites the data sectors of that
//! existing file. Everything here is therefore about *reading* a card that may hold anything (a hostile or
//! just odd one) and ending with either a list of sectors that really are the file's, or an [`Error`]:
//!
//! * [`first_partition`]: the MBR's first FAT32 partition (type 0x0b / 0x0c);
//! * [`Volume::parse`]: the FAT32 boot sector (BPB), checked for sense (512-byte sectors, a power-of-two
//!   cluster size, at least 65525 clusters, a FAT big enough for them, the data area inside the partition);
//! * [`locate`]: the root directory searched for an 8.3 name (long-name entries ignored, case-insensitive),
//!   then the file's cluster chain followed through the FAT into a [`LogFile`]: at most [`MAX_EXTENTS`] runs
//!   of sectors. The chain must have exactly as many clusters as the file's size needs and end there (no
//!   free, bad, reserved, out-of-range or repeated cluster, no loop, nothing longer or shorter), or the
//!   file is refused.
//!
//! [`LogFile`] is the only thing the writer is allowed to write through: [`LogFile::run`] and
//! [`LogFile::contains`] never name a sector outside the first `size / 512` sectors of the file.
//! All loops here are bounded; no function panics on any input (the host tests feed garbage).

/// Sector size. FAT32 volumes with another sector size are refused.
pub const SECTOR: usize = 512;
/// Most runs of consecutive sectors a file may be split into.
pub const MAX_EXTENTS: usize = 128;
/// Largest log file taken (sectors): 4 MiB.
pub const MAX_FILE_SECTORS: u32 = 8192;
/// Smallest log file taken (bytes).
pub const MIN_FILE_BYTES: u32 = 4096;
/// The most directory sectors read while looking for the name.
pub const MAX_DIR_SECTORS: u32 = 4096;
/// A volume with fewer clusters than this is FAT12 / FAT16 by the specification's definition.
pub const MIN_FAT32_CLUSTERS: u64 = 65525;

/// Where sectors come from (the SD card, or an image in a test).
pub trait Sectors {
    /// Read sector `lba` of the card (absolute, not relative to a partition). False if it cannot be read.
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> bool;
}

/// Why a card was refused. The text is for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A sector could not be read.
    Read(u64),
    /// Not a FAT32 card as far as the partition table or boot sector can tell.
    NotFat(&'static str),
    /// The volume's numbers do not add up.
    BadVolume(&'static str),
    /// No file of that name in the root directory.
    NoFile,
    /// The directory entry or the cluster chain is not one we will write through.
    BadFile(&'static str),
}

impl Error {
    /// A short phrase for the log.
    pub fn what(&self) -> &'static str {
        match self {
            Error::Read(_) => "a sector could not be read",
            Error::NotFat(w) | Error::BadVolume(w) | Error::BadFile(w) => w,
            Error::NoFile => "no such file in the root directory",
        }
    }
}

fn u16le(b: &[u8], o: usize) -> u32 {
    u32::from(b[o]) | u32::from(b[o + 1]) << 8
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from(b[o])
        | u32::from(b[o + 1]) << 8
        | u32::from(b[o + 2]) << 16
        | u32::from(b[o + 3]) << 24
}

/// A partition from the MBR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    pub start: u64,
    pub sectors: u64,
    pub kind: u8,
}

/// The first FAT32 partition (type 0x0b or 0x0c, with a start and a size) in an MBR sector.
pub fn first_partition(mbr: &[u8; SECTOR]) -> Result<Partition, Error> {
    if mbr[510] != 0x55 || mbr[511] != 0xaa {
        return Err(Error::NotFat(
            "no partition table (sector 0 has no 0x55aa signature)",
        ));
    }
    for i in 0..4 {
        let e = &mbr[446 + 16 * i..446 + 16 * (i + 1)];
        // The status byte is 0x00 or 0x80; anything else means this is not a partition table.
        if e[0] != 0x00 && e[0] != 0x80 {
            return Err(Error::NotFat("no partition table (bad status byte)"));
        }
    }
    for i in 0..4 {
        let e = &mbr[446 + 16 * i..446 + 16 * (i + 1)];
        let (kind, start, sectors) = (e[4], u64::from(u32le(e, 8)), u64::from(u32le(e, 12)));
        if (kind == 0x0b || kind == 0x0c) && start > 0 && sectors > 0 {
            return Ok(Partition {
                start,
                sectors,
                kind,
            });
        }
    }
    Err(Error::NotFat(
        "no FAT32 partition (type 0x0b or 0x0c) in the partition table",
    ))
}

/// A FAT32 volume's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Absolute sector of the volume's first sector (the boot sector).
    pub start: u64,
    pub sectors_per_cluster: u32,
    /// First FAT, in sectors from `start`.
    pub fat_start: u32,
    pub fat_sectors: u32,
    pub fats: u32,
    /// First data sector (cluster 2), from `start`.
    pub data_start: u32,
    /// Number of data clusters (clusters 2 ..= clusters + 1).
    pub clusters: u32,
    pub root_cluster: u32,
    pub total_sectors: u32,
}

impl Volume {
    /// Check a boot sector `bs` that lies at sector `part.start`.
    pub fn parse(bs: &[u8; SECTOR], part: &Partition) -> Result<Volume, Error> {
        if bs[510] != 0x55 || bs[511] != 0xaa {
            return Err(Error::NotFat("the partition has no boot sector signature"));
        }
        if u16le(bs, 11) != SECTOR as u32 {
            return Err(Error::NotFat("not 512-byte sectors"));
        }
        let spc = u32::from(bs[13]);
        if spc == 0 || !spc.is_power_of_two() || spc > 128 {
            return Err(Error::NotFat(
                "sectors per cluster is not a power of two up to 128",
            ));
        }
        let reserved = u16le(bs, 14);
        let fats = u32::from(bs[16]);
        if reserved == 0 || !(1..=2).contains(&fats) {
            return Err(Error::NotFat("reserved sector or FAT count out of range"));
        }
        if u16le(bs, 17) != 0 || u16le(bs, 19) != 0 || u16le(bs, 22) != 0 {
            return Err(Error::NotFat("FAT12/16 fields set: not FAT32"));
        }
        let total = u32le(bs, 32);
        let fat_sectors = u32le(bs, 36);
        let root_cluster = u32le(bs, 44);
        if total == 0 || fat_sectors == 0 {
            return Err(Error::NotFat("no sector count or FAT size"));
        }
        if u64::from(total) > part.sectors {
            return Err(Error::BadVolume("the volume is larger than its partition"));
        }
        let data_start = u64::from(reserved) + u64::from(fats) * u64::from(fat_sectors);
        if data_start >= u64::from(total) {
            return Err(Error::BadVolume("no room for data after the FATs"));
        }
        let clusters = (u64::from(total) - data_start) / u64::from(spc);
        if clusters < MIN_FAT32_CLUSTERS {
            return Err(Error::NotFat("too few clusters for FAT32"));
        }
        if clusters + 2 > u64::from(fat_sectors) * 128 || clusters + 2 >= 0x0fff_fff0 {
            return Err(Error::BadVolume(
                "the FAT is too small for the cluster count",
            ));
        }
        if u64::from(root_cluster) < 2 || u64::from(root_cluster) > clusters + 1 {
            return Err(Error::BadVolume("root directory cluster out of range"));
        }
        Ok(Volume {
            start: part.start,
            sectors_per_cluster: spc,
            fat_start: reserved,
            fat_sectors,
            fats,
            data_start: data_start as u32,
            clusters: clusters as u32,
            root_cluster,
            total_sectors: total,
        })
    }

    /// Is `c` a data cluster of this volume?
    pub fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && u64::from(c) <= u64::from(self.clusters) + 1
    }

    /// Absolute sector of the first sector of cluster `c`.
    pub fn cluster_lba(&self, c: u32) -> Option<u64> {
        self.valid_cluster(c).then(|| {
            self.start
                + u64::from(self.data_start)
                + u64::from(c - 2) * u64::from(self.sectors_per_cluster)
        })
    }
}

/// One FAT sector, kept so a chain that runs through it costs one read.
struct FatCache {
    lba: u64,
    buf: [u8; SECTOR],
}

/// The FAT entry of cluster `c`: its next cluster, or an end / free / bad marker (top 4 bits dropped).
fn fat_entry<S: Sectors>(
    vol: &Volume,
    src: &mut S,
    cache: &mut FatCache,
    c: u32,
) -> Result<u32, Error> {
    // `c` < clusters + 2 <= fat_sectors * 128 (checked in Volume::parse), so the sector is inside the FAT.
    let sector = u64::from(c) / 128;
    if sector >= u64::from(vol.fat_sectors) {
        return Err(Error::BadFile("a cluster number points outside the FAT"));
    }
    let lba = vol.start + u64::from(vol.fat_start) + sector;
    if cache.lba != lba {
        if !src.read(lba, &mut cache.buf) {
            cache.lba = u64::MAX;
            return Err(Error::Read(lba));
        }
        cache.lba = lba;
    }
    Ok(u32le(&cache.buf, (c as usize % 128) * 4) & 0x0fff_ffff)
}

fn is_end(v: u32) -> bool {
    v >= 0x0fff_fff8
}

/// A run of consecutive sectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Extent {
    pub lba: u64,
    pub sectors: u32,
}

/// The sectors of the log file: the only sectors the writer may touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFile {
    extents: [Extent; MAX_EXTENTS],
    n: usize,
    /// Sectors of the file that may be written: `ceil(size / 512)`; always inside the extents.
    sectors: u32,
    size: u32,
}

impl LogFile {
    pub const fn empty() -> LogFile {
        LogFile {
            extents: [Extent { lba: 0, sectors: 0 }; MAX_EXTENTS],
            n: 0,
            sectors: 0,
            size: 0,
        }
    }

    /// Writable sectors.
    pub fn sectors(&self) -> u32 {
        self.sectors
    }

    /// The file's size in bytes, from its directory entry.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Runs of consecutive sectors the file is made of.
    pub fn extent_count(&self) -> usize {
        self.n
    }

    pub fn extents(&self) -> &[Extent] {
        &self.extents[..self.n]
    }

    /// Absolute sector of the file's sector `idx` (0-based), if it is a writable one.
    pub fn lba(&self, idx: u32) -> Option<u64> {
        self.run(idx, 1).map(|(l, _)| l)
    }

    /// The run of consecutive sectors that starts at file sector `idx` and is at most `max` long
    /// (and does not leave the file's writable sectors): (absolute sector, count). None if `idx`
    /// is not a writable sector or `max` is 0.
    pub fn run(&self, idx: u32, max: u32) -> Option<(u64, u32)> {
        if idx >= self.sectors || max == 0 {
            return None;
        }
        let mut base = 0u32;
        for e in self.extents() {
            if idx < base + e.sectors {
                let off = idx - base;
                let count = max.min(e.sectors - off).min(self.sectors - idx);
                return Some((e.lba + u64::from(off), count));
            }
            base += e.sectors;
        }
        None
    }

    /// Is the whole run `[lba, lba + count)` inside one extent and inside the writable sectors?
    /// The writer asks this before every write.
    pub fn contains(&self, lba: u64, count: u32) -> bool {
        if count == 0 {
            return false;
        }
        let Some(end) = lba.checked_add(u64::from(count)) else {
            return false;
        };
        let mut base = 0u64;
        for e in self.extents() {
            if lba >= e.lba && end <= e.lba + u64::from(e.sectors) {
                let first = base + (lba - e.lba);
                return first + u64::from(count) <= u64::from(self.sectors);
            }
            base += u64::from(e.sectors);
        }
        false
    }

    fn push(&mut self, lba: u64, sectors: u32) -> Result<(), Error> {
        if let Some(last) = self.extents[..self.n].last_mut()
            && last.lba + u64::from(last.sectors) == lba
        {
            last.sectors += sectors;
            return Ok(());
        }
        if self.n == MAX_EXTENTS {
            return Err(Error::BadFile("the file is split into too many pieces"));
        }
        self.extents[self.n] = Extent { lba, sectors };
        self.n += 1;
        Ok(())
    }

    /// Does any extent already hold the first sector of a cluster at `lba` (a repeated cluster)?
    fn holds(&self, lba: u64) -> bool {
        self.extents()
            .iter()
            .any(|e| lba >= e.lba && lba < e.lba + u64::from(e.sectors))
    }
}

/// What [`locate`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub partition: Partition,
    pub volume: Volume,
    pub file: LogFile,
}

/// Upper-case ASCII for comparing 8.3 names.
fn up(b: u8) -> u8 {
    b.to_ascii_uppercase()
}

/// The 11-byte directory form of an 8.3 name like `MILKLOG.TXT` (upper-cased, space padded). None
/// if the name does not fit 8.3.
pub fn short_name(name: &str) -> Option<[u8; 11]> {
    let (stem, ext) = match name.split_once('.') {
        Some((s, e)) => (s, e),
        None => (name, ""),
    };
    if stem.is_empty() || stem.len() > 8 || ext.len() > 3 || ext.contains('.') || !name.is_ascii() {
        return None;
    }
    let mut out = [b' '; 11];
    for (i, b) in stem.bytes().enumerate() {
        out[i] = up(b);
    }
    for (i, b) in ext.bytes().enumerate() {
        out[8 + i] = up(b);
    }
    Some(out)
}

/// A root directory entry that names a plain file `name`: (first cluster, size).
fn match_entry(e: &[u8], name: &[u8; 11]) -> Option<(u32, u32)> {
    let attr = e[11];
    if attr & 0x3f == 0x0f || attr & 0x18 != 0 {
        return None; // a long-name entry, a volume label or a directory
    }
    if e.iter().take(11).zip(name).any(|(a, b)| up(*a) != up(*b)) {
        return None;
    }
    let cluster = u16le(e, 26) | u16le(e, 20) << 16;
    Some((cluster & 0x0fff_ffff, u32le(e, 28)))
}

/// Find `name` (11-byte directory form, see [`short_name`]) in the root directory of the volume
/// and follow its chain. See the module comment for what is refused.
pub fn locate_in<S: Sectors>(
    src: &mut S,
    part: &Partition,
    vol: &Volume,
    name: &[u8; 11],
) -> Result<Located, Error> {
    let mut cache = FatCache {
        lba: u64::MAX,
        buf: [0; SECTOR],
    };
    let mut sec = [0u8; SECTOR];
    // The root directory: a chain of clusters, followed with a bound on the sectors read and on the
    // clusters visited (a loop in the directory's own chain cannot spin).
    let mut cluster = vol.root_cluster;
    let mut read = 0u32;
    let mut found: Option<(u32, u32)> = None;
    'dir: while read < MAX_DIR_SECTORS {
        let Some(first) = vol.cluster_lba(cluster) else {
            break;
        };
        for s in 0..vol.sectors_per_cluster {
            if read >= MAX_DIR_SECTORS {
                break 'dir;
            }
            read += 1;
            let lba = first + u64::from(s);
            if !src.read(lba, &mut sec) {
                return Err(Error::Read(lba));
            }
            #[allow(clippy::chunks_exact_to_as_chunks)]
            // directory entries of a runtime-length sector
            for e in sec.chunks_exact(32) {
                if e[0] == 0 {
                    break 'dir; // the end of the directory
                }
                if e[0] == 0xe5 {
                    continue; // deleted
                }
                if let Some(hit) = match_entry(e, name) {
                    found = Some(hit);
                    break 'dir;
                }
            }
        }
        let next = fat_entry(vol, src, &mut cache, cluster)?;
        if !vol.valid_cluster(next) {
            break; // the end of the chain (or garbage: the same thing here)
        }
        cluster = next;
    }
    let (first, size) = found.ok_or(Error::NoFile)?;
    let file = follow(src, vol, &mut cache, first, size)?;
    Ok(Located {
        partition: *part,
        volume: *vol,
        file,
    })
}

/// The chain of the file whose first cluster and size the directory says.
fn follow<S: Sectors>(
    src: &mut S,
    vol: &Volume,
    cache: &mut FatCache,
    first: u32,
    size: u32,
) -> Result<LogFile, Error> {
    if size < MIN_FILE_BYTES {
        return Err(Error::BadFile("the file is smaller than 4 KiB"));
    }
    let sectors = size.div_ceil(SECTOR as u32);
    if sectors > MAX_FILE_SECTORS {
        return Err(Error::BadFile("the file is larger than 4 MiB"));
    }
    let spc = vol.sectors_per_cluster;
    let needed = sectors.div_ceil(spc);
    let mut file = LogFile::empty();
    let mut c = first;
    for i in 0..needed {
        let Some(lba) = vol.cluster_lba(c) else {
            return Err(Error::BadFile("the chain leaves the volume's clusters"));
        };
        if file.holds(lba) {
            return Err(Error::BadFile("the chain visits a cluster twice (a loop)"));
        }
        file.push(lba, spc)?;
        let next = fat_entry(vol, src, cache, c)?;
        if i + 1 < needed {
            if is_end(next) {
                return Err(Error::BadFile("the chain is shorter than the file's size"));
            }
            if !vol.valid_cluster(next) {
                return Err(Error::BadFile(
                    "the chain runs through a free, bad or reserved cluster",
                ));
            }
            c = next;
        } else if !is_end(next) {
            return Err(Error::BadFile("the chain is longer than the file's size"));
        }
    }
    file.sectors = sectors;
    file.size = size;
    Ok(file)
}

/// Everything: the MBR, the boot sector, the root directory, the chain. `name` is the 8.3 name in
/// its 11-byte form ([`short_name`]).
pub fn locate<S: Sectors>(src: &mut S, name: &[u8; 11]) -> Result<Located, Error> {
    let mut sec = [0u8; SECTOR];
    if !src.read(0, &mut sec) {
        return Err(Error::Read(0));
    }
    let part = first_partition(&sec)?;
    if !src.read(part.start, &mut sec) {
        return Err(Error::Read(part.start));
    }
    let vol = Volume::parse(&sec, &part)?;
    locate_in(src, &part, &vol, name)
}
