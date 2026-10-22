//! The on-disk format is the kernel's, byte for byte:
//! - `mkfs` here writes exactly the image main's kernel wrote (a fixture of it, with its
//!   clock and UUID seed replayed);
//! - every structure on a busy volume decodes and encodes back to the same bytes;
//! - an image another milkfs wrote (`MILKFS_COMPAT_IMAGE=path`, e.g. one main's kernel
//!   filled with its crash workload) mounts, checks clean, every file verifies, every
//!   structure round-trips, and it takes new writes; `MILKFS_EXPORT_IMAGE=path` writes an
//!   image this crate filled, for the other direction (main's kernel mounting it).

use std::collections::BTreeSet;
use std::string::String;
use std::vec::Vec;

use super::fixture::{OLD_MKFS, OLD_MKFS_SIZE};
use super::harness::*;
use super::workload;
use crate::blockdev::BlockRef;
use crate::format::*;
use crate::volume::{self, State};

/// Undo one xorshift64 (13, 7, 17) step: the counter `new_uuid` started from.
fn unxorshift(mut y: u64) -> u64 {
    let undo_left = |y: u64, s: u32| {
        let mut x = y;
        for _ in 0..64 / s {
            x = y ^ (x << s);
        }
        x
    };
    let undo_right = |y: u64, s: u32| {
        let mut x = y;
        for _ in 0..64 / s {
            x = y ^ (x >> s);
        }
        x
    };
    y = undo_left(y, 17);
    y = undo_right(y, 7);
    undo_left(y, 13)
}

fn fixture() -> Vec<u8> {
    let mut img = std::vec![0u8; OLD_MKFS_SIZE];
    for (off, b) in OLD_MKFS {
        img[*off..*off + b.len()].copy_from_slice(b);
    }
    img
}

#[test]
fn mkfs_writes_what_mains_kernel_wrote() {
    setup(120);
    same_as_mkfs(&fixture());
    // Another kernel's `disks format` (e.g. this branch's), to compare the same way.
    if let Some(p) = std::env::var_os("MILKFS_MKFS_IMAGE") {
        same_as_mkfs(&std::fs::read(&p).unwrap());
        std::eprintln!("compat: {} is what mkfs writes", p.to_string_lossy());
    }
}

/// `old` (a fresh volume another mkfs wrote, its only label) is byte for byte what this
/// mkfs writes with that one's clock and UUID seed.
fn same_as_mkfs(old: &[u8]) {
    let sb = Superblock::decode(old[BLOCK..2 * BLOCK].try_into().unwrap()).unwrap();
    assert_eq!(sb.generation, 1);
    // Replay its clock (the root inode's times) and its UUID seed (the timer counter).
    let DiskNode::Leaf(items) = decode_node(old[2 * BLOCK..3 * BLOCK].try_into().unwrap()).unwrap()
    else {
        panic!("root is a leaf");
    };
    let root = InodeItem::decode(&items[0].1).unwrap();
    set_wall(root.mtime.0, root.mtime.1 as i64);
    let x0 = unxorshift(u64::from_le_bytes(sb.uuid[..8].try_into().unwrap()));
    set_counter(x0 ^ 0x6d69_6c6b_6673_0001);
    let d = MemDisk::new(old.len());
    volume::mkfs(&(d.clone() as BlockRef), &sb.label).unwrap();
    let new = d.image();
    let differ: Vec<usize> = (0..new.len()).filter(|&i| new[i] != old[i]).collect();
    assert!(
        differ.is_empty(),
        "{} bytes differ, first at {:?}",
        differ.len(),
        differ.first()
    );
    // And it mounts here, clean.
    let fs = Fs::mount(MemDisk::from_image(old.to_vec())).unwrap();
    fs.check_clean();
}

/// `n` small files in `/many` (a tree with internal nodes).
pub fn many_files(fs: &Fs, n: usize) {
    fs.mkdir("/many").unwrap();
    for i in 0..n {
        fs.write(&std::format!("/many/f{i:05}"), &[i as u8; 40])
            .unwrap();
    }
}

/// Decode and re-encode every block a committed state can reach; each must come out as
/// the same bytes (plaintext volumes).
pub fn roundtrip_all(st: &State, img: &[u8]) -> usize {
    let raw = |b: u64| -> &[u8; BLOCK] { img[b as usize * BLOCK..][..BLOCK].try_into().unwrap() };
    let sbs = [0u64, 1].map(|s| Superblock::decode(raw(s)));
    let mut n = 0;
    for (slot, sb) in sbs.iter().enumerate() {
        if let Some(sb) = sb {
            assert_eq!(&sb.encode(), raw(slot as u64), "superblock slot {slot}");
            n += 1;
        }
    }
    let sb = &st.sb;
    let mut seen = BTreeSet::new();
    let mut stack: Vec<BlockPtr> = std::vec![sb.root];
    stack.extend(sb.snaps.iter().chain(sb.clones.iter()).map(|s| s.root));
    while let Some(p) = stack.pop() {
        if p.is_none() || !seen.insert(p.block) {
            continue;
        }
        let b = raw(p.block);
        assert_eq!(xxh64(b), p.csum, "node {}", p.block);
        let birth = u64::from_le_bytes(b[8..16].try_into().unwrap());
        let mut out = [0u8; BLOCK];
        match decode_node(b).unwrap() {
            DiskNode::Leaf(items) => {
                for (k, v) in &items {
                    match k.kind {
                        K_INODE => assert_eq!(&InodeItem::decode(v).unwrap().encode(), v),
                        K_DIRENT => assert_eq!(&encode_dirents(&decode_dirents(v).unwrap()), v),
                        K_EXT => assert_eq!(&Ext::decode(k, v).unwrap().encode(), v),
                        K_BREF if k.obj == VOL_OBJ => {
                            let (l, x) = decode_bref(v).unwrap();
                            assert_eq!(&encode_bref(l, x), v)
                        }
                        _ => {}
                    }
                }
                encode_leaf(&items, birth, &mut out);
            }
            DiskNode::Internal(level, kids) => {
                stack.extend(kids.iter().map(|(_, c)| *c));
                encode_internal(level, &kids, birth, &mut out);
            }
        }
        assert_eq!(&out, b, "node {} re-encodes to the same bytes", p.block);
        n += 1;
    }
    let heads = std::iter::once(sb.deadlist).chain(sb.snaps.iter().map(|s| s.deadlist));
    for h in heads {
        let mut p = h;
        while !p.is_none() {
            let (e, next) = decode_dead(raw(p.block)).unwrap();
            let mut out = [0u8; BLOCK];
            encode_dead(&e, next, &mut out);
            assert_eq!(&out, raw(p.block), "dead list block {}", p.block);
            n += 1;
            p = next;
        }
    }
    if !sb.spacemap.is_none() {
        let idx = MapIndex::decode(raw(sb.spacemap.block)).unwrap();
        let mut out = [0u8; BLOCK];
        idx.encode(&mut out);
        assert_eq!(&out, raw(sb.spacemap.block), "space map index");
        n += 1;
    }
    n
}

#[test]
fn every_structure_reencodes_to_the_same_bytes() {
    setup(121);
    let (d, fs) = Fs::fresh(64);
    many_files(&fs, 1500);
    workload::run(&fs, 150);
    fs.commit().unwrap();
    let n = roundtrip_all(&fs.st(), &d.image());
    assert!(n > 50, "{n} blocks");
}

#[test]
fn an_image_another_milkfs_wrote() {
    setup(122);
    if let Some(p) = std::env::var_os("MILKFS_EXPORT_IMAGE") {
        let (d, fs) = Fs::fresh(64);
        workload::run(&fs, 200);
        fs.commit().unwrap();
        drop(fs);
        std::fs::write(&p, d.image()).unwrap();
        std::eprintln!("compat: wrote {}", p.to_string_lossy());
    }
    let Some(path) = std::env::var_os("MILKFS_COMPAT_IMAGE") else {
        std::eprintln!("compat: MILKFS_COMPAT_IMAGE not set, nothing to read");
        return;
    };
    let img = std::fs::read(&path).unwrap();
    let d = MemDisk::from_image(img.clone());
    let fs = Fs::mount(d.clone()).unwrap();
    let n = roundtrip_all(&fs.st(), &img);
    let r = fs.st().check(false).unwrap();
    assert!(r.clean(), "{}", r.text());
    let v = workload::crash_check(&fs);
    assert!(v.bad.is_empty(), "{v:#?}");
    // It takes new writes.
    workload::run(&fs, 60);
    fs.commit().unwrap();
    fs.check_clean();
    let summary: String = std::format!(
        "compat: {}: generation {}, {} structures round-trip, {} live + {} snapshot/clone files verify, {} snapshot(s), {} clone(s){}",
        path.to_string_lossy(),
        v.generation,
        n,
        v.live_ok,
        v.other_ok,
        v.snapshots,
        v.clones,
        if v.branched { ", branched" } else { "" }
    );
    std::eprintln!("{summary}");
}
