//! Format, mount, read, write: the kernel self-test's `milkfs_destructive` namespace part
//! and what every guest test leans on.

use std::sync::Arc;
use std::vec::Vec;

use super::harness::*;
use crate::abi::{S_IFDIR, S_IFMT};
use crate::blockdev::BlockRef;
use crate::errno::Errno;
use crate::format::*;
use crate::items::{LIVE, MAX_FILE_SIZE};
use crate::volume::{self, COMMIT_DELAY_NS, MAX_BATCH_OPS, State};

#[test]
fn mkfs_gives_an_empty_root() {
    setup(1);
    let (d, fs) = Fs::fresh(16);
    assert_eq!(fs.generation(), 1);
    assert!(fs.ls("/").unwrap().is_empty());
    let root = fs.st().inode(LIVE, ROOT_INO).unwrap();
    assert_eq!(root.mode & S_IFMT, S_IFDIR);
    assert_eq!((root.nlink, root.parent), (2, ROOT_INO));
    assert_eq!(root.mtime, (EPOCH, 0), "times come from the platform clock");
    let sb = volume::probe(&(d.clone() as BlockRef)).unwrap();
    assert_eq!((sb.label.as_str(), sb.version), ("Milk", BASE_VERSION));
    assert_eq!(sb.total_blocks, (16 << 20) / BLOCK as u64);
    // Slot 0 was cleared, slot 1 holds generation 1.
    assert!(d.peek(0, BLOCK).iter().all(|&b| b == 0));
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn mkfs_refuses_a_tiny_disk() {
    setup(2);
    let d: BlockRef = MemDisk::new(15 * BLOCK);
    assert_eq!(volume::mkfs(&d, "x"), Err(Errno::ENOSPC));
    let d: BlockRef = MemDisk::new(16 * BLOCK);
    assert_eq!(volume::mkfs(&d, "x"), Ok(()));
}

#[test]
fn mount_refuses_what_isnt_milkfs() {
    setup(3);
    let d: BlockRef = MemDisk::new(1 << 20);
    assert!(volume::probe(&d).is_none());
    assert_eq!(State::open(d).err(), Some(Errno::EINVAL));
}

#[test]
fn write_read_back_inline_extents_sparse() {
    setup(4);
    let (_d, fs) = Fs::fresh(32);
    fs.mkdir("/d").unwrap();
    fs.write("/d/a.txt", b"hello world").unwrap();
    assert_eq!(fs.read("/d/a.txt").unwrap(), b"hello world");
    fs.pwrite("/d/a.txt", 6, b"W").unwrap();
    fs.append("/d/a.txt", b"!").unwrap();
    assert_eq!(fs.read("/d/a.txt").unwrap(), b"hello World!");
    // Small: inline, no data blocks.
    let a = fs.resolve(LIVE, "/d/a.txt").unwrap();
    assert!(fs.st().inline_bytes(LIVE, a).unwrap().is_some());
    assert_eq!(fs.st().block_count(LIVE, a).unwrap(), 0);
    // 300 KiB: extents (multi-block runs), byte-exact.
    let big = noise(300 << 10);
    fs.write("/d/big", &big).unwrap();
    assert_eq!(fs.read("/d/big").unwrap(), big);
    let b = fs.resolve(LIVE, "/d/big").unwrap();
    assert_eq!(fs.st().block_count(LIVE, b).unwrap(), 75);
    assert!(fs.st().exts(LIVE, b, 0, u64::MAX).unwrap().len() < 75);
    // Growing an inline file past INLINE_MAX moves it to extents.
    fs.append("/d/a.txt", &[7u8; INLINE_MAX]).unwrap();
    assert!(fs.st().inline_bytes(LIVE, a).unwrap().is_none());
    assert_eq!(fs.read("/d/a.txt").unwrap().len(), 12 + INLINE_MAX);
    // Truncate: the tail goes, growing reads zeros.
    fs.truncate("/d/big", 5000).unwrap();
    assert_eq!(fs.read("/d/big").unwrap(), big[..5000]);
    fs.truncate("/d/big", 9000).unwrap();
    let r = fs.read("/d/big").unwrap();
    assert_eq!(&r[..5000], &big[..5000]);
    assert!(r[5000..].iter().all(|&x| x == 0));
    // Sparse: one block written far out, the hole reads zeros.
    fs.write("/d/sparse", b"").unwrap();
    fs.pwrite("/d/sparse", 10 << 20, b"end").unwrap();
    let s = fs.resolve(LIVE, "/d/sparse").unwrap();
    assert_eq!(fs.st().block_count(LIVE, s).unwrap(), 1);
    let r = fs.read("/d/sparse").unwrap();
    assert_eq!(r.len(), (10 << 20) + 3);
    assert!(r[..10 << 20].iter().all(|&x| x == 0));
    assert_eq!(&r[10 << 20..], b"end");
    // Too big.
    assert_eq!(
        fs.pwrite("/d/sparse", MAX_FILE_SIZE, b"x"),
        Err(Errno::EFBIG)
    );
    fs.commit().unwrap();
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn everything_survives_a_remount() {
    setup(5);
    let (d, fs) = Fs::fresh(32);
    let mut want = Vec::new();
    fs.mkdir_all("/a/b/c").unwrap();
    for i in 0..60u64 {
        let p = std::format!("/a/b/c/f{i}");
        let data = content(i);
        fs.write(&p, &data).unwrap();
        want.push((p, data));
    }
    fs.symlink("/a/link", "b/c/f1").unwrap();
    fs.commit().unwrap();
    let tree = fs.tree(LIVE).unwrap();
    let generation = fs.generation();
    drop(fs);
    let fs = Fs::mount(d.clone()).unwrap();
    assert!(fs.report.fast, "a current space map: no tree scan");
    assert_eq!(fs.report.generation, generation);
    assert_eq!(fs.tree(LIVE).unwrap(), tree);
    for (p, data) in &want {
        assert_eq!(&fs.read(p).unwrap(), data);
    }
    let l = fs.resolve(LIVE, "/a/link").unwrap();
    assert_eq!(fs.st().symlink_target(LIVE, l).unwrap(), "b/c/f1");
    fs.check_clean();
}

#[test]
fn namespace_rules() {
    setup(6);
    let (_d, fs) = Fs::fresh(16);
    fs.mkdir("/d").unwrap();
    fs.write("/d/a", b"a").unwrap();
    assert_eq!(fs.mkdir("/d/a"), Err(Errno::EEXIST));
    assert_eq!(fs.create_in(LIVE, "/d/a/x", 0o100644), Err(Errno::ENOTDIR));
    // rename, the old name is gone
    fs.rename("/d/a", "/d/b").unwrap();
    assert_eq!(fs.resolve(LIVE, "/d/a"), Err(Errno::ENOENT));
    assert_eq!(fs.read("/d/b").unwrap(), b"a");
    // directories: nlink, rename, `..`, not into itself, not over a non-empty one
    fs.mkdir("/d/sub").unwrap();
    fs.mkdir("/d/sub/inner").unwrap();
    let d = fs.resolve(LIVE, "/d").unwrap();
    assert_eq!(fs.st().inode(LIVE, d).unwrap().nlink, 3);
    fs.rename("/d/sub", "/sub2").unwrap();
    assert_eq!(fs.st().inode(LIVE, d).unwrap().nlink, 2);
    let s2 = fs.resolve(LIVE, "/sub2").unwrap();
    assert_eq!(fs.st().inode(LIVE, s2).unwrap().parent, ROOT_INO);
    assert_eq!(fs.rename("/sub2", "/sub2/inner/x"), Err(Errno::EINVAL));
    assert_eq!(fs.rmdir("/sub2"), Err(Errno::ENOTEMPTY));
    fs.mkdir("/empty").unwrap();
    assert_eq!(fs.rename("/d/b", "/empty"), Err(Errno::EISDIR));
    assert_eq!(fs.rename("/empty", "/d/b"), Err(Errno::ENOTDIR));
    assert_eq!(fs.unlink("/sub2"), Err(Errno::EISDIR));
    assert_eq!(fs.rmdir("/d/b"), Err(Errno::ENOTDIR));
    // hard links
    fs.link("/d/b", "/d/c").unwrap();
    let b = fs.resolve(LIVE, "/d/b").unwrap();
    assert_eq!(fs.st().inode(LIVE, b).unwrap().nlink, 2);
    fs.unlink("/d/b").unwrap();
    assert_eq!(fs.read("/d/c").unwrap(), b"a");
    assert_eq!(fs.st().inode(LIVE, b).unwrap().nlink, 1);
    // renaming over a file drops its link
    fs.write("/d/x", b"x").unwrap();
    let x = fs.resolve(LIVE, "/d/x").unwrap();
    fs.rename("/d/c", "/d/x").unwrap();
    assert_eq!(fs.st().inode(LIVE, x).err(), Some(Errno::ENOENT));
    assert_eq!(fs.read("/d/x").unwrap(), b"a");
    fs.commit().unwrap();
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn rename_onto_an_empty_dir_replaces_it() {
    setup(7);
    let (_d, fs) = Fs::fresh(16);
    fs.mkdir_all("/a/x").unwrap();
    fs.mkdir("/b").unwrap();
    fs.rename("/b", "/a/x").unwrap();
    assert_eq!(fs.ls("/a").unwrap(), ["x"]);
    assert_eq!(fs.ls("/").unwrap(), ["a"]);
    let a = fs.resolve(LIVE, "/a").unwrap();
    assert_eq!(fs.st().inode(LIVE, a).unwrap().nlink, 3);
    fs.commit().unwrap();
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn uncommitted_work_is_lost_committed_work_is_not() {
    setup(8);
    let (d, fs) = Fs::fresh(16);
    fs.write("/kept", b"kept").unwrap();
    fs.commit().unwrap();
    fs.write("/lost", b"lost").unwrap();
    assert!(fs.st().dirty());
    drop(fs); // the machine goes down: the open transaction never committed
    let fs = Fs::mount(d.clone()).unwrap();
    assert_eq!(fs.read("/kept").unwrap(), b"kept");
    assert_eq!(fs.resolve(LIVE, "/lost"), Err(Errno::ENOENT));
    fs.check_clean();
}

#[test]
fn transactions_batch_until_old_or_big() {
    setup(9);
    let (_d, fs) = Fs::fresh(16);
    let g = fs.generation();
    fs.write("/a", b"1").unwrap();
    assert_eq!(fs.generation(), g, "joins the open transaction");
    advance(COMMIT_DELAY_NS / 1_000_000_000);
    fs.write("/b", b"2").unwrap();
    assert_eq!(fs.generation(), g + 1, "a second old: committed");
    // MAX_BATCH_OPS operations in a batch force a commit even with the clock stopped.
    let g = fs.generation();
    for i in 0..MAX_BATCH_OPS {
        fs.write(&std::format!("/m{i}"), b"").unwrap();
    }
    assert!(fs.generation() > g);
    // flush_if_due: the flusher thread's call
    fs.write("/c", b"3").unwrap();
    let g = fs.generation();
    fs.st().flush_if_due().unwrap();
    assert_eq!(fs.generation(), g);
    advance(2);
    fs.st().flush_if_due().unwrap();
    assert_eq!(fs.generation(), g + 1);
}

#[test]
fn unlinked_while_open_is_reclaimed_at_mount() {
    setup(10);
    let (d, fs) = Fs::fresh(16);
    fs.write("/f", &noise(40_000)).unwrap();
    let ino = fs.resolve(LIVE, "/f").unwrap();
    // Unlink it while "open": it stays, with no links, on the orphan list.
    fs.txn(LIVE, |st| st.unlink_node(ROOT_INO, "f", &|_, i| i == ino))
        .unwrap();
    assert_eq!(fs.st().inode(LIVE, ino).unwrap().nlink, 0);
    fs.commit().unwrap();
    drop(fs); // the machine goes down with the file open
    let fs = Fs::mount(d.clone()).unwrap();
    assert_eq!(fs.report.orphans, 1);
    assert_eq!(fs.st().inode(LIVE, ino).err(), Some(Errno::ENOENT));
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn mount_rebuilds_a_missing_space_map() {
    setup(11);
    let (d, fs) = Fs::fresh(16);
    fs.write("/f", &noise(100_000)).unwrap();
    fs.commit().unwrap();
    let map = fs.st().sb.spacemap;
    drop(fs);
    // Damage a bitmap chunk: mount notices and rebuilds from the trees.
    let idx = MapIndex::decode(
        d.peek(map.block as usize * BLOCK, BLOCK)
            .as_slice()
            .try_into()
            .unwrap(),
    );
    let chunk = idx.unwrap().chunks[0].block as usize;
    d.poke(chunk * BLOCK, &[0x5a; BLOCK]);
    take_log();
    let fs = Fs::mount(d.clone()).unwrap();
    assert!(!fs.report.fast);
    assert!(
        take_log()
            .iter()
            .any(|l| l.contains("space map chunk unreadable: rebuilding")),
        "says why"
    );
    assert_eq!(fs.report.problems, 0);
    fs.check_clean();
    fs.space_exact();
    // The next commit writes a good map again.
    fs.write("/g", b"g").unwrap();
    fs.commit().unwrap();
    drop(fs);
    assert!(Fs::mount(d).unwrap().report.fast);
}

#[test]
fn statfs_numbers_follow_writes() {
    setup(12);
    let (_d, fs) = Fs::fresh(16);
    let free0 = fs.st().store.free_blocks();
    fs.write("/f", &noise(64 * BLOCK)).unwrap();
    fs.commit().unwrap();
    let free1 = fs.st().store.free_blocks();
    assert!(free0 - free1 >= 64);
    fs.unlink("/f").unwrap();
    fs.commit().unwrap();
    // The commit after the one that freed them hands them back.
    fs.write("/g", b"").unwrap();
    fs.commit().unwrap();
    assert!(fs.st().store.free_blocks() + 4 >= free0);
}

#[test]
fn a_second_volume_on_the_same_host_is_independent() {
    setup(13);
    let (_a, fa) = Fs::fresh(16);
    let (_b, fb) = Fs::fresh(16);
    fa.write("/only-a", b"a").unwrap();
    fb.write("/only-b", b"b").unwrap();
    assert_eq!(fa.ls("/").unwrap(), ["only-a"]);
    assert_eq!(fb.ls("/").unwrap(), ["only-b"]);
    assert_ne!(fa.st().sb.uuid, fb.st().sb.uuid);
}

#[test]
fn the_cache_writes_back_and_flushes() {
    setup(14);
    let d = MemDisk::new(1 << 20);
    let c = crate::bcache::BCache::new(d.clone(), 4).unwrap();
    let blk = |n: u64| -> crate::bcache::Block { [(n as u8).wrapping_mul(37) ^ 0x5a; BLOCK] };
    for n in 0..10 {
        c.write(n, &blk(n)).unwrap();
    }
    let mut b = [0u8; BLOCK];
    c.read(9, &mut b).unwrap();
    assert_eq!(b, blk(9), "reads its own writes");
    assert!(c.stats().writebacks >= 6, "evicted and wrote back");
    c.flush().unwrap();
    assert_eq!(c.usage().2, 0, "flush leaves nothing dirty");
    for n in 0..10 {
        assert_eq!(
            d.peek(n as usize * BLOCK, BLOCK),
            blk(n),
            "on the disk after flush"
        );
    }
    assert_eq!(c.read(256, &mut b), Err(Errno::ENXIO), "past the end");
    let _: Arc<MemDisk> = d;
}
