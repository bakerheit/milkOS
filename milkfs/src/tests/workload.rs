//! The kernel crash test's workload (`kernel/src/fs/milkfs/crashtest.rs`, `crash-writer`)
//! and its check (`crash-check`), on the host: self-describing files written through a
//! temporary name and renamed, reflink copies, plain copies that dedupe then shares,
//! deletes, snapshots taken, deleted and rolled back to, clones written into and then
//! promoted or discarded. The check proves a volume is exactly some committed generation.

use std::string::String;
use std::vec::Vec;

use super::harness::*;
use crate::errno::{Errno, Result};
use crate::format::SNAP_NAME_MAX;
use crate::items::{LIVE, clone_view};

pub const DIR: &str = "/crash";
pub const SLOTS: u64 = 6;

/// The snapshot API around `State` the way the kernel's `milkfs::snapshot` & co. do it.
pub fn snapshot(fs: &Fs, name: &str) -> Result<()> {
    if fs.st().records_full() {
        return Err(Errno::ENOSPC);
    }
    fs.st().snap_create(name).map(|_| ())
}

pub fn rollback(fs: &Fs, name: &str) -> Result<()> {
    fs.st().not_converting()?;
    let zombies = {
        let mut st = fs.st();
        st.snap_rollback(name)?;
        st.unlinked_inodes()?
    };
    // `Volume::reclaim`: nothing is open here.
    if zombies.is_empty() {
        return Ok(());
    }
    fs.txn(LIVE, |st| {
        for &ino in &zombies {
            if st.inode(st.cur, ino).is_ok_and(|i| i.nlink == 0) {
                st.delete_inode(ino)?;
            }
        }
        Ok(())
    })
}

pub fn clone(fs: &Fs, snap: &str, name: &str) -> Result<u32> {
    fs.st().not_converting()?;
    if fs.st().records_full() {
        return Err(Errno::ENOSPC);
    }
    fs.st().clone_create(snap, name)
}

pub fn promote(fs: &Fs, name: &str) -> Result<String> {
    fs.st().not_converting()?;
    fs.st().clone_index_check(name)?;
    let mut keep = std::format!("before-{name}");
    keep.truncate(SNAP_NAME_MAX);
    fs.st().clone_promote(name, &keep)?;
    Ok(keep)
}

pub fn discard(fs: &Fs, name: &str) -> Result<()> {
    fs.st().clone_index_check(name)?;
    fs.st().clone_discard(name)
}

fn snap_names(fs: &Fs) -> Vec<String> {
    fs.st()
        .snap_list()
        .into_iter()
        .map(|s| s.name)
        .filter(|n| n.starts_with("crash-") || n.starts_with("before-"))
        .collect()
}

/// Where the writer starts on this volume (as the kernel does: generation x 1000).
pub fn first_iteration(fs: &Fs) -> u64 {
    fs.generation() * 1000
}

/// One iteration of the writer. Errors are ignored, as the kernel's writer logs and goes
/// on; after a power cut every one of them fails.
pub fn step(fs: &Fs, i: u64) {
    let k = i % SLOTS;
    let tmp = std::format!("{DIR}/.tmp-{k}");
    let fin = std::format!("{DIR}/f-{k}");
    let _ = fs
        .write(&tmp, &content(i))
        .and_then(|_| fs.rename(&tmp, &fin));
    // Shared blocks: reflink copies, plain copies deduped later, deletes.
    if i % 3 == 1 {
        let _ = fs.reflink(&fin, &std::format!("{DIR}/r-{k}"));
    }
    if i % 5 == 2
        && let Ok(d) = fs.read(&fin)
    {
        let _ = fs.write(&std::format!("{DIR}/d-{k}"), &d);
    }
    if i % 15 == 7 {
        let _ = fs.dedupe(DIR, false);
    }
    if i % 4 == 3 {
        let _ = fs.unlink(&std::format!("{DIR}/r-{}", (k + 1) % SLOTS));
        let _ = fs.unlink(&std::format!("{DIR}/d-{}", (k + 2) % SLOTS));
    }
    if i % 25 == 0 {
        let _ = snapshot(fs, &std::format!("crash-{i}"));
        // Keep at most 4, oldest first, skipping clone origins (EBUSY).
        let names = snap_names(fs);
        let mut excess = names.len().saturating_sub(4);
        for n in names {
            if excess == 0 {
                break;
            }
            if fs.st().snap_delete(&n).is_ok() {
                excess -= 1;
            }
        }
    }
    if i % 70 == 0
        && let Some(newest) = snap_names(fs).last()
    {
        let _ = rollback(fs, newest);
    }
    // Try-before-install: clone the newest snapshot, write into the clone, then keep it
    // (promote: the volume's history becomes branched) or throw it away.
    if i % 40 == 20
        && let Some(origin) = snap_names(fs)
            .into_iter()
            .rfind(|n| n.starts_with("crash-"))
    {
        let name = std::format!("cl-{i}");
        if let Ok(id) = clone(fs, &origin, &name) {
            let v = clone_view(id);
            let _ = fs.mkdir_all_in(v, DIR);
            let _ = fs.write_in(v, &std::format!("{DIR}/f-{}", i % SLOTS), &content(i));
            if (i / 40) % 2 != 0 || promote(fs, &name).is_err() {
                let _ = discard(fs, &name);
            }
        }
    }
    // The flusher commits what is a second old; the writer does about ten of these a
    // second.
    advance_ms(100);
    let _ = fs.st().flush_if_due();
}

/// Move the clocks on by `ms`.
pub fn advance_ms(ms: u64) {
    advance_ns(ms * 1_000_000);
}

/// Run the writer for `n` iterations from where this volume is.
pub fn run(fs: &Fs, n: u64) {
    let _ = fs.mkdir_all(DIR);
    let i0 = first_iteration(fs);
    for i in i0..i0 + n {
        step(fs, i);
    }
}

/// What a check found (the kernel's `CRASHCHECK` line).
#[derive(Debug, Default)]
pub struct Verdict {
    pub generation: u64,
    pub live_ok: u32,
    pub other_ok: u32,
    pub empty_tmp: u32,
    pub bad: Vec<String>,
    pub snapshots: usize,
    pub clones: usize,
    pub branched: bool,
}

fn check_dir(fs: &Fs, view: u64, path: &str, ok: &mut u32, v: &mut Verdict) {
    let Ok(names) = fs.ls_in(view, path) else {
        return;
    };
    for name in names {
        let p = std::format!("{path}/{name}");
        let Ok(ino) = fs.resolve(view, &p) else {
            continue;
        };
        if fs
            .st()
            .inode(view, ino)
            .map(|i| i.mode & crate::abi::S_IFMT)
            != Ok(crate::abi::S_IFREG)
        {
            continue;
        }
        match fs.read_ino(view, ino).map(|d| verify(&d)) {
            Ok(Some(true)) => *ok += 1,
            Ok(None) if name.starts_with(".tmp-") => v.empty_tmp += 1,
            r => v
                .bad
                .push(std::format!("{p} in view {view:#x} is bad ({r:?})")),
        }
    }
}

/// The crash check on a mounted volume: every file in the live tree, every snapshot and
/// every clone matches its own header, `check` is clean; then every clone discarded and
/// every snapshot deleted, space accounting must be exact (dead lists, or once branched,
/// sweeps, still right).
pub fn crash_check(fs: &Fs) -> Verdict {
    let mut v = Verdict {
        generation: fs.generation(),
        ..Verdict::default()
    };
    let mut ok = 0;
    check_dir(fs, LIVE, DIR, &mut ok, &mut v);
    v.live_ok = ok;
    let mut other = 0;
    let snaps = fs.st().snap_list();
    for s in &snaps {
        check_dir(fs, s.generation, DIR, &mut other, &mut v);
    }
    let clones = fs.st().clone_list();
    for c in &clones {
        check_dir(fs, clone_view(c.id), DIR, &mut other, &mut v);
    }
    v.other_ok = other;
    v.snapshots = snaps.len();
    v.clones = clones.len();
    match fs.st().check(false) {
        Ok(r) if r.clean() => {}
        Ok(r) => v.bad.push(std::format!("check: {}", r.text())),
        Err(e) => v.bad.push(std::format!("check: {e}")),
    }
    v.branched = fs.st().branched();
    for c in &clones {
        if let Err(e) = discard(fs, &c.name) {
            v.bad.push(std::format!("discard clone {}: {e}", c.name));
        }
    }
    let all = fs.st().snap_list();
    for s in all {
        if let Err(e) = fs.st().snap_delete(&s.name) {
            v.bad.push(std::format!("delete snapshot {}: {e}", s.name));
        }
    }
    match fs.st().verify_space() {
        Ok(c) if c.leaked == 0 && c.free_but_used == 0 => {}
        r => v.bad.push(std::format!("space: {r:?}")),
    }
    v
}
