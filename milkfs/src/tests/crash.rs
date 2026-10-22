//! Crash consistency (`scripts/crashtest.sh`): the writer runs on a disk that loses power,
//! then what is left must mount and be exactly some committed generation (the last one,
//! or the one in flight), every file whole, `check` clean, space exact after deleting
//! every snapshot and clone.
//!
//! Rounds like the guest test's (kill, power cut with a torn request, a cut inside a
//! superblock write), each continuing from what the previous round's check left; a sweep
//! of cut points over one whole stretch of the workload; and cuts on a drive with a
//! volatile write cache that keeps any subset of what wasn't flushed, in any order (the
//! guest test can't do that one: qemu writes in order).

use std::vec::Vec;

use super::harness::*;
use super::workload::{self, Verdict};

/// A volume the writer has been at for a while (files, reflinks, dedupe, snapshots, a
/// rollback, clones promoted and discarded).
fn base(seed: u64, iterations: u64) -> Vec<u8> {
    setup(seed);
    let (d, fs) = Fs::fresh(64);
    workload::run(&fs, iterations);
    fs.commit().unwrap();
    drop(fs);
    d.image()
}

/// What a crash round saw.
struct Crash {
    /// The last generation whose commit returned (what a clean shutdown would have).
    last: u64,
    /// The generation that mounted afterwards.
    mounted: u64,
    died: bool,
    verdict: Verdict,
    /// The image after the check (the next round starts from it, like crashtest.sh).
    after: Vec<u8>,
}

/// Mount `img` on a disk that loses power as `cut` says, run the writer until it does
/// (at most `max` iterations), then mount what is left and check it.
fn crash(img: Vec<u8>, cut: Cut, max: u64) -> Crash {
    let disk = CrashDisk::new(img, cut);
    let fs = Fs::mount(disk.clone()).expect("mount before the crash");
    let _ = fs.mkdir_all(workload::DIR);
    fs.commit().unwrap();
    disk.arm();
    let i0 = workload::first_iteration(&fs);
    for i in i0..i0 + max {
        workload::step(&fs, i);
        if disk.dead() {
            break;
        }
    }
    let last = fs.generation();
    let died = disk.dead();
    drop(fs);
    let left = MemDisk::from_image(disk.image());
    let fs = Fs::mount(left.clone()).expect("what the crash left mounts");
    let mounted = fs.generation();
    let verdict = workload::crash_check(&fs);
    drop(fs);
    Crash {
        last,
        mounted,
        died,
        verdict,
        after: left.image(),
    }
}

fn assert_good(what: &str, c: &Crash) {
    assert!(c.verdict.bad.is_empty(), "{what}: {:#?}", c.verdict);
    assert!(
        c.mounted == c.last || c.mounted == c.last + 1,
        "{what}: mounted generation {}, last committed {}",
        c.mounted,
        c.last
    );
}

#[test]
fn kill_and_power_cut_rounds() {
    // crashtest.sh's default: 4 kill rounds, 4 power cuts, 4 superblock cuts; more here.
    let mut img = base(30, 120);
    let mut cuts = 0;
    for r in 0..12 {
        let n = 50 + rand() % 3000;
        let c = crash(img, Cut::Writes(n), 400);
        assert_good(&std::format!("kill round {r} ({n} writes)"), &c);
        cuts += c.died as u32;
        img = c.after;
    }
    for r in 0..12 {
        let n = 1 + rand() % 40_000;
        let c = crash(img, Cut::Sectors(n), 400);
        assert_good(&std::format!("power cut round {r} ({n} sectors)"), &c);
        cuts += c.died as u32;
        img = c.after;
    }
    assert!(cuts >= 12, "the power went in most rounds ({cuts})");
    // The volume is still good for normal use.
    let fs = Fs::mount(MemDisk::from_image(img)).unwrap();
    workload::run(&fs, 30);
    fs.commit().unwrap();
    let v = workload::crash_check(&fs);
    assert!(v.bad.is_empty(), "{v:#?}");
}

#[test]
fn cuts_inside_superblock_writes() {
    // T of the 8 sectors land: torn (T < 8) mounts the previous generation, landed (T = 8,
    // but the commit never returned) the new one.
    const TEARS: [u64; 8] = [8, 1, 4, 7, 0, 8, 3, 6];
    let mut img = base(31, 80);
    for r in 0..16 {
        let k = 2 + rand() % 40;
        let t = TEARS[r % TEARS.len()];
        let c = crash(img, Cut::Superblock(k, t), 600);
        assert!(c.died, "round {r}: no superblock write {k}");
        assert_good(&std::format!("superblock round {r} (write {k}, {t}/8)"), &c);
        let want = if t == 8 { c.last + 1 } else { c.last };
        assert_eq!(
            c.mounted, want,
            "round {r}: write {k}, {t}/8 sectors landed"
        );
        img = c.after;
    }
}

#[test]
fn a_sweep_of_cut_points() {
    // One stretch of the workload, replayed identically from the same image and cut at
    // evenly spaced points over everything it writes (torn requests included).
    const STRETCH: u64 = 60;
    let img = base(32, 60);
    let total = {
        let disk = CrashDisk::new(img.clone(), Cut::Sectors(u64::MAX));
        setup(33);
        let fs = Fs::mount(disk.clone()).unwrap();
        let _ = fs.mkdir_all(workload::DIR);
        fs.commit().unwrap();
        disk.arm();
        let i0 = workload::first_iteration(&fs);
        for i in i0..i0 + STRETCH {
            workload::step(&fs, i);
        }
        disk.sectors_written()
    };
    assert!(
        total > 5_000,
        "the stretch writes something ({total} sectors)"
    );
    const POINTS: u64 = 48;
    for p in 1..=POINTS {
        setup(33);
        let n = total * p / (POINTS + 1) + p % 7;
        let c = crash(img.clone(), Cut::Sectors(n), STRETCH);
        assert!(c.died, "cut at sector {n} of {total}");
        assert_good(&std::format!("cut at sector {n} of {total}"), &c);
    }
}

#[test]
fn a_volatile_write_cache() {
    // Writes since the last flush land in any subset and order (torn, too): a commit is
    // only durable once its flushes returned, which is all milkfs relies on.
    let mut img = base(34, 80);
    for r in 0..24 {
        let n = 1 + rand() % 30_000;
        let seed = rand();
        let c = crash(img, Cut::Cache { sectors: n, seed }, 400);
        assert_good(
            &std::format!("cache round {r} ({n} sectors, seed {seed:#x})"),
            &c,
        );
        img = c.after;
    }
}
