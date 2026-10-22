//! The format upgrade (`scripts/upgradetest.sh`, the file-system part; moving the old
//! folder layout is the kernel's `fs/layout.rs`): a format-v0 image as the Mac tool
//! writes it (`mkfs --v0`, `put --v0`: one-block extents, no inline data, no space map)
//! mounts, reads back byte-exact, is partly rewritten (a file mixing v0 extents with a v1
//! run, a small file going inline), and is v1 afterwards, clean, every byte right.

use std::string::String;
use std::vec::Vec;

use super::harness::*;
use crate::abi::{DT_DIR, DT_REG, S_IFDIR, S_IFREG};
use crate::blockdev::BlockRef;
use crate::format::*;
use crate::items::LIVE;
use crate::volume;

/// A v0 image of `mib` MiB holding `/docs/<name>` for each of `files`, made the way
/// `mkmilkfs.py mkfs --v0` + `put --v0` make it (all in one leaf).
pub fn v0_image(mib: usize, files: &[(&str, &[u8])]) -> Vec<u8> {
    let total = (mib << 20) / BLOCK;
    let mut img = std::vec![0u8; mib << 20];
    let t = (EPOCH, 0u32);
    let inode = |mode: u32, nlink: u32, size: u64, parent: u64| InodeItem {
        mode,
        nlink,
        size,
        parent,
        atime: t,
        mtime: t,
        ctime: t,
        ..InodeItem::default()
    };
    let g = 2; // the generation of the Mac tool's commit
    let mut next_block = 3u64; // 0, 1: superblocks, 2: the leaf
    let mut items: Vec<(Key, Vec<u8>)> = Vec::new();
    let docs = 2u64;
    items.push((
        Key::new(ROOT_INO, K_INODE, 0),
        inode(S_IFDIR | 0o755, 3, 0, ROOT_INO).encode(),
    ));
    let dirent = |ino: u64, dtype: u8, name: &str| {
        encode_dirents(&[Dirent {
            ino,
            dtype,
            name: String::from(name),
        }])
    };
    items.push((
        Key::new(ROOT_INO, K_DIRENT, name_hash(b"docs")),
        dirent(docs, DT_DIR, "docs"),
    ));
    items.push((
        Key::new(docs, K_INODE, 0),
        inode(S_IFDIR | 0o755, 2, 0, ROOT_INO).encode(),
    ));
    for (i, (name, data)) in files.iter().enumerate() {
        let ino = 3 + i as u64;
        items.push((
            Key::new(docs, K_DIRENT, name_hash(name.as_bytes())),
            dirent(ino, DT_REG, name),
        ));
        items.push((
            Key::new(ino, K_INODE, 0),
            inode(S_IFREG | 0o644, 1, data.len() as u64, docs).encode(),
        ));
        for (j, chunk) in data.chunks(BLOCK).enumerate() {
            let mut blk = [0u8; BLOCK];
            blk[..chunk.len()].copy_from_slice(chunk);
            let b = next_block;
            next_block += 1;
            img[b as usize * BLOCK..][..BLOCK].copy_from_slice(&blk);
            let mut v = std::vec![0u8; PTR_SIZE];
            BlockPtr {
                block: b,
                birth: g,
                csum: xxh64(&blk),
            }
            .put(&mut v);
            items.push((Key::new(ino, K_EXTENT, j as u64), v));
        }
    }
    items.sort_by_key(|a| a.0);
    let mut leaf = [0u8; BLOCK];
    encode_leaf(&items, g, &mut leaf);
    img[2 * BLOCK..3 * BLOCK].copy_from_slice(&leaf);
    let sb = Superblock {
        total_blocks: total as u64,
        generation: g,
        root: BlockPtr {
            block: 2,
            birth: g,
            csum: xxh64(&leaf),
        },
        deadlist: BlockPtr::NONE,
        next_ino: 3 + files.len() as u64,
        created: EPOCH as u64,
        uuid: [0x42; 16],
        label: String::from("Milk"),
        snaps: Vec::new(),
        clones: Vec::new(),
        auto: [0; 4],
        flags: 0,
        version: 0,
        spacemap: BlockPtr::NONE,
        last_scrub: 0,
        scrub_every: 0,
        keyring: BlockPtr::NONE,
        sb_mac: 0,
    };
    img[(g % 2) as usize * BLOCK..][..BLOCK].copy_from_slice(&sb.encode());
    img
}

#[test]
fn a_v0_volume_is_read_and_upgraded_by_its_first_write() {
    setup(110);
    let data = noise(250_000);
    let small = b"small file from the v0 days\n";
    let d = MemDisk::from_image(v0_image(64, &[("data.bin", &data), ("small.txt", small)]));
    let dev: BlockRef = d.clone();
    assert_eq!(volume::probe(&dev).unwrap().version, 0);
    take_log();
    let fs = Fs::mount(dev.clone()).unwrap();
    assert!(
        take_log()
            .iter()
            .any(|l| l.contains("format v0 volume: it is upgraded to v1")),
        "says so at mount"
    );
    assert!(!fs.report.fast, "no space map: rebuilt from the tree");
    assert_eq!(fs.report.problems, 0);
    assert_eq!(fs.read("/docs/small.txt").unwrap(), small);
    assert_eq!(fs.read("/docs/data.bin").unwrap(), data);
    let ino = fs.resolve(LIVE, "/docs/data.bin").unwrap();
    assert!(
        fs.st()
            .exts(LIVE, ino, 0, u64::MAX)
            .unwrap()
            .iter()
            .all(|e| e.legacy)
    );
    fs.check_clean();
    // A copy (all v1 runs), an append (v0 extents + a v1 run), a small file rewritten
    // (inline now).
    let copy = fs.read("/docs/data.bin").unwrap();
    fs.write("/docs/copy.bin", &copy).unwrap();
    fs.append("/docs/data.bin", b"appended by milkOS\n")
        .unwrap();
    fs.write("/docs/small.txt", b"new small\n").unwrap();
    fs.commit().unwrap();
    let e = fs.st().exts(LIVE, ino, 0, u64::MAX).unwrap();
    assert!(
        e.iter().any(|x| x.legacy) && e.iter().any(|x| !x.legacy),
        "mixed extents"
    );
    drop(fs);
    assert_eq!(volume::probe(&dev).unwrap().version, BASE_VERSION, "v1 now");
    let fs = Fs::mount(dev).unwrap();
    assert!(fs.report.fast);
    let mut want = data.clone();
    want.extend_from_slice(b"appended by milkOS\n");
    assert_eq!(fs.read("/docs/data.bin").unwrap(), want);
    assert_eq!(fs.read("/docs/copy.bin").unwrap(), data);
    assert_eq!(fs.read("/docs/small.txt").unwrap(), b"new small\n");
    let s = fs.resolve(LIVE, "/docs/small.txt").unwrap();
    assert!(fs.st().inline_bytes(LIVE, s).unwrap().is_some());
    fs.check_clean();
    fs.space_exact();
}
