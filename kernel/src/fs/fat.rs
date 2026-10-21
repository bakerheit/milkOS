//! FAT32 (read/write) with VFAT long file names, mounted at `/Volumes/<LABEL>`.
//!
//! Layout: boot sector (BPB) | reserved | FAT copies | data clusters (root dir = a cluster
//! chain on FAT32). The whole FAT is cached in RAM (4 bytes per cluster) and changes are
//! written through to every FAT copy at the end of each operation. Data and directories
//! are read and written a cluster at a time, straight to the block device.
//!
//! Names: long names are UTF-16 LFN entries in front of an 8.3 alias (`BASIS~N.EXT`,
//! generated like VFAT); names compare case-insensitively and keep their case. FAT has no
//! inodes: directories use their first cluster as inode number, files the position of
//! their directory entry (updated when renamed).
//!
//! Not supported: symlinks, hard links, permissions beyond the read-only bit, FAT12/16,
//! sector sizes other than 512. The FSInfo free count is set to "unknown" on first write.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;

use super::blockdev::BlockRef;
use super::errno::{Errno, Result};
use super::node::{
    DirEntry, FileSystem, Inode, InodeRef, InodeType, SetAttr, Stat, Timespec, alloc_dev,
    check_name, downcast,
};
use crate::kprintln;
use crate::sched::Mutex;
use crate::sync::SpinLock;

const SECTOR: usize = 512;
const EOC: u32 = 0x0FFF_FFFF;
const ATTR_RO: u8 = 0x01;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;
const FILE_INO_BASE: u64 = 1 << 40;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Boot-sector geometry of a FAT32 volume.
#[derive(Clone, Debug)]
pub struct Bpb {
    pub spc: u32,
    pub reserved: u32,
    pub nfats: u32,
    pub fat_size: u32,
    pub total: u32,
    pub root: u32,
    pub fsinfo: u32,
    pub label: String,
}

/// Is `dev` a FAT32 volume (512-byte sectors)? Returns its geometry.
pub fn probe(dev: &BlockRef) -> Option<Bpb> {
    if dev.block_size() != SECTOR {
        return None;
    }
    let mut b = [0u8; SECTOR];
    dev.read_blocks(0, &mut b).ok()?;
    if b[510] != 0x55 || b[511] != 0xAA || le16(&b, 11) as usize != SECTOR {
        return None;
    }
    let spc = b[13] as u32;
    let root_entries = le16(&b, 17);
    let fat16 = le16(&b, 22);
    let fat32 = le32(&b, 36);
    if spc == 0 || !spc.is_power_of_two() || root_entries != 0 || fat16 != 0 || fat32 == 0 {
        return None;
    }
    let total = if le16(&b, 19) != 0 {
        le16(&b, 19) as u32
    } else {
        le32(&b, 32)
    };
    let label = String::from_utf8_lossy(&b[71..82]).trim_end().into();
    Some(Bpb {
        spc,
        reserved: le16(&b, 14) as u32,
        nfats: b[16] as u32,
        fat_size: fat32,
        total,
        root: le32(&b, 44),
        fsinfo: le16(&b, 48) as u32,
        label,
    })
}

// ---- dates ------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn fat_to_unix(date: u16, time: u16) -> i64 {
    if date == 0 {
        return 0;
    }
    let (y, m, d) = (
        1980 + (date >> 9) as i64,
        ((date >> 5) & 15) as i64,
        (date & 31) as i64,
    );
    let (h, mi, s) = (
        (time >> 11) as i64,
        ((time >> 5) & 63) as i64,
        ((time & 31) * 2) as i64,
    );
    days_from_civil(y, m.max(1), d.max(1)) * 86400 + h * 3600 + mi * 60 + s
}

fn unix_to_fat(t: i64) -> (u16, u16) {
    let (y, mo, d, h, mi, s) = super::milkfs::timemachine::civil(t.max(315_532_800) as u64);
    let date = (((y.saturating_sub(1980)).min(127) as u16) << 9) | ((mo as u16) << 5) | d as u16;
    let time = ((h as u16) << 11) | ((mi as u16) << 5) | (s as u16 / 2);
    (date, time)
}

fn now_fat() -> (u16, u16) {
    unix_to_fat(super::clock::now().sec)
}

// ---- names ------------------------------------------------------------------------

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |s, &c| {
        ((s & 1) << 7).wrapping_add(s >> 1).wrapping_add(c)
    })
}

fn short_ok(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || b"!#$%&'()-@^_`{}~".contains(&c)
}

/// The 8.3 form of `name` if it is already a valid upper-case 8.3 name.
fn exact_short(name: &str) -> Option<[u8; 11]> {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || name == "." || name == ".." {
        return None;
    }
    if !base.bytes().chain(ext.bytes()).all(short_ok) {
        return None;
    }
    let mut s = [b' '; 11];
    s[..base.len()].copy_from_slice(base.as_bytes());
    s[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(s)
}

/// A unique `BASIS~N.EXT` alias for a long name.
fn alias(name: &str, taken: &BTreeSet<[u8; 11]>) -> [u8; 11] {
    let clean = |s: &str| -> Vec<u8> {
        s.chars()
            .filter(|c| *c != ' ' && *c != '.')
            .map(|c| {
                let u = c.to_ascii_uppercase() as u32;
                if u < 128 && short_ok(u as u8) {
                    u as u8
                } else {
                    b'_'
                }
            })
            .collect()
    };
    let trimmed = name.trim_start_matches('.');
    let (b, e) = match trimmed.rsplit_once('.') {
        Some((b, e)) if !b.is_empty() => (clean(b), clean(e)),
        _ => (clean(trimmed), Vec::new()),
    };
    let base: Vec<u8> = if b.is_empty() { b"FILE".to_vec() } else { b };
    for n in 1u32.. {
        let tail = format!("~{n}");
        let keep = base.len().min(8 - tail.len());
        let mut s = [b' '; 11];
        s[..keep].copy_from_slice(&base[..keep]);
        s[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        let el = e.len().min(3);
        s[8..8 + el].copy_from_slice(&e[..el]);
        if !taken.contains(&s) {
            return s;
        }
    }
    unreachable!()
}

/// `NAME    EXT` -> `NAME.EXT`, honouring the NT lower-case flags (byte 12).
fn short_to_string(s: &[u8], nt: u8) -> String {
    let lower = |part: &[u8], on: bool| -> String {
        let t = String::from_utf8_lossy(part);
        let t = t.trim_end();
        if on {
            t.to_ascii_lowercase()
        } else {
            String::from(t)
        }
    };
    let mut raw = [0u8; 11];
    raw.copy_from_slice(&s[..11]);
    if raw[0] == 0x05 {
        raw[0] = 0xE5;
    }
    let base = lower(&raw[..8], nt & 0x08 != 0);
    let ext = lower(&raw[8..11], nt & 0x10 != 0);
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

fn eq_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// One live directory entry, with where its pieces are on disk.
#[derive(Clone, Debug)]
struct Ent {
    name: String,
    short: [u8; 11],
    attr: u8,
    first: u32,
    size: u32,
    mdate: u16,
    mtime: u16,
    /// Absolute byte offset of the 8.3 entry.
    off: u64,
    /// Absolute byte offsets of its LFN entries.
    lfn: Vec<u64>,
}

impl Ent {
    fn kind(&self) -> InodeType {
        if self.attr & ATTR_DIR != 0 {
            InodeType::Dir
        } else {
            InodeType::File
        }
    }
}

// ---- the volume -------------------------------------------------------------------

struct FatState {
    fat: Vec<u32>,
    dirty: BTreeSet<u32>,
    hint: u32,
    fsinfo_unknown: bool,
}

pub struct FatVol {
    dev: BlockRef,
    bpb: Bpb,
    cluster_bytes: usize,
    data_start: u64,
    nclusters: u32,
    rw: bool,
    devno: u64,
    st: Mutex<FatState>,
    inodes: SpinLock<BTreeMap<u64, Weak<FatNode>>>,
    me: Weak<FatVol>,
}

impl FatVol {
    fn sector_of(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.bpb.spc as u64
    }

    fn valid(&self, c: u32) -> bool {
        c >= 2 && c < self.nclusters + 2
    }

    fn read_cluster(&self, c: u32, buf: &mut [u8]) -> Result<()> {
        if !self.valid(c) {
            return Err(Errno::EIO);
        }
        self.dev.read_blocks(self.sector_of(c), buf)
    }

    fn write_cluster(&self, c: u32, buf: &[u8]) -> Result<()> {
        if !self.valid(c) {
            return Err(Errno::EIO);
        }
        self.dev.write_blocks(self.sector_of(c), buf)
    }

    fn chain(&self, st: &FatState, first: u32) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        let mut c = first;
        while self.valid(c) {
            out.push(c);
            if out.len() > self.nclusters as usize {
                return Err(Errno::EIO); // loop
            }
            c = st.fat[c as usize] & 0x0FFF_FFFF;
        }
        Ok(out)
    }

    fn set_fat(&self, st: &mut FatState, c: u32, v: u32) {
        let e = &mut st.fat[c as usize];
        *e = (*e & 0xF000_0000) | (v & 0x0FFF_FFFF);
        st.dirty.insert(c * 4 / SECTOR as u32);
    }

    fn alloc_cluster(&self, st: &mut FatState) -> Result<u32> {
        let n = self.nclusters;
        for i in 0..n {
            let c = 2 + (st.hint - 2 + i) % n;
            if st.fat[c as usize] & 0x0FFF_FFFF == 0 {
                self.set_fat(st, c, EOC);
                st.hint = c + 1;
                if st.hint >= n + 2 {
                    st.hint = 2;
                }
                self.write_cluster(c, &vec![0u8; self.cluster_bytes])?;
                return Ok(c);
            }
        }
        Err(Errno::ENOSPC)
    }

    fn free_chain(&self, st: &mut FatState, first: u32) -> Result<()> {
        for c in self.chain(st, first)? {
            self.set_fat(st, c, 0);
        }
        Ok(())
    }

    /// Write dirty FAT sectors to every copy (and mark FSInfo's counts unknown once).
    fn flush_fat(&self, st: &mut FatState) -> Result<()> {
        for s in core::mem::take(&mut st.dirty) {
            let mut buf = [0u8; SECTOR];
            for (i, e) in buf.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let idx = s as usize * (SECTOR / 4) + i;
                let v = st.fat.get(idx).copied().unwrap_or(0);
                e.copy_from_slice(&v.to_le_bytes());
            }
            for f in 0..self.bpb.nfats {
                let sec = (self.bpb.reserved + f * self.bpb.fat_size + s) as u64;
                self.dev.write_blocks(sec, &buf)?;
            }
        }
        if !st.fsinfo_unknown && self.bpb.fsinfo != 0 && self.bpb.fsinfo != 0xFFFF {
            let mut b = [0u8; SECTOR];
            let sec = self.bpb.fsinfo as u64;
            self.dev.read_blocks(sec, &mut b)?;
            if le32(&b, 0) == 0x4161_5252 {
                b[488..496].fill(0xFF); // free count + next free: unknown
                self.dev.write_blocks(sec, &b)?;
            }
            st.fsinfo_unknown = true;
        }
        Ok(())
    }

    // ---- 32-byte directory entries ----

    fn read_raw(&self, off: u64) -> Result<[u8; 32]> {
        let mut s = [0u8; SECTOR];
        self.dev.read_blocks(off / SECTOR as u64, &mut s)?;
        let i = (off % SECTOR as u64) as usize;
        Ok(s[i..i + 32].try_into().unwrap())
    }

    fn write_raw(&self, off: u64, e: &[u8; 32]) -> Result<()> {
        let mut s = [0u8; SECTOR];
        let sec = off / SECTOR as u64;
        self.dev.read_blocks(sec, &mut s)?;
        let i = (off % SECTOR as u64) as usize;
        s[i..i + 32].copy_from_slice(e);
        self.dev.write_blocks(sec, &s)
    }

    /// Absolute offsets of every 32-byte slot of directory `dir` (in order).
    fn slots(&self, st: &FatState, dir: u32) -> Result<Vec<u64>> {
        let per = self.cluster_bytes / 32;
        let mut v = Vec::new();
        for c in self.chain(st, dir)? {
            let base = self.sector_of(c) * SECTOR as u64;
            v.extend((0..per).map(|i| base + i as u64 * 32));
        }
        Ok(v)
    }

    /// Live entries of directory `dir` (without `.` and `..` and the volume label).
    fn entries(&self, st: &FatState, dir: u32) -> Result<Vec<Ent>> {
        let mut out = Vec::new();
        let mut lfn: Vec<(u64, [u8; 32])> = Vec::new();
        let mut buf = vec![0u8; self.cluster_bytes];
        'outer: for c in self.chain(st, dir)? {
            self.read_cluster(c, &mut buf)?;
            let base = self.sector_of(c) * SECTOR as u64;
            for i in 0..self.cluster_bytes / 32 {
                let e: [u8; 32] = buf[i * 32..i * 32 + 32].try_into().unwrap();
                let off = base + i as u64 * 32;
                match e[0] {
                    0x00 => break 'outer,
                    0xE5 => {
                        lfn.clear();
                        continue;
                    }
                    _ => {}
                }
                if e[11] & 0x3F == ATTR_LFN {
                    lfn.push((off, e));
                    continue;
                }
                let short: [u8; 11] = e[..11].try_into().unwrap();
                let pieces = core::mem::take(&mut lfn);
                if e[11] & ATTR_VOLUME != 0 || &short == b".          " || &short == b"..         "
                {
                    continue;
                }
                let sum = lfn_checksum(&short);
                let long = if !pieces.is_empty() && pieces.iter().all(|(_, p)| p[13] == sum) {
                    let mut units: Vec<u16> = Vec::new();
                    for (_, p) in pieces.iter().rev() {
                        for &o in &[1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                            units.push(le16(p, o));
                        }
                    }
                    let end = units
                        .iter()
                        .position(|&u| u == 0 || u == 0xFFFF)
                        .unwrap_or(units.len());
                    Some(String::from_utf16_lossy(&units[..end]))
                } else {
                    None
                };
                out.push(Ent {
                    name: long.unwrap_or_else(|| short_to_string(&short, e[12])),
                    short,
                    attr: e[11],
                    first: (le16(&e, 20) as u32) << 16 | le16(&e, 26) as u32,
                    size: le32(&e, 28),
                    mdate: le16(&e, 24),
                    mtime: le16(&e, 22),
                    off,
                    lfn: if long_ok(&pieces, sum) {
                        pieces.iter().map(|(o, _)| *o).collect()
                    } else {
                        Vec::new()
                    },
                });
            }
        }
        Ok(out)
    }

    fn find(&self, st: &FatState, dir: u32, name: &str) -> Result<Option<Ent>> {
        Ok(self
            .entries(st, dir)?
            .into_iter()
            .find(|e| eq_name(&e.name, name)))
    }

    /// The parent directory cluster of `dir` (from its `..` entry).
    fn parent_of(&self, st: &FatState, dir: u32) -> Result<u32> {
        if dir == self.bpb.root {
            return Ok(dir);
        }
        let mut buf = vec![0u8; self.cluster_bytes];
        let first = *self.chain(st, dir)?.first().ok_or(Errno::EIO)?;
        self.read_cluster(first, &mut buf)?;
        let e = &buf[32..64];
        if &e[..2] != b".." {
            return Err(Errno::EIO);
        }
        let c = (le16(e, 20) as u32) << 16 | le16(e, 26) as u32;
        Ok(if c == 0 { self.bpb.root } else { c })
    }

    /// Write a new entry (LFN pieces + 8.3) for `name` in `dir`. Returns its 8.3 offset.
    fn add_entry(
        &self,
        st: &mut FatState,
        dir: u32,
        name: &str,
        attr: u8,
        first: u32,
        size: u32,
    ) -> Result<u64> {
        check_name(name)?;
        if name
            .chars()
            .any(|c| "\\/:*?\"<>|".contains(c) || (c as u32) < 0x20)
        {
            return Err(Errno::EINVAL);
        }
        let existing = self.entries(st, dir)?;
        if existing.iter().any(|e| eq_name(&e.name, name)) {
            return Err(Errno::EEXIST);
        }
        let (short, long) = match exact_short(name) {
            Some(s) => (s, false),
            None => {
                let taken: BTreeSet<[u8; 11]> = existing.iter().map(|e| e.short).collect();
                (alias(name, &taken), true)
            }
        };
        let units: Vec<u16> = name.encode_utf16().collect();
        if long && units.len() > 255 {
            return Err(Errno::ENAMETOOLONG);
        }
        let nlfn = if long { units.len().div_ceil(13) } else { 0 };
        let need = nlfn + 1;
        // A run of `need` free slots, growing the directory if there is none.
        let run = loop {
            let slots = self.slots(st, dir)?;
            let mut free_run = Vec::new();
            let mut found = None;
            for &off in &slots {
                let b0 = self.read_raw(off)?[0];
                if b0 == 0x00 || b0 == 0xE5 {
                    free_run.push(off);
                    if free_run.len() == need {
                        found = Some(free_run.clone());
                        break;
                    }
                } else {
                    free_run.clear();
                }
            }
            if let Some(r) = found {
                break r;
            }
            let last = *self.chain(st, dir)?.last().ok_or(Errno::EIO)?;
            let c = self.alloc_cluster(st)?;
            self.set_fat(st, last, c);
        };
        let sum = lfn_checksum(&short);
        for (k, &slot) in run.iter().enumerate().take(nlfn) {
            let seq = nlfn - k; // on disk: last piece first
            let mut e = [0u8; 32];
            e[0] = seq as u8 | if k == 0 { 0x40 } else { 0 };
            e[11] = ATTR_LFN;
            e[13] = sum;
            for (j, &o) in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
                .iter()
                .enumerate()
            {
                let i = (seq - 1) * 13 + j;
                let u = match i.cmp(&units.len()) {
                    core::cmp::Ordering::Less => units[i],
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 0xFFFF,
                };
                e[o..o + 2].copy_from_slice(&u.to_le_bytes());
            }
            self.write_raw(slot, &e)?;
        }
        let (d, t) = now_fat();
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(&short);
        e[11] = attr;
        e[14..16].copy_from_slice(&t.to_le_bytes());
        e[16..18].copy_from_slice(&d.to_le_bytes());
        e[18..20].copy_from_slice(&d.to_le_bytes());
        e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        e[22..24].copy_from_slice(&t.to_le_bytes());
        e[24..26].copy_from_slice(&d.to_le_bytes());
        e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        self.write_raw(run[nlfn], &e)?;
        Ok(run[nlfn])
    }

    fn remove_entry(&self, e: &Ent) -> Result<()> {
        for &o in e.lfn.iter().chain(core::iter::once(&e.off)) {
            let mut raw = self.read_raw(o)?;
            raw[0] = 0xE5;
            self.write_raw(o, &raw)?;
        }
        Ok(())
    }

    /// Update first cluster / size / mtime of the 8.3 entry at `off`.
    fn update_entry(&self, off: u64, first: u32, size: u32) -> Result<()> {
        let mut e = self.read_raw(off)?;
        let (d, t) = now_fat();
        e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        e[22..24].copy_from_slice(&t.to_le_bytes());
        e[24..26].copy_from_slice(&d.to_le_bytes());
        self.write_raw(off, &e)
    }

    fn arc(&self) -> Arc<FatVol> {
        self.me.upgrade().expect("fat: volume gone")
    }

    fn dir_obj(&self, cluster: u32) -> Arc<FatNode> {
        self.obj(cluster as u64, InodeType::Dir, cluster, 0, None)
    }

    fn file_obj(&self, e: &Ent) -> Arc<FatNode> {
        if e.kind() == InodeType::Dir {
            let d = self.dir_obj(e.first);
            d.loc.lock().dirent = Some(e.off);
            return d;
        }
        self.obj(
            FILE_INO_BASE + e.off / 32,
            InodeType::File,
            e.first,
            e.size,
            Some(e.off),
        )
    }

    fn obj(
        &self,
        ino: u64,
        kind: InodeType,
        first: u32,
        size: u32,
        dirent: Option<u64>,
    ) -> Arc<FatNode> {
        let mut m = self.inodes.lock();
        if let Some(n) = m.get(&ino).and_then(|w| w.upgrade()) {
            return n;
        }
        if m.len() > 1024 {
            m.retain(|_, w| w.strong_count() > 0);
        }
        let n = Arc::new(FatNode {
            vol: self.arc(),
            ino: SpinLock::new(ino),
            kind,
            loc: SpinLock::new(Loc {
                first,
                size,
                dirent,
            }),
            cursor: SpinLock::new((0, 0)),
        });
        m.insert(ino, Arc::downgrade(&n));
        n
    }

    fn ro(&self) -> Result<()> {
        if self.rw { Ok(()) } else { Err(Errno::EROFS) }
    }
}

fn long_ok(pieces: &[(u64, [u8; 32])], sum: u8) -> bool {
    !pieces.is_empty() && pieces.iter().all(|(_, p)| p[13] == sum)
}

impl FileSystem for FatVol {
    fn name(&self) -> &'static str {
        "vfat"
    }
    fn dev(&self) -> u64 {
        self.devno
    }
    fn root(&self) -> InodeRef {
        self.dir_obj(self.bpb.root)
    }
    fn sync(&self) -> Result<()> {
        self.dev.flush()
    }
}

#[derive(Clone, Copy, Debug)]
struct Loc {
    first: u32,
    size: u32,
    /// Where the 8.3 entry is (None: root).
    dirent: Option<u64>,
}

pub struct FatNode {
    vol: Arc<FatVol>,
    ino: SpinLock<u64>,
    kind: InodeType,
    loc: SpinLock<Loc>,
    /// Last (cluster index, cluster) a read reached, so sequential reads don't walk the
    /// chain from the start every time. Reset by anything that changes the chain.
    cursor: SpinLock<(usize, u32)>,
}

impl FatNode {
    fn dir_cluster(&self) -> Result<u32> {
        if self.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        Ok(self.loc.lock().first)
    }

    /// Make sure the file has at least `clusters` clusters; returns the chain.
    fn grow(&self, st: &mut FatState, clusters: usize) -> Result<Vec<u32>> {
        let v = &self.vol;
        let mut loc = *self.loc.lock();
        let mut chain = if loc.first == 0 {
            Vec::new()
        } else {
            v.chain(st, loc.first)?
        };
        while chain.len() < clusters {
            let c = v.alloc_cluster(st)?;
            match chain.last() {
                Some(&last) => v.set_fat(st, last, c),
                None => loc.first = c,
            }
            chain.push(c);
        }
        self.loc.lock().first = loc.first;
        Ok(chain)
    }

    fn read_raw(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        let v = &self.vol;
        let st = v.st.lock();
        let loc = *self.loc.lock();
        if off >= loc.size as u64 {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(loc.size as u64 - off) as usize;
        let cb = v.cluster_bytes;
        let (first_ci, last_ci) = (off as usize / cb, (off as usize + n - 1) / cb);
        // Find cluster `first_ci`, starting from the sequential-read cursor when it helps.
        let (mut ci, mut c) = match *self.cursor.lock() {
            (i, cl) if i <= first_ci && cl != 0 && loc.first != 0 => (i, cl),
            _ => (0, loc.first),
        };
        while ci < first_ci {
            c = st.fat[c as usize] & 0x0FFF_FFFF;
            if !v.valid(c) {
                return Err(Errno::EIO);
            }
            ci += 1;
        }
        // The clusters we need, then read each run of consecutive ones in one request.
        let mut clusters = alloc::vec![c];
        while ci < last_ci {
            c = st.fat[c as usize] & 0x0FFF_FFFF;
            if !v.valid(c) {
                return Err(Errno::EIO);
            }
            clusters.push(c);
            ci += 1;
        }
        *self.cursor.lock() = (last_ci, c);
        let mut tmp = Vec::new();
        let mut done = 0;
        let mut i = 0;
        while i < clusters.len() {
            let mut j = i + 1;
            while j < clusters.len() && clusters[j] == clusters[j - 1] + 1 {
                j += 1;
            }
            tmp.resize((j - i) * cb, 0);
            v.dev.read_blocks(v.sector_of(clusters[i]), &mut tmp)?;
            let run_start = (first_ci + i) * cb; // file offset of the run
            let from = (off as usize + done) - run_start;
            let take = (tmp.len() - from).min(n - done);
            buf[done..done + take].copy_from_slice(&tmp[from..from + take]);
            done += take;
            i = j;
        }
        Ok(n)
    }

    fn write_raw(&self, off: u64, data: &[u8]) -> Result<usize> {
        if self.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        let v = &self.vol;
        v.ro()?;
        let end = off.checked_add(data.len() as u64).ok_or(Errno::EFBIG)?;
        if end > u32::MAX as u64 {
            return Err(Errno::EFBIG);
        }
        let mut st = v.st.lock();
        let cb = v.cluster_bytes;
        let old = self.loc.lock().size as u64;
        let chain = self.grow(&mut st, (end as usize).div_ceil(cb))?;
        // FAT has no holes: anything between the old end and `off` must read as zeros.
        // New clusters are zeroed by alloc_cluster; zero the tail of the old last one.
        let mut cbuf = vec![0u8; cb];
        if off > old && !old.is_multiple_of(cb as u64) {
            let c = chain[(old / cb as u64) as usize];
            v.read_cluster(c, &mut cbuf)?;
            let from = (old % cb as u64) as usize;
            let to = if off / cb as u64 == old / cb as u64 {
                (off % cb as u64) as usize
            } else {
                cb
            };
            cbuf[from..to].fill(0);
            v.write_cluster(c, &cbuf)?;
        }
        let mut done = 0;
        while done < data.len() {
            let pos = off as usize + done;
            let (ci, inc) = (pos / cb, pos % cb);
            let chunk = (cb - inc).min(data.len() - done);
            let c = chain[ci];
            if chunk < cb {
                v.read_cluster(c, &mut cbuf)?;
            }
            cbuf[inc..inc + chunk].copy_from_slice(&data[done..done + chunk]);
            v.write_cluster(c, &cbuf)?;
            done += chunk;
        }
        {
            let mut l = self.loc.lock();
            l.size = l.size.max(end as u32);
        }
        self.save()?;
        v.flush_fat(&mut st)?;
        Ok(data.len())
    }

    fn truncate_raw(&self, len: u64) -> Result<()> {
        if self.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        let v = &self.vol;
        v.ro()?;
        if len > u32::MAX as u64 {
            return Err(Errno::EFBIG);
        }
        let mut st = v.st.lock();
        let cb = v.cluster_bytes;
        let loc = *self.loc.lock();
        if len < loc.size as u64 {
            let keep = (len as usize).div_ceil(cb);
            let chain = if loc.first == 0 {
                Vec::new()
            } else {
                v.chain(&st, loc.first)?
            };
            if keep == 0 {
                if loc.first != 0 {
                    v.free_chain(&mut st, loc.first)?;
                }
                self.loc.lock().first = 0;
            } else if keep < chain.len() {
                v.free_chain(&mut st, chain[keep])?;
                v.set_fat(&mut st, chain[keep - 1], EOC);
            }
            self.loc.lock().size = len as u32;
        } else if len > loc.size as u64 {
            drop(st);
            // Growing: write zeros (FAT has no sparse files).
            let zeros = vec![0u8; (len - loc.size as u64) as usize];
            return self.write_raw(loc.size as u64, &zeros).map(|_| ());
        }
        self.save()?;
        v.flush_fat(&mut st)
    }

    fn save(&self) -> Result<()> {
        let loc = *self.loc.lock();
        if let Some(off) = loc.dirent
            && self.kind == InodeType::File
        {
            self.vol.update_entry(off, loc.first, loc.size)?;
        }
        Ok(())
    }
}

impl Inode for FatNode {
    fn ino(&self) -> u64 {
        *self.ino.lock()
    }
    fn dev(&self) -> u64 {
        self.vol.devno
    }
    fn kind(&self) -> InodeType {
        self.kind
    }

    fn stat(&self) -> Stat {
        let loc = *self.loc.lock();
        let (mut attr, mut mt) = (0u8, 0i64);
        if let Some(off) = loc.dirent
            && let Ok(e) = self.vol.read_raw(off)
        {
            attr = e[11];
            mt = fat_to_unix(le16(&e, 24), le16(&e, 22));
        }
        let perm = match (self.kind, attr & ATTR_RO != 0) {
            (InodeType::Dir, _) => 0o755,
            (_, true) => 0o444,
            _ => 0o644,
        };
        let mut s = Stat::new(self.vol.devno, self.ino(), self.kind, perm);
        s.nlink = if self.kind == InodeType::Dir { 2 } else { 1 };
        s.size = if self.kind == InodeType::Dir {
            self.vol.cluster_bytes as u64
        } else {
            loc.size as u64
        };
        s.blksize = self.vol.cluster_bytes as u32;
        s.blocks = s.size.div_ceil(512);
        let t = Timespec { sec: mt, nsec: 0 };
        s.atime = t;
        s.mtime = t;
        s.ctime = t;
        s
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        if self.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        let size = self.loc.lock().size as u64;
        let (dev, ino) = (self.vol.devno, self.ino());
        super::pagecache::read(dev, ino, size, off, buf, |pg, page| {
            self.read_raw(pg * super::pagecache::PAGE as u64, page)
                .map(|_| ())
        })
    }

    fn write_at(&self, off: u64, data: &[u8]) -> Result<usize> {
        *self.cursor.lock() = (0, 0);
        let r = self.write_raw(off, data);
        super::pagecache::invalidate(self.vol.devno, self.ino());
        r
    }

    fn truncate(&self, len: u64) -> Result<()> {
        *self.cursor.lock() = (0, 0);
        let r = self.truncate_raw(len);
        super::pagecache::invalidate(self.vol.devno, self.ino());
        r
    }

    fn lookup(&self, name: &str) -> Result<InodeRef> {
        let dir = self.dir_cluster()?;
        let v = &self.vol;
        let st = v.st.lock();
        if name == ".." {
            let p = v.parent_of(&st, dir)?;
            drop(st);
            return Ok(v.dir_obj(p));
        }
        let e = v.find(&st, dir, name)?.ok_or(Errno::ENOENT)?;
        drop(st);
        Ok(v.file_obj(&e))
    }

    fn readdir(&self) -> Result<Vec<DirEntry>> {
        let dir = self.dir_cluster()?;
        let st = self.vol.st.lock();
        Ok(self
            .vol
            .entries(&st, dir)?
            .into_iter()
            .map(|e| DirEntry {
                ino: if e.kind() == InodeType::Dir {
                    e.first as u64
                } else {
                    FILE_INO_BASE + e.off / 32
                },
                kind: e.kind(),
                name: e.name,
            })
            .collect())
    }

    fn create(&self, name: &str, _perm: u32) -> Result<InodeRef> {
        let dir = self.dir_cluster()?;
        let v = &self.vol;
        v.ro()?;
        let mut st = v.st.lock();
        let off = v.add_entry(&mut st, dir, name, ATTR_ARCHIVE, 0, 0)?;
        v.flush_fat(&mut st)?;
        drop(st);
        // Entry slots are reused: drop whatever an older file in this slot left cached.
        super::pagecache::invalidate(v.devno, FILE_INO_BASE + off / 32);
        Ok(v.obj(FILE_INO_BASE + off / 32, InodeType::File, 0, 0, Some(off)))
    }

    fn mkdir(&self, name: &str, _perm: u32) -> Result<InodeRef> {
        let dir = self.dir_cluster()?;
        let v = &self.vol;
        v.ro()?;
        let mut st = v.st.lock();
        let c = v.alloc_cluster(&mut st)?;
        // `.` and `..` (the root is cluster 0 in `..`).
        let (d, t) = now_fat();
        let mut buf = vec![0u8; v.cluster_bytes];
        let parent = if dir == v.bpb.root { 0 } else { dir };
        for (i, (nm, cl)) in [(b".          ", c), (b"..         ", parent)]
            .iter()
            .enumerate()
        {
            let e = &mut buf[i * 32..i * 32 + 32];
            e[..11].copy_from_slice(*nm);
            e[11] = ATTR_DIR;
            e[20..22].copy_from_slice(&((cl >> 16) as u16).to_le_bytes());
            e[22..24].copy_from_slice(&t.to_le_bytes());
            e[24..26].copy_from_slice(&d.to_le_bytes());
            e[26..28].copy_from_slice(&(*cl as u16).to_le_bytes());
        }
        v.write_cluster(c, &buf)?;
        match v.add_entry(&mut st, dir, name, ATTR_DIR, c, 0) {
            Ok(off) => {
                v.flush_fat(&mut st)?;
                drop(st);
                let n = v.dir_obj(c);
                n.loc.lock().dirent = Some(off);
                Ok(n)
            }
            Err(e) => {
                v.set_fat(&mut st, c, 0);
                v.flush_fat(&mut st)?;
                Err(e)
            }
        }
    }

    fn unlink(&self, name: &str) -> Result<()> {
        let dir = self.dir_cluster()?;
        let v = &self.vol;
        v.ro()?;
        let mut st = v.st.lock();
        let e = v.find(&st, dir, name)?.ok_or(Errno::ENOENT)?;
        if e.kind() == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        v.remove_entry(&e)?;
        if e.first != 0 {
            v.free_chain(&mut st, e.first)?;
        }
        v.flush_fat(&mut st)?;
        drop(st);
        super::pagecache::invalidate(v.devno, FILE_INO_BASE + e.off / 32);
        // An open file loses its data with the entry (FAT has no link counts).
        if let Some(n) = v
            .inodes
            .lock()
            .get(&(FILE_INO_BASE + e.off / 32))
            .and_then(|w| w.upgrade())
        {
            *n.loc.lock() = Loc {
                first: 0,
                size: 0,
                dirent: None,
            };
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<()> {
        let dir = self.dir_cluster()?;
        let v = &self.vol;
        v.ro()?;
        let mut st = v.st.lock();
        let e = v.find(&st, dir, name)?.ok_or(Errno::ENOENT)?;
        if e.kind() != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        if !v.entries(&st, e.first)?.is_empty() {
            return Err(Errno::ENOTEMPTY);
        }
        v.remove_entry(&e)?;
        v.free_chain(&mut st, e.first)?;
        v.flush_fat(&mut st)
    }

    fn rename(&self, old: &str, new_dir: &InodeRef, new: &str) -> Result<()> {
        let od = self.dir_cluster()?;
        let v = &self.vol;
        v.ro()?;
        let nd_node = downcast::<FatNode>(new_dir).ok_or(Errno::EXDEV)?;
        if !Arc::ptr_eq(&nd_node.vol, v) {
            return Err(Errno::EXDEV);
        }
        let nd = nd_node.dir_cluster()?;
        let mut st = v.st.lock();
        let src = v.find(&st, od, old)?.ok_or(Errno::ENOENT)?;
        if let Some(dst) = v.find(&st, nd, new)? {
            if dst.off == src.off {
                if dst.name == new {
                    return Ok(());
                }
                // Same entry, different case: rewrite the name below.
            } else {
                match (src.kind(), dst.kind()) {
                    (InodeType::Dir, InodeType::File) => return Err(Errno::ENOTDIR),
                    (InodeType::File, InodeType::Dir) => return Err(Errno::EISDIR),
                    (InodeType::Dir, InodeType::Dir) if !v.entries(&st, dst.first)?.is_empty() => {
                        return Err(Errno::ENOTEMPTY);
                    }
                    _ => {}
                }
                v.remove_entry(&dst)?;
                if dst.first != 0 {
                    v.free_chain(&mut st, dst.first)?;
                }
            }
        }
        if src.kind() == InodeType::Dir {
            // Not into its own subtree.
            let mut c = nd;
            loop {
                if c == src.first {
                    return Err(Errno::EINVAL);
                }
                if c == v.bpb.root {
                    break;
                }
                c = v.parent_of(&st, c)?;
            }
        }
        v.remove_entry(&src)?;
        let off = v.add_entry(&mut st, nd, new, src.attr, src.first, src.size)?;
        if src.kind() == InodeType::Dir && od != nd {
            // Point the moved directory's `..` at its new parent.
            let first = src.first;
            let mut buf = vec![0u8; v.cluster_bytes];
            v.read_cluster(first, &mut buf)?;
            let p = if nd == v.bpb.root { 0 } else { nd };
            buf[32 + 20..32 + 22].copy_from_slice(&((p >> 16) as u16).to_le_bytes());
            buf[32 + 26..32 + 28].copy_from_slice(&(p as u16).to_le_bytes());
            v.write_cluster(first, &buf)?;
        }
        v.flush_fat(&mut st)?;
        drop(st);
        super::pagecache::invalidate(v.devno, FILE_INO_BASE + src.off / 32);
        super::pagecache::invalidate(v.devno, FILE_INO_BASE + off / 32);
        // Keep an open file's object pointing at its entry.
        if src.kind() == InodeType::File {
            let (old_ino, new_ino) = (FILE_INO_BASE + src.off / 32, FILE_INO_BASE + off / 32);
            let mut m = v.inodes.lock();
            if let Some(n) = m.remove(&old_ino).and_then(|w| w.upgrade()) {
                n.loc.lock().dirent = Some(off);
                *n.ino.lock() = new_ino;
                m.insert(new_ino, Arc::downgrade(&n));
            }
        } else if let Some(n) = v
            .inodes
            .lock()
            .get(&(src.first as u64))
            .and_then(|w| w.upgrade())
        {
            n.loc.lock().dirent = Some(off);
        }
        Ok(())
    }

    fn symlink(&self, _name: &str, _target: &str) -> Result<InodeRef> {
        Err(Errno::EPERM)
    }

    fn link(&self, _name: &str, _target: &InodeRef) -> Result<()> {
        Err(Errno::EPERM)
    }

    fn setattr(&self, a: &SetAttr) -> Result<()> {
        self.vol.ro()?;
        let loc = *self.loc.lock();
        let Some(off) = loc.dirent else { return Ok(()) };
        let mut e = self.vol.read_raw(off)?;
        if let Some(p) = a.perm {
            if p & 0o222 == 0 {
                e[11] |= ATTR_RO;
            } else {
                e[11] &= !ATTR_RO;
            }
        }
        if let Some(t) = a.mtime {
            let (d, tm) = unix_to_fat(t.sec);
            e[22..24].copy_from_slice(&tm.to_le_bytes());
            e[24..26].copy_from_slice(&d.to_le_bytes());
        }
        self.vol.write_raw(off, &e) // uid/gid: FAT has none
    }

    fn sync(&self) -> Result<()> {
        self.vol.dev.flush()
    }
}

/// Mount the FAT32 volume on `dev` at `path`.
pub fn mount(dev: BlockRef, path: &str) -> Result<Arc<FatVol>> {
    let bpb = probe(&dev).ok_or(Errno::EINVAL)?;
    let data_start = (bpb.reserved + bpb.nfats * bpb.fat_size) as u64;
    let nclusters = ((bpb.total as u64 - data_start) / bpb.spc as u64) as u32;
    if (nclusters as usize + 2) > bpb.fat_size as usize * SECTOR / 4 {
        return Err(Errno::EINVAL); // FAT too small for the volume
    }
    let mut fat = vec![0u32; bpb.fat_size as usize * SECTOR / 4];
    let mut buf = vec![0u8; 64 * SECTOR];
    let mut s = 0usize;
    while s < bpb.fat_size as usize {
        let n = 64.min(bpb.fat_size as usize - s);
        dev.read_blocks((bpb.reserved as usize + s) as u64, &mut buf[..n * SECTOR])?;
        for i in 0..n * SECTOR / 4 {
            fat[s * SECTOR / 4 + i] = le32(&buf, i * 4);
        }
        s += n;
    }
    let rw = !dev.read_only();
    let vol = Arc::new_cyclic(|me| FatVol {
        cluster_bytes: bpb.spc as usize * SECTOR,
        data_start,
        nclusters,
        rw,
        devno: alloc_dev(),
        st: Mutex::new(FatState {
            fat,
            dirty: BTreeSet::new(),
            hint: 2,
            fsinfo_unknown: false,
        }),
        inodes: SpinLock::new(BTreeMap::new()),
        me: me.clone(),
        dev,
        bpb,
    });
    super::ops::mkdir_all(path, 0o755)?;
    super::path::mount(path, vol.clone())?;
    Ok(vol)
}

/// The label a FAT volume on `dev` would mount under (`/Volumes/<label>`).
pub fn label_of(dev: &BlockRef) -> Option<String> {
    let bpb = probe(dev)?;
    let data_start = (bpb.reserved + bpb.nfats * bpb.fat_size) as u64;
    let mut buf = vec![0u8; bpb.spc as usize * SECTOR];
    let mut label = bpb.label.clone();
    if dev
        .read_blocks(
            data_start + (bpb.root as u64 - 2) * bpb.spc as u64,
            &mut buf,
        )
        .is_ok()
    {
        for e in buf.as_chunks::<32>().0 {
            if e[0] == 0 {
                break;
            }
            if e[0] != 0xE5 && e[11] & 0x3F != ATTR_LFN && e[11] & ATTR_VOLUME != 0 {
                label = String::from_utf8_lossy(&e[..11]).trim_end().into();
                break;
            }
        }
    }
    let clean: String = label
        .chars()
        .filter(|c| !c.is_control() && *c != '/')
        .collect();
    Some(if clean.is_empty() || clean == "NO NAME" {
        String::from("FAT")
    } else {
        clean
    })
}

/// Mount every FAT32 disk at `/Volumes/<LABEL>`.
pub fn automount() {
    for dev in super::blockdev::devices() {
        if super::milkfs::probe(&dev).is_some() {
            continue;
        }
        let Some(label) = label_of(&dev) else {
            continue;
        };
        let path = format!("/Volumes/{label}");
        match mount(dev.clone(), &path) {
            Ok(v) => kprintln!(
                "fat: {} (FAT32, {} clusters of {} B) mounted at {path}{}",
                dev.name(),
                v.nclusters,
                v.cluster_bytes,
                if v.rw { "" } else { " read-only" }
            ),
            Err(e) => kprintln!("fat: mounting {} failed: {e}", dev.name()),
        }
    }
}
