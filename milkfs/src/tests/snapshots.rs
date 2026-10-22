//! Snapshots (create, delete with dead lists, rollback), writable clones (discard,
//! promote, undo with a rollback), diff, a file's versions, and the automatic snapshots'
//! policy: the kernel self-test's milkfs snapshot, clone, diff, versions and Time Machine
//! checks, with space accounting exact after every step.

use super::harness::*;
use super::workload;
use crate::diff::Change;
use crate::errno::Errno;
use crate::format::*;
use crate::items::{LIVE, clone_view};
use crate::snap::SnapInfo;
use crate::timemachine::{self, Config, prune_plan, stamp};

#[test]
fn snapshot_delete_rollback_keep_space_exact() {
    setup(60);
    let (d, fs) = Fs::fresh(64);
    fs.mkdir("/d").unwrap();
    fs.write("/d/a", b"version 1").unwrap();
    fs.write("/d/big", &noise(500_000)).unwrap();
    let s = fs.st().snap_create("one").unwrap();
    assert_eq!(fs.st().snap_create("one").err(), Some(Errno::EEXIST));
    assert_eq!(fs.st().snap_create("bad/name").err(), Some(Errno::EINVAL));
    assert_eq!(
        fs.st().snap_create(&"x".repeat(SNAP_NAME_MAX + 1)).err(),
        Some(Errno::ENAMETOOLONG)
    );
    fs.space_exact();
    fs.write("/d/a", b"version 2").unwrap();
    fs.unlink("/d/big").unwrap();
    fs.write("/d/new", b"new").unwrap();
    fs.commit().unwrap();
    // The snapshot still has the old contents (the dead list keeps big's blocks).
    assert_eq!(fs.read_in(s.generation, "/d/a").unwrap(), b"version 1");
    assert_eq!(fs.read_in(s.generation, "/d/big").unwrap().len(), 500_000);
    assert_eq!(fs.resolve(s.generation, "/d/new"), Err(Errno::ENOENT));
    assert_eq!(
        fs.txn(s.generation, |st| st.write_file(1, 0, b"x")),
        Err(Errno::EROFS)
    );
    fs.space_exact();
    // A second snapshot, then delete the first: what only it had is freed.
    let free0 = fs.st().store.free_blocks();
    fs.st().snap_create("two").unwrap();
    fs.st().snap_delete("one").unwrap();
    fs.write("/d/x", b"").unwrap();
    fs.commit().unwrap();
    assert!(
        fs.st().store.free_blocks() >= free0 + 120,
        "big's blocks came back"
    );
    fs.space_exact();
    // Rollback to two: changes after it are gone.
    fs.write("/d/a", b"version 3").unwrap();
    fs.commit().unwrap();
    workload::rollback(&fs, "two").unwrap();
    assert_eq!(fs.read("/d/a").unwrap(), b"version 2");
    assert_eq!(fs.resolve(LIVE, "/d/x"), Err(Errno::ENOENT));
    fs.space_exact();
    // Survives a remount; deleting the last snapshot leaves the volume exact.
    drop(fs);
    let fs = Fs::mount(d).unwrap();
    assert_eq!(fs.st().snap_list().len(), 1);
    fs.st().snap_delete("two").unwrap();
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn clones_discard_promote_and_undo() {
    setup(61);
    let (d, fs) = Fs::fresh(64);
    fs.mkdir("/c").unwrap();
    fs.write("/c/config", b"v1").unwrap();
    fs.write("/c/keep", b"unchanged").unwrap();
    fs.st().snap_create("base").unwrap();
    let id = workload::clone(&fs, "base", "try").unwrap();
    let cv = clone_view(id);
    assert_eq!(
        fs.read_in(cv, "/c/config").unwrap(),
        b"v1",
        "starts as the snapshot"
    );
    fs.write_in(cv, "/c/config", b"v2").unwrap();
    fs.write_in(cv, "/c/installed.app", b"new app").unwrap();
    assert_eq!(fs.read_in(cv, "/c/config").unwrap(), b"v2");
    assert_eq!(fs.read("/c/config").unwrap(), b"v1", "live untouched");
    assert_eq!(fs.resolve(LIVE, "/c/installed.app"), Err(Errno::ENOENT));
    let base = fs.st().snap_list()[0].generation;
    assert_eq!(
        fs.read_in(base, "/c/config").unwrap(),
        b"v1",
        "origin unchanged"
    );
    assert_eq!(
        fs.st().snap_delete("base"),
        Err(Errno::EBUSY),
        "origin in use"
    );
    fs.space_exact();
    // A second clone with a big file, discarded: its blocks come back.
    let id2 = workload::clone(&fs, "base", "scratch").unwrap();
    fs.write_in(clone_view(id2), "/big", &noise(200_000))
        .unwrap();
    fs.space_exact();
    let with = fs.st().store.free_blocks();
    workload::discard(&fs, "scratch").unwrap();
    fs.commit().unwrap();
    let now = fs.st().store.free_blocks();
    assert!(
        now >= with + 49,
        "discard frees the clone's blocks ({with} -> {now})"
    );
    fs.space_exact();
    // The clone survives a remount.
    fs.commit().unwrap();
    drop(fs);
    let fs = Fs::mount(d).unwrap();
    assert_eq!(fs.report.problems, 0);
    let c = fs.st().clone_list();
    assert_eq!(c.len(), 1);
    assert_eq!(
        fs.read_in(clone_view(c[0].id), "/c/installed.app").unwrap(),
        b"new app"
    );
    // Promote: the clone is live, the old live kept as before-try.
    assert_eq!(workload::promote(&fs, "try").unwrap(), "before-try");
    assert_eq!(fs.read("/c/config").unwrap(), b"v2");
    assert_eq!(fs.read("/c/installed.app").unwrap(), b"new app");
    assert_eq!(fs.read("/c/keep").unwrap(), b"unchanged");
    let before = fs
        .st()
        .snap_list()
        .iter()
        .find(|s| s.name == "before-try")
        .unwrap()
        .generation;
    assert_eq!(fs.read_in(before, "/c/config").unwrap(), b"v1");
    assert!(fs.st().branched(), "history is branched now");
    fs.space_exact();
    fs.st().snap_delete("base").unwrap();
    fs.space_exact();
    // Undo the install.
    workload::rollback(&fs, "before-try").unwrap();
    assert_eq!(fs.read("/c/config").unwrap(), b"v1");
    assert_eq!(fs.resolve(LIVE, "/c/installed.app"), Err(Errno::ENOENT));
    fs.space_exact();
    fs.st().snap_delete("before-try").unwrap();
    assert!(!fs.st().branched(), "no snapshots left: linear again");
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn diff_between_snapshots_and_live() {
    setup(62);
    let (_d, fs) = Fs::fresh(32);
    fs.mkdir("/docs").unwrap();
    fs.write("/docs/a", b"a").unwrap();
    fs.write("/docs/b", b"b").unwrap();
    fs.write("/docs/c", b"c").unwrap();
    fs.st().snap_create("s1").unwrap();
    fs.write("/docs/a", b"a changed").unwrap();
    fs.unlink("/docs/b").unwrap();
    fs.rename("/docs/c", "/docs/c2").unwrap();
    fs.write("/docs/new", b"new").unwrap();
    fs.st().snap_create("s2").unwrap();
    let r = fs.st().diff("s1", Some("s2")).unwrap();
    let mut lines: Vec<String> = r.changes.iter().map(Change::line).collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "A /docs/new",
            "D /docs/b",
            "M /docs/a",
            "R /docs/c -> /docs/c2"
        ],
        "{lines:?}"
    );
    // Against the live tree, and nothing between a snapshot and itself.
    fs.write("/docs/live-only", b"x").unwrap();
    let r = fs.st().diff("s2", None).unwrap();
    assert_eq!(
        r.changes.iter().map(Change::line).collect::<Vec<_>>(),
        ["A /docs/live-only"]
    );
    assert!(fs.st().diff("s2", Some("s2")).unwrap().changes.is_empty());
    assert_eq!(fs.st().diff("nope", None).err(), Some(Errno::ENOENT));
}

#[test]
fn a_files_versions() {
    setup(63);
    let (_d, fs) = Fs::fresh(32);
    fs.write("/notes.txt", b"one").unwrap();
    fs.st().snap_create("s1").unwrap();
    advance(60);
    fs.st().snap_create("s2").unwrap(); // same content: one version
    advance(60);
    fs.write("/notes.txt", b"two").unwrap();
    fs.st().snap_create("s3").unwrap();
    advance(60);
    // Saved the editor way (new inode): followed by path.
    fs.write("/.tmp", b"three").unwrap();
    fs.rename("/.tmp", "/notes.txt").unwrap();
    let ino = fs.resolve(LIVE, "/notes.txt").unwrap();
    let v = fs.st().versions_of(ino).unwrap();
    let names: Vec<Option<&str>> = v.iter().map(|x| x.snapshot.as_deref()).collect();
    assert_eq!(names, [Some("s1"), Some("s3"), None], "{v:#?}");
    assert_eq!(v[0].size, 3);
    assert_ne!(v[0].hash, v[1].hash);
}

#[test]
fn time_machine_policy() {
    setup(64);
    // Retention: the newest `keep`, plus the newest of each of the last `daily` days.
    let mk = |g: u64, t: u64| SnapInfo {
        name: std::format!("auto-{g}"),
        generation: g,
        created: t,
    };
    let day = 86_400;
    let snaps: Vec<SnapInfo> = (1..=10).map(|g| mk(g, g * day / 2)).collect();
    let mut gone = prune_plan(&snaps, 2, 3);
    gone.sort();
    // Newest 2: 10, 9; the newest of each of the last 3 days (5, 4, 3): 10, 9, 7.
    assert_eq!(
        gone,
        [
            "auto-1", "auto-2", "auto-3", "auto-4", "auto-5", "auto-6", "auto-8"
        ]
    );
    assert_eq!(stamp(1_792_627_200), "2026-10-22T00:00:00");
    assert_eq!(timemachine::civil(951_782_400), (2000, 2, 29, 0, 0, 0));
    assert_eq!(timemachine::parse_interval("15m"), Some(900));
    assert_eq!(timemachine::parse_interval("2"), Some(120));
    assert_eq!(timemachine::parse_interval("0s"), None);
    assert_eq!(timemachine::interval_text(7200), "2h");
    let c = Config::from_raw([0; 4]);
    assert!(c.enabled && c.interval_s == 900 && c.keep == 8 && c.daily == 14);

    // Automatic snapshots on a volume: none when nothing changed; retention applied;
    // the settings persist.
    let (d, fs) = Fs::fresh(32);
    fs.st()
        .set_auto_config(Config {
            enabled: true,
            interval_s: 30,
            keep: 3,
            daily: 1,
        })
        .unwrap();
    let now = |fs: &Fs| -> u64 {
        let _ = fs;
        crate::platform::now().sec as u64
    };
    let first = fs.st().auto_snapshot(now(&fs)).unwrap().unwrap();
    assert_eq!(first.name, "auto-2026-10-22T00:00:00");
    assert!(
        fs.st().auto_snapshot(now(&fs)).unwrap().is_none(),
        "nothing changed"
    );
    for i in 0..6 {
        advance(30);
        fs.write(&std::format!("/f{i}"), b"x").unwrap();
        fs.st().auto_snapshot(now(&fs)).unwrap().unwrap();
    }
    let autos = timemachine::autos(&fs.st());
    assert_eq!(
        autos.len(),
        3,
        "keep 3 (and the newest of today is one of them)"
    );
    // A manual snapshot is never pruned.
    fs.st().snap_create("mine").unwrap();
    advance(30);
    fs.write("/g", b"x").unwrap();
    fs.st().auto_snapshot(now(&fs)).unwrap();
    assert!(fs.st().snap_list().iter().any(|s| s.name == "mine"));
    fs.space_exact();
    drop(fs);
    let fs = Fs::mount(d).unwrap();
    assert_eq!(fs.st().auto_config().keep, 3);
    fs.check_clean();
}

#[test]
fn a_full_snapshot_table() {
    setup(65);
    let (_d, fs) = Fs::fresh(32);
    for i in 0..MAX_SNAPS {
        fs.write("/f", &[i as u8]).unwrap();
        fs.st().snap_create(&std::format!("s{i}")).unwrap();
    }
    assert_eq!(fs.st().snap_create("one-more").err(), Some(Errno::ENOSPC));
    assert_eq!(workload::clone(&fs, "s0", "c").err(), Some(Errno::ENOSPC));
    for i in 0..MAX_SNAPS {
        let g = fs.st().snap_list()[i].generation;
        assert_eq!(fs.read_in(g, "/f").unwrap(), [i as u8]);
    }
    fs.check_clean();
    fs.space_exact();
}
