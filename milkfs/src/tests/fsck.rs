//! `check` / `check --repair` on damage made on purpose (`scripts/fscktest.sh`, whose
//! kinds these are: orphan dangling nlink parent sb smap bref, and data node, which must be
//! reported and never "repaired"). The damage is written the way `mkmilkfs.py corrupt`
//! writes it: a commit of the changed items with no space map (so mount rebuilds and runs
//! its namespace check), or bytes overwritten on the disk.

use std::string::String;
use std::sync::Arc;
use std::vec::Vec;

use super::harness::*;
use super::workload;
use crate::abi::{S_IFDIR, S_IFMT, S_IFREG};
use crate::blockdev::BlockRef;
use crate::btree;
use crate::format::*;
use crate::items::LIVE;
use crate::volume::{self, State};

/// The base image: a few seconds of the crash workload (files, reflinks, snapshots).
fn base() -> Arc<MemDisk> {
    setup(40);
    let (d, fs) = Fs::fresh(64);
    super::compat::many_files(&fs, 400);
    workload::run(&fs, 90);
    fs.commit().unwrap();
    fs.check_clean();
    drop(fs);
    d
}

fn copy(d: &MemDisk) -> Arc<MemDisk> {
    MemDisk::from_image(d.image())
}

/// Rewrite the newest superblock without its space map pointer (as the Mac tool's
/// commits leave it): the next mount rebuilds the map from the trees and checks the
/// namespace.
fn drop_spacemap(d: &MemDisk) {
    let dev: BlockRef = Arc::new(Shared(d.image_ref()));
    let sb = volume::probe(&dev).unwrap();
    let mut sb2 = sb.clone();
    sb2.spacemap = BlockPtr::NONE;
    d.poke((sb.generation % 2) as usize * BLOCK, &sb2.encode());
}

/// A view of a memory disk's bytes, for probing.
struct Shared(Vec<u8>);

impl crate::blockdev::BlockDevice for Shared {
    fn name(&self) -> &str {
        "copy"
    }
    fn block_size(&self) -> usize {
        SECTOR
    }
    fn num_blocks(&self) -> u64 {
        (self.0.len() / SECTOR) as u64
    }
    fn read_only(&self) -> bool {
        true
    }
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> crate::errno::Result<()> {
        let at = lba as usize * SECTOR;
        buf.copy_from_slice(&self.0[at..at + buf.len()]);
        Ok(())
    }
    fn write_blocks(&self, _: u64, _: &[u8]) -> crate::errno::Result<()> {
        Err(crate::errno::Errno::EROFS)
    }
    fn flush(&self) -> crate::errno::Result<()> {
        Ok(())
    }
}

impl MemDisk {
    fn image_ref(&self) -> Vec<u8> {
        self.image()
    }
}

/// Change live items in one commit, then drop the space map pointer.
fn damage_items(d: &Arc<MemDisk>, f: impl FnOnce(&mut State)) {
    let fs = Fs::mount(d.clone()).unwrap();
    {
        let mut st = fs.st();
        st.set_cur(LIVE).unwrap();
        f(&mut st);
        st.meta_dirty = true;
        st.commit().unwrap();
    }
    drop(fs);
    drop_spacemap(d);
}

fn files_and_dirs(st: &State) -> (Vec<u64>, Vec<u64>) {
    let items = st
        .test_items(Key::new(0, 0, 0), Key::new(VOL_OBJ, 0, 0))
        .unwrap();
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for (k, v) in items {
        if k.kind == K_INODE {
            let i = InodeItem::decode(&v).unwrap();
            match i.mode & S_IFMT {
                S_IFREG => files.push(k.obj),
                S_IFDIR if k.obj != ROOT_INO => dirs.push(k.obj),
                _ => {}
            }
        }
    }
    (files, dirs)
}

/// After a repair: clean, every file whole, and clean again after a remount.
fn repaired_and_good(d: &Arc<MemDisk>) {
    let fs = Fs::mount(d.clone()).unwrap();
    fs.check_clean();
    let v = workload::crash_check(&fs);
    assert!(v.bad.is_empty(), "{v:#?}");
    drop(fs);
    let fs = Fs::mount(d.clone()).unwrap();
    let r = fs.st().check(false).unwrap();
    assert!(r.clean() && r.notes.is_empty(), "{}", r.text());
}

/// Damage `kind` on a copy of `b`; mount must report it (and not repair it), check must
/// find it, repair must fix it with `fixed` in its words, and the result must be good.
fn repairable(
    b: &MemDisk,
    kind: &str,
    damage: impl FnOnce(&Arc<MemDisk>),
    found: &str,
    fixed: &str,
) {
    let d = copy(b);
    damage(&d);
    let fs = Fs::mount(d.clone()).unwrap();
    if !matches!(kind, "sb" | "bref") {
        assert!(fs.report.problems > 0, "{kind}: mount reports it");
    }
    let r1 = fs.st().check(false).unwrap();
    let text = r1.text();
    if kind == "sb" {
        assert!(
            r1.clean() && r1.notes.iter().any(|n| n.contains(found)),
            "{kind}: {text}"
        );
    } else {
        assert!(!r1.clean(), "{kind}: check finds it: {text}");
        assert!(text.contains(found), "{kind}: {text}");
    }
    let r2 = fs.st().check(true).unwrap();
    assert!(
        r2.repaired.iter().any(|r| r.contains(fixed)),
        "{kind}: repaired: {}",
        r2.text()
    );
    drop(fs);
    repaired_and_good(&d);
}

#[test]
fn repairable_damage() {
    let b = base();
    let st0 = Fs::mount(copy(&b)).unwrap();
    let (files, dirs) = files_and_dirs(&st0.st());
    drop(st0);
    assert!(!files.is_empty() && !dirs.is_empty());

    repairable(
        &b,
        "orphan",
        |d| {
            damage_items(d, |st| {
                let ino = st.alloc_ino();
                st.put_inode(
                    ino,
                    &InodeItem {
                        mode: S_IFREG | 0o644,
                        nlink: 1,
                        parent: ROOT_INO,
                        ..InodeItem::default()
                    },
                )
                .unwrap();
            })
        },
        "is in no directory",
        "lost+found",
    );
    repairable(
        &b,
        "dangling",
        |d| {
            damage_items(d, |st| {
                st.add_dirent(
                    ROOT_INO,
                    Dirent {
                        ino: 999_999,
                        dtype: 8,
                        name: String::from("ghost"),
                    },
                )
                .unwrap();
            })
        },
        "points at missing inode 999999",
        "removed entry ghost",
    );
    let f = files[0];
    repairable(
        &b,
        "nlink",
        |d| {
            damage_items(d, |st| {
                let mut i = st.inode(LIVE, f).unwrap();
                i.nlink = 9;
                st.put_inode(f, &i).unwrap();
            })
        },
        "9 links, should be",
        "link count set to",
    );
    let dir = dirs[0];
    repairable(
        &b,
        "parent",
        |d| {
            damage_items(d, |st| {
                let mut i = st.inode(LIVE, dir).unwrap();
                i.parent = 999_999;
                st.put_inode(dir, &i).unwrap();
            })
        },
        "parent 999999 should be",
        "parent set to",
    );
    repairable(
        &b,
        "sb",
        |d| {
            let sb = volume::probe(&(d.clone() as BlockRef)).unwrap();
            d.poke(((sb.generation + 1) % 2) as usize * BLOCK, &[0xab; BLOCK]);
        },
        "is torn",
        "superblock slot",
    );
}

#[test]
fn a_damaged_space_map_is_rebuilt() {
    let b = base();
    let d = copy(&b);
    let sb = volume::probe(&(d.clone() as BlockRef)).unwrap();
    let idx = MapIndex::decode(
        &d.peek(sb.spacemap.block as usize * BLOCK, BLOCK)
            .try_into()
            .unwrap(),
    )
    .unwrap();
    d.poke(
        idx.chunks.last().unwrap().block as usize * BLOCK,
        &[0x5a; BLOCK],
    );
    take_log();
    let fs = Fs::mount(d.clone()).unwrap();
    assert!(!fs.report.fast);
    assert!(
        take_log()
            .iter()
            .any(|l| l.contains("space map chunk unreadable: rebuilding"))
    );
    fs.check_clean();
    fs.write("/after", b"x").unwrap();
    fs.commit().unwrap();
    drop(fs);
    assert!(
        Fs::mount(d.clone()).unwrap().report.fast,
        "the next commit wrote a good map"
    );
    repaired_and_good(&d);
}

#[test]
fn a_wrong_shared_block_table_is_rebuilt() {
    let b = base();
    let d = copy(&b);
    // The workload made reflink copies; drop one shared-block entry: a shared run then
    // counts as referenced once, too low.
    damage_items(&d, |st| {
        let brefs = st
            .test_items(
                Key::new(VOL_OBJ, K_BREF, 0),
                Key::new(VOL_OBJ, K_BREF + 1, 0),
            )
            .unwrap();
        assert!(!brefs.is_empty(), "the workload shares blocks");
        st.test_del(&brefs[0].0).unwrap();
    });
    let fs = Fs::mount(d.clone()).unwrap();
    assert_eq!(fs.report.problems, 0, "mount doesn't count references");
    let r = fs.st().check(false).unwrap();
    assert!(r.text().contains("shared-block table"), "{}", r.text());
    assert!(r.text().contains("too low"));
    let r = fs.st().check(true).unwrap();
    assert!(
        r.repaired
            .iter()
            .any(|x| x.contains("shared-block table rebuilt"))
    );
    drop(fs);
    repaired_and_good(&d);
}

/// Unrepairable damage: reported, and repair leaves it (and the files it can't vouch
/// for) alone.
#[test]
fn damaged_data_and_nodes_are_reported_not_repaired() {
    let b = base();
    // data: the first data block of a live file zeroed
    let d = copy(&b);
    let blk = {
        let fs = Fs::mount(d.clone()).unwrap();
        let st = fs.st();
        let (files, _) = files_and_dirs(&st);
        files
            .iter()
            .find_map(|&f| st.exts(LIVE, f, 0, u64::MAX).ok()?.first().map(|e| e.block))
            .unwrap()
    };
    d.poke(blk as usize * BLOCK, &[0; BLOCK]);
    let fs = Fs::mount(d.clone()).unwrap();
    let r = fs.st().check(false).unwrap();
    assert!(
        !r.clean()
            && r.unrepairable
                .iter()
                .any(|u| u.contains("fails its checksum")),
        "{}",
        r.text()
    );
    let r = fs.st().check(true).unwrap();
    assert!(
        !r.unrepairable.is_empty() && r.repaired.is_empty(),
        "{}",
        r.text()
    );
    assert!(!fs.st().check(false).unwrap().clean(), "still damaged");
    drop(fs);

    // node: a leaf of the live tree zeroed
    let d = copy(&b);
    let leaf = {
        let fs = Fs::mount(d.clone()).unwrap();
        let st = fs.st();
        let mut nodes = Vec::new();
        btree::walk(
            &st.store,
            st.sb.root,
            &mut |p| {
                nodes.push(p.block);
                true
            },
            &mut |_, _| Ok(()),
        )
        .unwrap();
        // The root's first child: mount (a current space map) only reads the rightmost
        // path, for the orphan list; check reads it all.
        assert!(nodes.len() > 2);
        nodes[1]
    };
    d.poke(leaf as usize * BLOCK, &[0; BLOCK]);
    let before = d.image();
    let fs = Fs::mount(d.clone()).unwrap();
    let r = fs.st().check(true).unwrap();
    assert!(
        r.unrepairable
            .iter()
            .any(|u| u.contains("a tree node is damaged")),
        "{}",
        r.text()
    );
    assert!(r.repaired.is_empty());
    drop(fs);
    assert!(d.image() == before, "nothing was written");
}
