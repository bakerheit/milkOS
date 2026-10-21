//! `snap send` / `snap receive`: a snapshot as a byte stream (ZFS-send style), full or
//! incremental from an older snapshot, and recreating it on another milkfs volume.
//!
//! The stream is a sequence of records, each `type u8 | 3 x 0 | length u32 | payload |
//! xxh64 u64` (the checksum covers type, length and payload). The first is the header
//! (magic, version, names, times), the last `END` carries the number of records and data
//! blocks and a chained hash of every record checksum, so a truncated, reordered or
//! damaged stream is refused before anything becomes visible. Layout in
//! `docs/filesystem.md`; `scripts/mkmilkfs.py verify-stream` / `receive` read it too.
//!
//! What is sent is tree items, not blocks: inodes, directory entries, symlinks, inline
//! data as they are; file data as `EXTENT` records (file range + the sender's block
//! numbers and the xxh64 of each block's plaintext, so an encrypted sender's keyed tags
//! never leave it), each data block once as a `DATA` record before its first use,
//! so reflink copies stay shared on the receiver. Data removal is by file range (`DROP`),
//! so the receiver may lay its blocks out differently. An incremental stream holds only
//! the items that differ (found like `snap diff`, reading only changed nodes): removals
//! first, then changes in key order. Changed data runs are sent whole. Not sent: files
//! with no links left (unlinked while open), the orphan list, the shared-block table (the
//! receiver recounts it).
//!
//! Receive is atomic: the records are applied to a hidden clone (`.recv`) of the base
//! snapshot (for a full stream, of a snapshot of the empty target), and only when the
//! `END` record checks out does the clone become the live tree (`clone_fast_forward`) and
//! the snapshot get recorded, with the sender's name and creation time. A crash or a bad
//! stream leaves the target as it was (a leftover `.recv` clone is discarded by the next
//! receive). The target's live tree must still be the base snapshot's (nothing written
//! since); `--force` rolls it back to the base first, like `zfs receive -F`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use super::btree::{self, Child};
use super::format::*;
use super::items::{LIVE, clone_view};
use super::snap::{SnapInfo, check_snap_name};
use super::volume::State;
use crate::fs::errno::{Errno, Result};

pub const STREAM_MAGIC: &[u8; 8] = b"MILKSTRM";
pub const STREAM_VERSION: u32 = 1;

pub const R_HEADER: u8 = 1;
pub const R_ITEM: u8 = 2;
pub const R_DELETE: u8 = 3;
pub const R_DROP: u8 = 4;
pub const R_DATA: u8 = 5;
pub const R_EXTENT: u8 = 6;
pub const R_END: u8 = 7;
/// Largest record payload accepted.
pub const RECORD_MAX: usize = 64 << 10;
const HEADER_LEN: usize = 200;

/// Name of the hidden clone a receive builds in, and of the snapshot of the empty target
/// a full receive starts from.
pub const RECV_CLONE: &str = ".recv";
pub const RECV_BASE: &str = ".recv-base";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamHeader {
    pub incremental: bool,
    /// Sending volume's uuid and label (informational).
    pub uuid: [u8; 16],
    pub label: String,
    pub snap: String,
    pub snap_created: u64,
    pub snap_gen: u64,
    /// Incremental: the base snapshot, found on the receiver by name + creation time.
    pub base: String,
    pub base_created: u64,
    pub base_gen: u64,
    pub next_ino: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Header(StreamHeader),
    /// Set an item (not file data).
    Item(Key, Vec<u8>),
    /// Remove an item (not file data).
    Delete(Key),
    /// Remove file blocks `from..to` of `ino`.
    Drop {
        ino: u64,
        from: u64,
        to: u64,
    },
    /// Contents of the sender's block `src`.
    Data {
        src: u64,
        data: Vec<u8>,
    },
    /// File blocks `start..start + csums.len()` of `ino` are the sender's blocks
    /// `src..`; whatever the receiver had in `start..max(end, old_end)` goes first.
    Extent {
        ino: u64,
        start: u64,
        old_end: u64,
        src: u64,
        csums: Vec<u64>,
    },
    End {
        records: u64,
        data: u64,
        chain: u64,
    },
}

fn put_name(b: &mut Vec<u8>, s: &str, n: usize) {
    let mut f = alloc::vec![0u8; n];
    let m = s.len().min(n - 1);
    f[..m].copy_from_slice(&s.as_bytes()[..m]);
    b.extend_from_slice(&f);
}

fn get_name(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn chain_step(chain: u64, csum: u64) -> u64 {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&chain.to_le_bytes());
    b[8..].copy_from_slice(&csum.to_le_bytes());
    xxh64(&b)
}

impl StreamHeader {
    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_LEN);
        b.extend_from_slice(STREAM_MAGIC);
        b.extend_from_slice(&STREAM_VERSION.to_le_bytes());
        b.extend_from_slice(&(self.incremental as u32).to_le_bytes());
        b.extend_from_slice(&self.uuid);
        put_name(&mut b, &self.label, 32);
        put_name(&mut b, &self.snap, 48);
        b.extend_from_slice(&self.snap_created.to_le_bytes());
        b.extend_from_slice(&self.snap_gen.to_le_bytes());
        put_name(&mut b, &self.base, 48);
        b.extend_from_slice(&self.base_created.to_le_bytes());
        b.extend_from_slice(&self.base_gen.to_le_bytes());
        b.extend_from_slice(&self.next_ino.to_le_bytes());
        b
    }

    fn decode(b: &[u8]) -> Result<StreamHeader> {
        if b.len() != HEADER_LEN || &b[..8] != STREAM_MAGIC {
            return Err(Errno::EINVAL); // not a milkfs stream
        }
        if le32(&b[8..]) != STREAM_VERSION {
            return Err(Errno::EINVAL); // a stream format this kernel doesn't know
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&b[16..32]);
        Ok(StreamHeader {
            incremental: le32(&b[12..]) & 1 != 0,
            uuid,
            label: get_name(&b[32..64]),
            snap: get_name(&b[64..112]),
            snap_created: le64(&b[112..]),
            snap_gen: le64(&b[120..]),
            base: get_name(&b[128..176]),
            base_created: le64(&b[176..]),
            base_gen: le64(&b[184..]),
            next_ino: le64(&b[192..]),
        })
    }
}

/// Builds a stream in memory; the caller writes `buf` out and clears it as it goes.
#[derive(Default)]
pub struct Encoder {
    pub buf: Vec<u8>,
    pub records: u64,
    pub data_blocks: u64,
    pub bytes: u64,
    chain: u64,
}

impl Encoder {
    fn rec(&mut self, t: u8, payload: &[u8]) {
        let start = self.buf.len();
        self.buf.push(t);
        self.buf.extend_from_slice(&[0; 3]);
        self.buf
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(payload);
        let c = xxh64(&self.buf[start..]);
        self.buf.extend_from_slice(&c.to_le_bytes());
        self.bytes += (self.buf.len() - start) as u64;
        if t != R_END {
            self.chain = chain_step(self.chain, c);
            self.records += 1;
        }
    }

    pub fn record(&mut self, r: &Record) {
        let mut p = Vec::new();
        let key = |p: &mut Vec<u8>, k: &Key| {
            let mut b = [0u8; KEY_SIZE];
            k.put(&mut b);
            p.extend_from_slice(&b);
        };
        let t = match r {
            Record::Header(h) => {
                p = h.encode();
                R_HEADER
            }
            Record::Item(k, v) => {
                key(&mut p, k);
                p.extend_from_slice(v);
                R_ITEM
            }
            Record::Delete(k) => {
                key(&mut p, k);
                R_DELETE
            }
            Record::Drop { ino, from, to } => {
                for x in [ino, from, to] {
                    p.extend_from_slice(&x.to_le_bytes());
                }
                R_DROP
            }
            Record::Data { src, data } => {
                p.extend_from_slice(&src.to_le_bytes());
                p.extend_from_slice(data);
                self.data_blocks += 1;
                R_DATA
            }
            Record::Extent {
                ino,
                start,
                old_end,
                src,
                csums,
            } => {
                for x in [ino, start, old_end, src] {
                    p.extend_from_slice(&x.to_le_bytes());
                }
                p.extend_from_slice(&(csums.len() as u32).to_le_bytes());
                for c in csums {
                    p.extend_from_slice(&c.to_le_bytes());
                }
                R_EXTENT
            }
            Record::End { .. } => {
                for x in [self.records, self.data_blocks, self.chain] {
                    p.extend_from_slice(&x.to_le_bytes());
                }
                R_END
            }
        };
        self.rec(t, &p);
    }
}

/// Reads records from `read` (fills part of a buffer, 0 = end), checking every
/// checksum and, at `END`, the counts and the chained hash.
pub struct Decoder<R: FnMut(&mut [u8]) -> Result<usize>> {
    read: R,
    pub records: u64,
    pub data_blocks: u64,
    pub bytes: u64,
    chain: u64,
}

impl<R: FnMut(&mut [u8]) -> Result<usize>> Decoder<R> {
    pub fn new(read: R) -> Self {
        Decoder {
            read,
            records: 0,
            data_blocks: 0,
            bytes: 0,
            chain: 0,
        }
    }

    fn fill(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut got = 0;
        while got < buf.len() {
            let n = (self.read)(&mut buf[got..])?;
            if n == 0 {
                return Err(Errno::EIO); // truncated stream
            }
            got += n;
        }
        self.bytes += buf.len() as u64;
        Ok(())
    }

    pub fn next(&mut self) -> Result<Record> {
        let mut h = [0u8; 8];
        self.fill(&mut h)?;
        let (t, len) = (h[0], le32(&h[4..]) as usize);
        if len > RECORD_MAX || h[1..4] != [0; 3] {
            return Err(Errno::EIO);
        }
        let mut rec = alloc::vec![0u8; 8 + len + 8];
        rec[..8].copy_from_slice(&h);
        self.fill(&mut rec[8..])?;
        let c = le64(&rec[8 + len..]);
        if xxh64(&rec[..8 + len]) != c {
            return Err(Errno::EIO); // damaged record
        }
        let p = &rec[8..8 + len];
        let key = |b: &[u8]| -> Result<Key> {
            if b.len() < KEY_SIZE {
                return Err(Errno::EIO);
            }
            Ok(Key::get(b))
        };
        let r = match t {
            R_HEADER => Record::Header(StreamHeader::decode(p)?),
            R_ITEM => Record::Item(key(p)?, p[KEY_SIZE..].to_vec()),
            R_DELETE if len == KEY_SIZE => Record::Delete(key(p)?),
            R_DROP if len == 24 => Record::Drop {
                ino: le64(p),
                from: le64(&p[8..]),
                to: le64(&p[16..]),
            },
            R_DATA if len == 8 + BLOCK => {
                self.data_blocks += 1;
                Record::Data {
                    src: le64(p),
                    data: p[8..].to_vec(),
                }
            }
            R_EXTENT if len >= 36 => {
                let n = le32(&p[32..]) as usize;
                if n == 0 || n as u64 > MAX_RUN || len != 36 + 8 * n {
                    return Err(Errno::EIO);
                }
                Record::Extent {
                    ino: le64(p),
                    start: le64(&p[8..]),
                    old_end: le64(&p[16..]),
                    src: le64(&p[24..]),
                    csums: (0..n).map(|i| le64(&p[36 + 8 * i..])).collect(),
                }
            }
            R_END if len == 24 => {
                let (records, data, chain) = (le64(p), le64(&p[8..]), le64(&p[16..]));
                if records != self.records || data != self.data_blocks || chain != self.chain {
                    return Err(Errno::EIO); // records missing, reordered or replaced
                }
                return Ok(Record::End {
                    records,
                    data,
                    chain,
                });
            }
            _ => return Err(Errno::EIO),
        };
        self.chain = chain_step(self.chain, c);
        self.records += 1;
        Ok(r)
    }
}

// ---- send ---------------------------------------------------------------------------

/// What a send will emit, worked out under the volume lock; data is read later, a run
/// at a time.
pub enum Plan {
    Item(Key, Vec<u8>),
    Delete(Key),
    Drop(u64, u64, u64),
    Extent { ino: u64, old_end: u64, ext: Ext },
}

pub struct SendPlan {
    pub header: StreamHeader,
    pub recs: Vec<Plan>,
    /// Snapshots the send reads (kept from deletion until it is done).
    pub gens: Vec<u64>,
}

fn removal(recs: &mut Vec<Plan>, k: &Key, v: &[u8]) {
    match Ext::decode(k, v) {
        Some(e) => recs.push(Plan::Drop(k.obj, e.start, e.end())),
        None => recs.push(Plan::Delete(*k)),
    }
}

fn addition(recs: &mut Vec<Plan>, k: Key, v: Vec<u8>, old_end: u64) {
    match Ext::decode(&k, &v) {
        Some(ext) => recs.push(Plan::Extent {
            ino: k.obj,
            old_end,
            ext,
        }),
        None => recs.push(Plan::Item(k, v)),
    }
}

impl State {
    /// Work out the stream for snapshot `snap`, incremental from `from` (an older one).
    pub fn send_plan(&mut self, snap: &str, from: Option<&str>) -> Result<SendPlan> {
        self.commit()?;
        let find = |n: &str| {
            self.snaps
                .iter()
                .find(|s| s.name == n)
                .cloned()
                .ok_or(Errno::ENOENT)
        };
        let s = find(snap)?;
        let b = from.map(find).transpose()?;
        if b.as_ref().is_some_and(|b| b.generation >= s.generation) {
            return Err(Errno::EINVAL); // the base must be older
        }
        let header = StreamHeader {
            incremental: b.is_some(),
            uuid: self.sb.uuid,
            label: self.sb.label.clone(),
            snap: s.name.clone(),
            snap_created: s.created,
            snap_gen: s.generation,
            base: b.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
            base_created: b.as_ref().map_or(0, |b| b.created),
            base_gen: b.as_ref().map_or(0, |b| b.generation),
            next_ino: s.next_ino,
        };
        let mut recs = Vec::new();
        let unlinked = |i: Result<InodeItem>| i.is_ok_and(|i| i.nlink == 0);
        match &b {
            None => {
                let mut items: Vec<(Key, Vec<u8>)> = Vec::new();
                btree::walk(&self.store, s.root, &mut |_| true, &mut |k, v| {
                    items.push((*k, v.to_vec()));
                    Ok(())
                })?;
                let gone: BTreeSet<u64> = items
                    .iter()
                    .filter(|(k, v)| k.kind == K_INODE && unlinked(InodeItem::decode(v)))
                    .map(|(k, _)| k.obj)
                    .collect();
                for (k, v) in items {
                    if k.obj != VOL_OBJ && !gone.contains(&k.obj) {
                        addition(&mut recs, k, v, 0);
                    }
                }
            }
            Some(b) => {
                let (old, new) = self.tree_delta(b.root, b.generation, s.root, &mut 0, &mut 0)?;
                // Files with no links left in the new snapshot aren't sent; if the
                // receiver has them from the base, everything of theirs goes.
                let objs: BTreeSet<u64> = new
                    .keys()
                    .chain(old.keys())
                    .map(|k| k.obj)
                    .filter(|&o| o != VOL_OBJ)
                    .collect();
                let gone: BTreeSet<u64> = objs
                    .into_iter()
                    .filter(|&o| unlinked(self.inode(s.generation, o)))
                    .collect();
                for (k, v) in &old {
                    if k.obj != VOL_OBJ && !gone.contains(&k.obj) && !new.contains_key(k) {
                        removal(&mut recs, k, v);
                    }
                }
                for &o in &gone {
                    if self.inode(b.generation, o).is_ok_and(|i| i.nlink > 0) {
                        let mut items = Vec::new();
                        btree::range(
                            &Child::Disk(b.root),
                            &self.store,
                            &Key::new(o, 0, 0),
                            &Key::new(o + 1, 0, 0),
                            &mut items,
                        )?;
                        for (k, v) in items {
                            removal(&mut recs, &k, &v);
                        }
                    }
                }
                for (k, v) in new {
                    if k.obj == VOL_OBJ || gone.contains(&k.obj) || old.get(&k) == Some(&v) {
                        continue;
                    }
                    let old_end = old
                        .get(&k)
                        .and_then(|ov| Ext::decode(&k, ov))
                        .map_or(0, |e| e.end());
                    addition(&mut recs, k, v, old_end);
                }
            }
        }
        let mut gens = alloc::vec![s.generation];
        gens.extend(b.map(|b| b.generation));
        Ok(SendPlan { header, recs, gens })
    }
}

// ---- receive ------------------------------------------------------------------------

/// A receive in progress.
pub struct RecvCtx {
    pub view: u64,
    /// Full stream: we made the `.recv-base` snapshot (deleted at the end).
    pub temp_base: bool,
    pub base: String,
    /// Sender's block -> (our block, checksum, birth).
    /// Sender's block -> (our block, xxh64 of the plaintext, birth, our checksum).
    map: BTreeMap<u64, (u64, u64, u64, u64)>,
    /// Our blocks written by DATA records and not yet used by an EXTENT (freed if the
    /// receive fails; commits only happen when this is empty).
    pub unused: BTreeSet<u64>,
    /// Origin blocks the clone dropped so far (see `Store::recv_drops`).
    drops: Vec<DeadEntry>,
    max_obj: u64,
    pub items: u64,
    pub blocks: u64,
}

impl State {
    /// Check the target and set up the hidden clone the stream is applied to.
    pub fn recv_begin(&mut self, h: &StreamHeader, force: bool) -> Result<RecvCtx> {
        if self.receiving {
            return Err(Errno::EBUSY);
        }
        self.set_cur(LIVE)?;
        self.commit()?;
        check_snap_name(&h.snap)?;
        if h.snap.starts_with(".recv") {
            return Err(Errno::EINVAL);
        }
        if self.name_taken(&h.snap) {
            return Err(Errno::EEXIST);
        }
        // Leftovers of a receive that was interrupted.
        if self.clones.iter().any(|c| c.name == RECV_CLONE) {
            self.clone_discard(RECV_CLONE)?;
        }
        if self.snaps.iter().any(|s| s.name == RECV_BASE) {
            self.snap_delete(RECV_BASE)?;
        }
        let (base, temp_base) = if h.incremental {
            let b = self
                .snaps
                .iter()
                .find(|s| s.name == h.base && s.created == h.base_created)
                .cloned()
                .ok_or(Errno::ENOENT)?; // the base snapshot isn't here
            (b, false)
        } else {
            if !self.list(LIVE, ROOT_INO)?.is_empty() {
                return Err(Errno::ENOTEMPTY); // a full stream needs an empty volume
            }
            self.snap_create(RECV_BASE)?;
            (self.snaps.last().cloned().ok_or(Errno::EIO)?, true)
        };
        let ready = if self.sb.root != base.root || !self.deadlist.is_none() {
            if force {
                self.snap_rollback(&base.name)
            } else {
                Err(Errno::EBUSY) // written since the base snapshot
            }
        } else {
            Ok(())
        };
        let id = ready.and_then(|_| self.clone_create(&base.name, RECV_CLONE));
        let id = match id {
            Ok(id) => id,
            Err(e) => {
                if temp_base {
                    let _ = self.snap_delete(RECV_BASE);
                }
                return Err(e);
            }
        };
        self.receiving = true;
        Ok(RecvCtx {
            view: clone_view(id),
            temp_base,
            base: base.name,
            map: BTreeMap::new(),
            unused: BTreeSet::new(),
            drops: Vec::new(),
            max_obj: 0,
            items: 0,
            blocks: 0,
        })
    }

    /// Apply records to the receive's clone.
    pub fn recv_apply(&mut self, ctx: &mut RecvCtx, recs: Vec<Record>) -> Result<()> {
        self.set_cur(ctx.view)?;
        self.store.recv_drops = Some(core::mem::take(&mut ctx.drops));
        let r = (|| {
            for r in recs {
                self.recv_one(ctx, r)?;
            }
            Ok(())
        })();
        ctx.drops = self.store.recv_drops.take().unwrap_or_default();
        let _ = self.set_cur(LIVE);
        r
    }

    fn recv_one(&mut self, ctx: &mut RecvCtx, r: Record) -> Result<()> {
        let data_kind = |k: &Key| matches!(k.kind, K_EXT | K_EXTENT);
        match r {
            Record::Item(k, v) => {
                if k.obj == VOL_OBJ || data_kind(&k) || v.len() > MAX_VALUE {
                    return Err(Errno::EIO);
                }
                self.put(k, v)?;
                ctx.max_obj = ctx.max_obj.max(k.obj);
                ctx.items += 1;
            }
            Record::Delete(k) => {
                if k.obj == VOL_OBJ || data_kind(&k) {
                    return Err(Errno::EIO);
                }
                self.del(&k)?;
            }
            Record::Drop { ino, from, to } => self.drop_range(ino, from, to)?,
            Record::Data { src, data } => {
                if ctx.map.contains_key(&src) {
                    return Err(Errno::EIO);
                }
                let b = self.store.alloc()?;
                let mut blk: [u8; BLOCK] = data.as_slice().try_into().map_err(|_| Errno::EIO)?;
                let plain = xxh64(&blk);
                // Ours: sealed with this volume's key if it is encrypted.
                let csum = self.store.seal(b, self.store.txn, &mut blk);
                self.store.cache.write(b, &blk)?;
                ctx.map.insert(src, (b, plain, self.store.txn, csum));
                ctx.unused.insert(b);
                ctx.blocks += 1;
            }
            Record::Extent {
                ino,
                start,
                old_end,
                src,
                csums,
            } => {
                let n = csums.len() as u64;
                self.drop_range(ino, start, (start + n).max(old_end))?;
                let mut i = 0u64;
                while i < n {
                    let get = |j: u64| -> Result<(u64, u64, u64, u64)> {
                        let m = *ctx.map.get(&(src + j)).ok_or(Errno::EIO)?;
                        if m.1 != csums[j as usize] {
                            return Err(Errno::EIO); // data doesn't match the extent
                        }
                        Ok(m)
                    };
                    let (b0, _, birth, _) = get(i)?;
                    let mut j = i + 1;
                    while j < n {
                        let (b, _, br, _) = get(j)?;
                        if b != b0 + (j - i) || br != birth {
                            break;
                        }
                        j += 1;
                    }
                    for k in i..j {
                        ctx.unused.remove(&(b0 + k - i));
                    }
                    let e = Ext {
                        start: start + i,
                        block: b0,
                        birth,
                        csums: (i..j)
                            .map(|k| ctx.map.get(&(src + k)).map_or(0, |m| m.3))
                            .collect(),
                        legacy: false,
                    };
                    self.put(e.key(ino), e.encode())?;
                    i = j;
                }
                ctx.max_obj = ctx.max_obj.max(ino);
                ctx.items += 1;
            }
            Record::Header(_) | Record::End { .. } => return Err(Errno::EIO),
        }
        Ok(())
    }

    /// The stream checked out: recount shared blocks, make the clone the live tree and
    /// record the snapshot. On failure everything is undone (`recv_abort`).
    pub fn recv_finish(&mut self, ctx: &mut RecvCtx, h: &StreamHeader) -> Result<SnapInfo> {
        let r = (|| {
            self.set_cur(ctx.view)?;
            self.store.recv_drops = Some(core::mem::take(&mut ctx.drops));
            let r = self.rebuild_brefs();
            let _ = self.set_cur(LIVE);
            r?;
            self.clone_fast_forward(RECV_CLONE)?;
            self.next_ino = self.next_ino.max(h.next_ino).max(ctx.max_obj + 1);
            let info = self.snap_create_at(&h.snap, Some(h.snap_created))?;
            if ctx.temp_base {
                self.snap_delete(RECV_BASE)?;
            }
            Ok(info)
        })();
        if r.is_err() {
            self.recv_abort(ctx);
        }
        self.receiving = false;
        r
    }

    /// Something went wrong: throw the clone (and a temporary base) away.
    pub fn recv_abort(&mut self, ctx: &RecvCtx) {
        self.store.recv_drops = None;
        let _ = self.set_cur(LIVE);
        for &b in &ctx.unused {
            self.store.free_after_commit(b, self.store.txn);
        }
        if self.clones.iter().any(|c| c.name == RECV_CLONE) {
            let _ = self.clone_discard(RECV_CLONE);
        }
        if ctx.temp_base && self.snaps.iter().any(|s| s.name == RECV_BASE) {
            let _ = self.snap_delete(RECV_BASE);
        }
        self.receiving = false;
    }
}
