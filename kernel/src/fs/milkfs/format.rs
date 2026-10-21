//! milkfs v0 on-disk format. Everything little endian, 4 KiB blocks. Must stay in sync
//! with `scripts/mkmilkfs.py`; the layout is documented in `docs/filesystem.md`.
//!
//! ```text
//! block 0, 1   superblock slots; generation g lives in slot g % 2, mount takes the valid
//!              one with the higher generation (the superblock write is the commit point)
//! block 2..    B+tree nodes, data blocks, dead-list blocks, allocated anywhere, never
//!              overwritten while the last committed superblock can reach them
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::errno::{Errno, Result};

pub const BLOCK: usize = 4096;
pub const MAGIC: &[u8; 8] = b"MILKFS01";
pub const VERSION: u32 = 0;
pub const NODE_MAGIC: &[u8; 4] = b"MKND";
pub const DEAD_MAGIC: &[u8; 4] = b"MKDL";

pub const MAX_SNAPS: usize = 30;
pub const SNAP_NAME_MAX: usize = 47;
pub const LABEL_MAX: usize = 31;

/// Item kinds (the `kind` byte of a key).
pub const K_INODE: u8 = 1;
pub const K_DIRENT: u8 = 2;
pub const K_EXTENT: u8 = 3;
pub const K_SYMLINK: u8 = 4;

/// Root directory inode number.
pub const ROOT_INO: u64 = 1;
/// Largest value stored in one leaf item (keeps a split leaf within a block).
pub const MAX_VALUE: usize = 1500;
pub const SYMLINK_MAX: usize = 1024;

// ---- xxh64 (Yann Collet's XXH64, seed 0) --------------------------------------------

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(acc: u64, val: u64) -> u64 {
    (acc ^ round(0, val)).wrapping_mul(P1).wrapping_add(P4)
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}

pub fn xxh64(data: &[u8]) -> u64 {
    let len = data.len();
    let mut p = 0;
    let mut h;
    if len >= 32 {
        let mut v = [P1.wrapping_add(P2), P2, 0, 0u64.wrapping_sub(P1)];
        while p + 32 <= len {
            for (i, vi) in v.iter_mut().enumerate() {
                *vi = round(*vi, le64(&data[p + i * 8..]));
            }
            p += 32;
        }
        h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        for vi in v {
            h = merge(h, vi);
        }
    } else {
        h = P5;
    }
    h = h.wrapping_add(len as u64);
    while p + 8 <= len {
        h ^= round(0, le64(&data[p..]));
        h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        p += 8;
    }
    if p + 4 <= len {
        h ^= (le32(&data[p..]) as u64).wrapping_mul(P1);
        h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        p += 4;
    }
    while p < len {
        h ^= (data[p] as u64).wrapping_mul(P5);
        h = h.rotate_left(11).wrapping_mul(P1);
        p += 1;
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^= h >> 32;
    h
}

/// FNV-1a 64, the directory-entry name hash.
pub fn name_hash(name: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in name {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- block pointers and keys --------------------------------------------------------

/// `{ block, birth generation, xxh64 of the block }`, 24 bytes. `block == 0` means none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockPtr {
    pub block: u64,
    pub birth: u64,
    pub csum: u64,
}

pub const PTR_SIZE: usize = 24;

impl BlockPtr {
    pub const NONE: BlockPtr = BlockPtr {
        block: 0,
        birth: 0,
        csum: 0,
    };

    pub fn is_none(&self) -> bool {
        self.block == 0
    }

    pub fn put(&self, b: &mut [u8]) {
        b[0..8].copy_from_slice(&self.block.to_le_bytes());
        b[8..16].copy_from_slice(&self.birth.to_le_bytes());
        b[16..24].copy_from_slice(&self.csum.to_le_bytes());
    }

    pub fn get(b: &[u8]) -> BlockPtr {
        BlockPtr {
            block: le64(b),
            birth: le64(&b[8..]),
            csum: le64(&b[16..]),
        }
    }
}

/// B+tree key: (object id, kind, offset), sorted in that order. 17 bytes on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub obj: u64,
    pub kind: u8,
    pub off: u64,
}

pub const KEY_SIZE: usize = 17;

impl Key {
    pub const MIN: Key = Key {
        obj: 0,
        kind: 0,
        off: 0,
    };

    pub const fn new(obj: u64, kind: u8, off: u64) -> Key {
        Key { obj, kind, off }
    }

    pub fn put(&self, b: &mut [u8]) {
        b[0..8].copy_from_slice(&self.obj.to_le_bytes());
        b[8] = self.kind;
        b[9..17].copy_from_slice(&self.off.to_le_bytes());
    }

    pub fn get(b: &[u8]) -> Key {
        Key {
            obj: le64(b),
            kind: b[8],
            off: le64(&b[9..]),
        }
    }
}

// ---- superblock ---------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct SnapRecord {
    pub name: String,
    /// Generation of the commit the snapshot captured.
    pub generation: u64,
    /// Unix seconds.
    pub created: u64,
    pub root: BlockPtr,
    /// Blocks born before the previous snapshot that died between it and this one.
    pub deadlist: BlockPtr,
    pub next_ino: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Superblock {
    pub total_blocks: u64,
    pub generation: u64,
    pub root: BlockPtr,
    /// Live head's dead list: blocks the live tree dropped that the newest snapshot keeps.
    pub deadlist: BlockPtr,
    pub next_ino: u64,
    pub created: u64,
    pub uuid: [u8; 16],
    pub label: String,
    /// Oldest first (ascending generation).
    pub snaps: Vec<SnapRecord>,
}

// Superblock byte offsets.
const SB_VERSION: usize = 8;
const SB_BSIZE: usize = 12;
const SB_TOTAL: usize = 16;
const SB_GEN: usize = 24;
const SB_ROOT: usize = 32;
const SB_DEAD: usize = 56;
const SB_NEXT_INO: usize = 80;
const SB_CREATED: usize = 88;
const SB_UUID: usize = 96;
const SB_LABEL: usize = 112;
const SB_NSNAPS: usize = 144;
const SB_SNAPS: usize = 152;
const SNAP_REC: usize = 128;
const SB_CSUM: usize = BLOCK - 8;

const _: () = assert!(SB_SNAPS + MAX_SNAPS * SNAP_REC <= SB_CSUM);

fn put_str(b: &mut [u8], s: &str) {
    b.fill(0);
    let n = s.len().min(b.len() - 1);
    b[..n].copy_from_slice(&s.as_bytes()[..n]);
}

fn get_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

impl Superblock {
    pub fn encode(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        b[..8].copy_from_slice(MAGIC);
        b[SB_VERSION..SB_VERSION + 4].copy_from_slice(&VERSION.to_le_bytes());
        b[SB_BSIZE..SB_BSIZE + 4].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        b[SB_TOTAL..SB_TOTAL + 8].copy_from_slice(&self.total_blocks.to_le_bytes());
        b[SB_GEN..SB_GEN + 8].copy_from_slice(&self.generation.to_le_bytes());
        self.root.put(&mut b[SB_ROOT..]);
        self.deadlist.put(&mut b[SB_DEAD..]);
        b[SB_NEXT_INO..SB_NEXT_INO + 8].copy_from_slice(&self.next_ino.to_le_bytes());
        b[SB_CREATED..SB_CREATED + 8].copy_from_slice(&self.created.to_le_bytes());
        b[SB_UUID..SB_UUID + 16].copy_from_slice(&self.uuid);
        put_str(&mut b[SB_LABEL..SB_LABEL + LABEL_MAX + 1], &self.label);
        b[SB_NSNAPS..SB_NSNAPS + 4].copy_from_slice(&(self.snaps.len() as u32).to_le_bytes());
        for (i, s) in self.snaps.iter().enumerate() {
            let r = &mut b[SB_SNAPS + i * SNAP_REC..SB_SNAPS + (i + 1) * SNAP_REC];
            put_str(&mut r[0..48], &s.name);
            r[48..56].copy_from_slice(&s.generation.to_le_bytes());
            r[56..64].copy_from_slice(&s.created.to_le_bytes());
            s.root.put(&mut r[64..]);
            s.deadlist.put(&mut r[88..]);
            r[112..120].copy_from_slice(&s.next_ino.to_le_bytes());
        }
        let c = xxh64(&b[..SB_CSUM]);
        b[SB_CSUM..].copy_from_slice(&c.to_le_bytes());
        b
    }

    /// Parse and validate one superblock slot. `None` if it is not a valid milkfs v0 one.
    pub fn decode(b: &[u8; BLOCK]) -> Option<Superblock> {
        if &b[..8] != MAGIC || le64(&b[SB_CSUM..]) != xxh64(&b[..SB_CSUM]) {
            return None;
        }
        if le32(&b[SB_VERSION..]) != VERSION || le32(&b[SB_BSIZE..]) != BLOCK as u32 {
            return None;
        }
        let n = le32(&b[SB_NSNAPS..]) as usize;
        if n > MAX_SNAPS {
            return None;
        }
        let snaps = (0..n)
            .map(|i| {
                let r = &b[SB_SNAPS + i * SNAP_REC..SB_SNAPS + (i + 1) * SNAP_REC];
                SnapRecord {
                    name: get_str(&r[0..48]),
                    generation: le64(&r[48..]),
                    created: le64(&r[56..]),
                    root: BlockPtr::get(&r[64..]),
                    deadlist: BlockPtr::get(&r[88..]),
                    next_ino: le64(&r[112..]),
                }
            })
            .collect();
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&b[SB_UUID..SB_UUID + 16]);
        Some(Superblock {
            total_blocks: le64(&b[SB_TOTAL..]),
            generation: le64(&b[SB_GEN..]),
            root: BlockPtr::get(&b[SB_ROOT..]),
            deadlist: BlockPtr::get(&b[SB_DEAD..]),
            next_ino: le64(&b[SB_NEXT_INO..]),
            created: le64(&b[SB_CREATED..]),
            uuid,
            label: get_str(&b[SB_LABEL..SB_LABEL + LABEL_MAX + 1]),
            snaps,
        })
    }
}

// ---- tree nodes ---------------------------------------------------------------------
//
// Header (16 bytes): magic "MKND", level u8 (0 = leaf), pad u8, count u16, birth u64.
// Leaf entry:     key (17) | value length u16 | value
// Internal entry: key (17) | child BlockPtr (24)
// A node's checksum is in the pointer to it, not in the node.

pub const NODE_HEADER: usize = 16;
pub const LEAF_ITEM_OVERHEAD: usize = KEY_SIZE + 2;
pub const INTERNAL_ENTRY: usize = KEY_SIZE + PTR_SIZE;

pub enum DiskNode {
    Leaf(Vec<(Key, Vec<u8>)>),
    Internal(u8, Vec<(Key, BlockPtr)>),
}

pub fn encode_leaf(items: &[(Key, Vec<u8>)], birth: u64, out: &mut [u8; BLOCK]) {
    out.fill(0);
    out[..4].copy_from_slice(NODE_MAGIC);
    out[4] = 0;
    out[6..8].copy_from_slice(&(items.len() as u16).to_le_bytes());
    out[8..16].copy_from_slice(&birth.to_le_bytes());
    let mut p = NODE_HEADER;
    for (k, v) in items {
        k.put(&mut out[p..]);
        out[p + KEY_SIZE..p + KEY_SIZE + 2].copy_from_slice(&(v.len() as u16).to_le_bytes());
        out[p + LEAF_ITEM_OVERHEAD..p + LEAF_ITEM_OVERHEAD + v.len()].copy_from_slice(v);
        p += LEAF_ITEM_OVERHEAD + v.len();
    }
}

pub fn encode_internal(level: u8, kids: &[(Key, BlockPtr)], birth: u64, out: &mut [u8; BLOCK]) {
    out.fill(0);
    out[..4].copy_from_slice(NODE_MAGIC);
    out[4] = level;
    out[6..8].copy_from_slice(&(kids.len() as u16).to_le_bytes());
    out[8..16].copy_from_slice(&birth.to_le_bytes());
    let mut p = NODE_HEADER;
    for (k, ptr) in kids {
        k.put(&mut out[p..]);
        ptr.put(&mut out[p + KEY_SIZE..]);
        p += INTERNAL_ENTRY;
    }
}

pub fn decode_node(b: &[u8; BLOCK]) -> Result<DiskNode> {
    if &b[..4] != NODE_MAGIC {
        return Err(Errno::EIO);
    }
    let level = b[4];
    let n = le16(&b[6..]) as usize;
    let mut p = NODE_HEADER;
    if level == 0 {
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            if p + LEAF_ITEM_OVERHEAD > BLOCK {
                return Err(Errno::EIO);
            }
            let k = Key::get(&b[p..]);
            let len = le16(&b[p + KEY_SIZE..]) as usize;
            let v = b
                .get(p + LEAF_ITEM_OVERHEAD..p + LEAF_ITEM_OVERHEAD + len)
                .ok_or(Errno::EIO)?;
            items.push((k, v.to_vec()));
            p += LEAF_ITEM_OVERHEAD + len;
        }
        Ok(DiskNode::Leaf(items))
    } else {
        if NODE_HEADER + n * INTERNAL_ENTRY > BLOCK {
            return Err(Errno::EIO);
        }
        let kids = (0..n)
            .map(|i| {
                let q = NODE_HEADER + i * INTERNAL_ENTRY;
                (Key::get(&b[q..]), BlockPtr::get(&b[q + KEY_SIZE..]))
            })
            .collect();
        Ok(DiskNode::Internal(level, kids))
    }
}

// ---- dead-list blocks ---------------------------------------------------------------
//
// magic "MKDL" | count u32 | next BlockPtr (24) | count x (block u64, birth u64)
// Chains are only ever prepended to or rewritten whole, never modified in place.

pub const DEAD_HEADER: usize = 32;
pub const DEAD_PER_BLOCK: usize = (BLOCK - DEAD_HEADER) / 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeadEntry {
    pub block: u64,
    pub birth: u64,
}

pub fn encode_dead(entries: &[DeadEntry], next: BlockPtr, out: &mut [u8; BLOCK]) {
    out.fill(0);
    out[..4].copy_from_slice(DEAD_MAGIC);
    out[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    next.put(&mut out[8..]);
    for (i, e) in entries.iter().enumerate() {
        let p = DEAD_HEADER + i * 16;
        out[p..p + 8].copy_from_slice(&e.block.to_le_bytes());
        out[p + 8..p + 16].copy_from_slice(&e.birth.to_le_bytes());
    }
}

pub fn decode_dead(b: &[u8; BLOCK]) -> Result<(Vec<DeadEntry>, BlockPtr)> {
    if &b[..4] != DEAD_MAGIC {
        return Err(Errno::EIO);
    }
    let n = le32(&b[4..]) as usize;
    if n > DEAD_PER_BLOCK {
        return Err(Errno::EIO);
    }
    let next = BlockPtr::get(&b[8..]);
    let entries = (0..n)
        .map(|i| {
            let p = DEAD_HEADER + i * 16;
            DeadEntry {
                block: le64(&b[p..]),
                birth: le64(&b[p + 8..]),
            }
        })
        .collect();
    Ok((entries, next))
}

// ---- items --------------------------------------------------------------------------

/// `K_INODE` value, 68 bytes: mode u32 (S_IF* | perm), nlink u32, uid u32, gid u32,
/// size u64, parent u64 (directories: the containing directory; others 0), then
/// atime, mtime, ctime as (i64 seconds, u32 nanoseconds).
#[derive(Clone, Copy, Debug, Default)]
pub struct InodeItem {
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub parent: u64,
    pub atime: (i64, u32),
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
}

pub const INODE_SIZE: usize = 68;

impl InodeItem {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(INODE_SIZE);
        v.extend_from_slice(&self.mode.to_le_bytes());
        v.extend_from_slice(&self.nlink.to_le_bytes());
        v.extend_from_slice(&self.uid.to_le_bytes());
        v.extend_from_slice(&self.gid.to_le_bytes());
        v.extend_from_slice(&self.size.to_le_bytes());
        v.extend_from_slice(&self.parent.to_le_bytes());
        for (s, ns) in [self.atime, self.mtime, self.ctime] {
            v.extend_from_slice(&s.to_le_bytes());
            v.extend_from_slice(&ns.to_le_bytes());
        }
        v
    }

    pub fn decode(b: &[u8]) -> Result<InodeItem> {
        if b.len() < INODE_SIZE {
            return Err(Errno::EIO);
        }
        let t = |o: usize| (le64(&b[o..]) as i64, le32(&b[o + 8..]));
        Ok(InodeItem {
            mode: le32(b),
            nlink: le32(&b[4..]),
            uid: le32(&b[8..]),
            gid: le32(&b[12..]),
            size: le64(&b[16..]),
            parent: le64(&b[24..]),
            atime: t(32),
            mtime: t(44),
            ctime: t(56),
        })
    }
}

/// One name in a `K_DIRENT` item. The item for (dir, hash) holds every name in `dir`
/// with that FNV-1a hash (normally one): repeated `ino u64 | d_type u8 | len u8 | name`.
#[derive(Clone, Debug)]
pub struct Dirent {
    pub ino: u64,
    pub dtype: u8,
    pub name: String,
}

pub fn encode_dirents(list: &[Dirent]) -> Vec<u8> {
    let mut v = Vec::new();
    for d in list {
        v.extend_from_slice(&d.ino.to_le_bytes());
        v.push(d.dtype);
        v.push(d.name.len() as u8);
        v.extend_from_slice(d.name.as_bytes());
    }
    v
}

pub fn decode_dirents(b: &[u8]) -> Result<Vec<Dirent>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < b.len() {
        if p + 10 > b.len() {
            return Err(Errno::EIO);
        }
        let ino = le64(&b[p..]);
        let dtype = b[p + 8];
        let n = b[p + 9] as usize;
        let name = b.get(p + 10..p + 10 + n).ok_or(Errno::EIO)?;
        out.push(Dirent {
            ino,
            dtype,
            name: String::from_utf8_lossy(name).into_owned(),
        });
        p += 10 + n;
    }
    Ok(out)
}
