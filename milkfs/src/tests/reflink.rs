//! Reflink copies and dedupe (`scripts/reflinktest.sh`): a reflink copy of 2 MB costs only
//! metadata, writing into it leaves the original alone, `dedupe -n` only reports, `dedupe`
//! makes plain copies share and frees their blocks, a second one finds them shared
//! already, check is clean (shared-block table, space map), the format becomes v2, every
//! file byte-exact; and the self-test's reflink checks (errors, snapshots).

use super::harness::*;
use crate::errno::Errno;
use crate::format::*;
use crate::items::LIVE;
use crate::volume;

#[test]
fn reflink_copy_dedupe_and_check() {
    setup(50);
    let (d, fs) = Fs::fresh(64);
    let big = noise(2_000_000);
    fs.mkdir("/d").unwrap();
    fs.write("/d/big.bin", &big).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.st().version, BASE_VERSION);
    let f1 = fs.st().store.free_blocks();
    fs.reflink("/d/big.bin", "/d/copy.bin").unwrap();
    fs.commit().unwrap();
    let f2 = fs.st().store.free_blocks();
    assert!(f1 - f2 < 10, "a 2 MB reflink copy used {} blocks", f1 - f2);
    assert_eq!(fs.st().version, REFLINK_VERSION);
    // Writing into the copy leaves the original alone.
    fs.append("/d/copy.bin", b"patched\n").unwrap();
    // Two plain copies, then dedupe.
    fs.write("/d/dup1.bin", &big).unwrap();
    fs.write("/d/dup2.bin", &big).unwrap();
    fs.commit().unwrap();
    let f3 = fs.st().store.free_blocks();
    let dry = fs.dedupe("/d", true).unwrap();
    assert_eq!((dry.groups, dry.deduped), (1, 2), "{dry:?}");
    assert_eq!(fs.st().store.free_blocks(), f3, "-n changes nothing");
    let r = fs.dedupe("/d", false).unwrap();
    assert_eq!((r.deduped, r.already), (2, 0), "{r:?}");
    // One more commit hands back what the dedupe's commit freed.
    fs.write("/d/x", b"").unwrap();
    fs.commit().unwrap();
    let f4 = fs.st().store.free_blocks();
    assert!(
        f4 - f3 > 900,
        "dedupe freed {} blocks (two 2 MB copies)",
        f4 - f3
    );
    let again = fs.dedupe("/d", false).unwrap();
    assert_eq!((again.deduped, again.already), (0, 2), "{again:?}");
    fs.check_clean();
    fs.space_exact();
    drop(fs);
    let fs = Fs::mount(d.clone()).unwrap();
    let mut want = big.clone();
    want.extend_from_slice(b"patched\n");
    assert_eq!(fs.read("/d/copy.bin").unwrap(), want);
    for f in ["big.bin", "dup1.bin", "dup2.bin"] {
        assert_eq!(fs.read(&std::format!("/d/{f}")).unwrap(), big, "{f}");
    }
    let sb = volume::probe(&(d as crate::blockdev::BlockRef)).unwrap();
    assert_eq!(sb.version, REFLINK_VERSION, "v2: shared blocks");
    fs.check_clean();
}

#[test]
fn reflink_rules_and_snapshots() {
    setup(51);
    let (_d, fs) = Fs::fresh(32);
    fs.mkdir("/dir").unwrap();
    fs.write("/a", &noise(100_000)).unwrap();
    fs.write("/small", b"inline data").unwrap();
    let a = fs.resolve(LIVE, "/a").unwrap();
    let dir = fs.resolve(LIVE, "/dir").unwrap();
    assert_eq!(fs.txn(LIVE, |st| st.reflink_file(a, a)), Err(Errno::EINVAL));
    assert_eq!(
        fs.txn(LIVE, |st| st.reflink_file(a, dir)),
        Err(Errno::EISDIR)
    );
    // Inline data is copied, not shared.
    fs.reflink("/small", "/small2").unwrap();
    assert_eq!(fs.read("/small2").unwrap(), b"inline data");
    // A snapshot between: deleting the original keeps the copy, the snapshot's too.
    fs.reflink("/a", "/b").unwrap();
    let data = fs.read("/a").unwrap();
    fs.st().snap_create("s").unwrap();
    fs.unlink("/a").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.read("/b").unwrap(), data);
    let g = fs.st().snap_list()[0].generation;
    assert_eq!(fs.read_in(g, "/a").unwrap(), data);
    fs.check_clean();
    fs.space_exact();
    fs.st().snap_delete("s").unwrap();
    fs.unlink("/b").unwrap();
    fs.commit().unwrap();
    fs.check_clean();
    fs.space_exact();
}
