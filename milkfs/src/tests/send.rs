//! Snapshots + diff + send / receive (`scripts/sendtest.sh`): full and incremental
//! streams, an encrypted one, receiving them elsewhere gives identical trees; receiving
//! again is refused; an incremental stream without its base, a damaged and a truncated
//! stream are refused and change nothing; a crash in the middle of a receive leaves the
//! target untouched or complete, and receiving again works.

use std::sync::Arc;
use std::vec::Vec;

use super::harness::*;
use crate::errno::{Errno, Result};
use crate::items::LIVE;
use crate::stream::{self, StreamSink};

struct Buf<'a>(&'a mut Vec<u8>);

impl StreamSink for Buf<'_> {
    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.0.extend_from_slice(b);
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

fn send(fs: &Fs, snap: &str, from: Option<&str>, pass: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    let r = stream::send(fs, snap, from, pass.map(str::as_bytes), || {
        Ok(Buf(&mut out))
    })
    .unwrap();
    assert_eq!(r.bytes, out.len() as u64);
    assert_eq!(r.encrypted, pass.is_some());
    out
}

fn receive(fs: &Fs, s: &[u8], pass: Option<&str>) -> Result<stream::RecvReport> {
    let mut at = 0;
    let read = |b: &mut [u8]| -> Result<usize> {
        let n = b.len().min(s.len() - at);
        b[..n].copy_from_slice(&s[at..at + n]);
        at += n;
        Ok(n)
    };
    stream::receive(fs, read, false, pass.map(str::as_bytes))
}

fn view_of(fs: &Fs, snap: &str) -> u64 {
    fs.st()
        .snap_list()
        .into_iter()
        .find(|s| s.name == snap)
        .unwrap()
        .generation
}

/// The source volume: a big file put there, then a reflink copy, a small file, a
/// subdirectory, a hard link; s1; then appends, a delete, a rename, a new file, a write
/// through the link; s2.
fn source() -> (Arc<MemDisk>, Fs) {
    let (d, fs) = Fs::fresh(64);
    let big = noise(1_500_000);
    fs.mkdir("/data").unwrap();
    fs.write("/data/big.bin", &big).unwrap();
    fs.mkdir("/bulk").unwrap();
    fs.write("/bulk/bulk.bin", &noise(4_000_000)).unwrap();
    fs.reflink("/data/big.bin", "/data/big-copy.bin").unwrap();
    fs.write("/data/small.txt", b"hello\n").unwrap();
    fs.mkdir("/data/sub").unwrap();
    fs.write("/data/sub/n.txt", b"nested\n").unwrap();
    fs.link("/data/small.txt", "/data/link").unwrap();
    fs.symlink("/data/sym", "small.txt").unwrap();
    fs.st().snap_create("s1").unwrap();
    advance(10);
    fs.append("/data/small.txt", b"more\n").unwrap();
    fs.unlink("/data/sub/n.txt").unwrap();
    fs.rename("/data/big-copy.bin", "/data/sub/moved.bin")
        .unwrap();
    fs.append("/data/big.bin", b"appended\n").unwrap();
    fs.write("/data/new.txt", b"brand new\n").unwrap();
    fs.write("/data/link-target.txt", b"rewritten\n").unwrap();
    fs.st().snap_create("s2").unwrap();
    (d, fs)
}

#[test]
fn full_and_incremental_streams_recreate_the_trees() {
    setup(70);
    let (_d, src) = source();
    let s1 = send(&src, "s1", None, None);
    let s2 = send(&src, "s2", Some("s1"), None);
    let s2_full = send(&src, "s2", None, None);
    assert!(
        s1.windows(8).any(|w| w == stream::STREAM_MAGIC),
        "a plaintext stream"
    );
    assert!(
        s2.len() < s2_full.len() / 2,
        "incremental is the change only"
    );
    let t1 = src.tree(view_of(&src, "s1")).unwrap();
    let t2 = src.tree(view_of(&src, "s2")).unwrap();

    let (_b, bk) = Fs::fresh(64);
    let r = receive(&bk, &s1, None).unwrap();
    assert!(r.snapshot == "s1" && !r.incremental);
    let r = receive(&bk, &s2, None).unwrap();
    assert!(r.snapshot == "s2" && r.incremental);
    assert_eq!(
        receive(&bk, &s2, None).err(),
        Some(Errno::EEXIST),
        "again: refused"
    );
    assert_eq!(bk.tree(view_of(&bk, "s1")).unwrap(), t1);
    assert_eq!(bk.tree(view_of(&bk, "s2")).unwrap(), t2);
    assert_eq!(
        bk.tree(LIVE).unwrap(),
        t2,
        "the live tree is the newest received"
    );
    // The sender's creation times travel (an incremental finds its base by them).
    let (a, b) = (src.st().snap_list(), bk.st().snap_list());
    assert_eq!(
        a.iter().map(|s| s.created).collect::<Vec<_>>(),
        b.iter().map(|s| s.created).collect::<Vec<_>>()
    );
    // Reflink copies stay shared on the receiver (DATA once, EXTENT twice).
    bk.check_clean();
    bk.space_exact();

    let (_h, h2) = Fs::fresh(64);
    receive(&h2, &s2_full, None).unwrap();
    assert_eq!(h2.tree(view_of(&h2, "s2")).unwrap(), t2);
    h2.check_clean();
    src.check_clean();
}

#[test]
fn encrypted_streams() {
    setup(71);
    let (_d, src) = source();
    let sealed = send(&src, "s1", None, Some("fake-stream-passphrase"));
    assert_eq!(&sealed[..8], crate::crypt::SEALED_MAGIC);
    assert!(
        !sealed.windows(8).any(|w| w == stream::STREAM_MAGIC),
        "no plaintext header inside"
    );
    assert!(
        !sealed.windows(9).any(|w| w == b"small.txt"),
        "no file names"
    );
    let (_h, h) = Fs::fresh(64);
    take_reason();
    assert_eq!(receive(&h, &sealed, None).err(), Some(Errno::EACCES));
    assert!(take_reason().contains("give its passphrase"));
    assert_eq!(
        receive(&h, &sealed, Some("wrong-fake-passphrase")).err(),
        Some(Errno::EACCES)
    );
    assert_eq!(take_reason(), "wrong passphrase for the stream");
    assert!(h.ls("/").unwrap().is_empty());
    let r = receive(&h, &sealed, Some("fake-stream-passphrase")).unwrap();
    assert!(r.encrypted);
    assert_eq!(
        h.tree(view_of(&h, "s1")).unwrap(),
        src.tree(view_of(&src, "s1")).unwrap()
    );
    h.check_clean();
}

#[test]
fn refused_streams_change_nothing() {
    setup(72);
    let (_d, src) = source();
    let s1 = send(&src, "s1", None, None);
    let s2 = send(&src, "s2", Some("s1"), None);
    let mut bad = s1.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x40;
    let short = s1[..s1.len() / 2].to_vec();
    let (_f, fresh) = Fs::fresh(32);
    assert_eq!(
        receive(&fresh, &s2, None).err(),
        Some(Errno::ENOENT),
        "base missing"
    );
    assert_eq!(receive(&fresh, &bad, None).err(), Some(Errno::EIO));
    assert_eq!(receive(&fresh, &short, None).err(), Some(Errno::EIO));
    let empty = |fs: &Fs| {
        assert!(fs.ls("/").unwrap().is_empty());
        assert!(fs.st().snap_list().is_empty());
        assert!(fs.st().clone_list().is_empty());
        assert!(!fs.st().receiving);
        fs.check_clean();
        fs.space_exact();
    };
    empty(&fresh);
    // A full stream needs an empty volume; an incremental one an untouched base.
    let (_g, used) = Fs::fresh(32);
    used.write("/x", b"x").unwrap();
    assert_eq!(receive(&used, &s1, None).err(), Some(Errno::ENOTEMPTY));
    let (_k, k) = Fs::fresh(64);
    receive(&k, &s1, None).unwrap();
    k.write("/changed", b"since s1").unwrap();
    assert_eq!(receive(&k, &s2, None).err(), Some(Errno::EBUSY));
    let mut at = 0;
    let read = |b: &mut [u8]| -> Result<usize> {
        let n = b.len().min(s2.len() - at);
        b[..n].copy_from_slice(&s2[at..at + n]);
        at += n;
        Ok(n)
    };
    stream::receive(&k, read, true, None).unwrap(); // --force: roll back to the base first
    assert_eq!(
        k.tree(LIVE).unwrap(),
        src.tree(view_of(&src, "s2")).unwrap()
    );
}

#[test]
fn a_crash_in_the_middle_of_a_receive() {
    setup(73);
    let (_d, src) = source();
    let full = send(&src, "s2", None, None);
    let want = src.tree(view_of(&src, "s2")).unwrap();
    let (blank, _) = Fs::fresh(64);
    let img = blank.image();
    let (mut untouched, mut complete) = (0, 0);
    for n in [1u64, 3, 6, 10, 15, 22, 30, 60] {
        let disk = CrashDisk::new(img.clone(), Cut::Writes(n));
        let fs = Fs::mount(disk.clone()).unwrap();
        disk.arm();
        let r = receive(&fs, &full, None);
        drop(fs);
        let left = MemDisk::from_image(disk.image());
        let fs = Fs::mount(left.clone()).unwrap();
        fs.check_clean();
        if fs.st().snap_list().iter().any(|s| s.name == "s2") {
            // It had finished (its last commit landed): complete, and a second receive
            // is refused.
            assert_eq!(fs.tree(view_of(&fs, "s2")).unwrap(), want, "complete");
            assert_eq!(receive(&fs, &full, None).err(), Some(Errno::EEXIST));
            complete += 1;
        } else {
            assert!(r.is_err() && disk.dead());
            assert!(fs.ls("/").unwrap().is_empty(), "untouched after {n} writes");
            // Receiving again works (a leftover hidden clone is discarded first).
            if let Err(e) = receive(&fs, &full, None) {
                let snaps = fs.st().snap_list();
                let clones = fs.st().clone_list();
                panic!("again after {n}: {e} (snapshots {snaps:?}, clones {clones:?})");
            }
            assert_eq!(fs.tree(view_of(&fs, "s2")).unwrap(), want);
            untouched += 1;
        }
        assert!(fs.st().clone_list().is_empty());
        fs.check_clean();
        fs.space_exact();
    }
    assert!(complete > 0, "some receive finished before the cut");
    assert!(untouched > 0, "some cut landed mid-receive");
}
