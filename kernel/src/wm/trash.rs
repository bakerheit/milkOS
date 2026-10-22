//! The Trash for the desktop (Files, the dock): the same Trash as `files delete` and `trash`
//! (user/filetools), with the same rules from `filekit` (docs/filesystem.md, "Files and
//! the Trash"). Deleting moves an item into ~/.Trash, or into its disk's own
//! `/Volumes/<disk>/.Trash`, by a rename (never a copy), after writing its metadata file
//! create-exclusive. The desktop runs as a kernel thread, which is root, and new files take
//! their folder's owner, so ~/.Trash and its records stay the user's.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use filekit::path;
use filekit::trash::{self as tk, INFO_DIR, Info, Keep, TRASH_DIR};
use filekit::words::say;

use crate::fs::{self, InodeType, Stat};

use super::apps::fsx::HOME;

/// Is there anything in the Trash (the dock's full / empty icon)? Updated by [`refresh`].
static FULL: AtomicBool = AtomicBool::new(false);

pub fn full() -> bool {
    FULL.load(Ordering::Relaxed)
}

/// Look again (cheap: one or two directory reads). True if full / empty changed.
pub fn refresh() -> bool {
    let now = !items().is_empty();
    FULL.swap(now, Ordering::Relaxed) != now
}

/// What the dock last drew.
static DRAWN: AtomicBool = AtomicBool::new(false);

/// Did full / empty change since the dock last asked? (The desktop redraws the dock then.)
pub fn take_change() -> bool {
    let f = full();
    DRAWN.swap(f, Ordering::Relaxed) != f
}

fn now() -> i64 {
    fs::clock::now().sec
}

fn exists(p: &str) -> bool {
    fs::lstat(p).is_ok()
}

fn is_dir(p: &str) -> bool {
    fs::stat(p).is_ok_and(|s| s.kind == InodeType::Dir)
}

/// The user's uid (the home folder's owner).
fn me() -> u32 {
    fs::stat(HOME).map_or(0, |s| s.uid)
}

pub fn home_trash() -> String {
    path::join(HOME, TRASH_DIR)
}

/// How long things stay: Settings > Trash.
pub fn keep() -> Keep {
    super::prefs::get().trash
}

/// The top folder of the disk `abs` is on.
fn volume_root(abs: &str, dev: u64) -> String {
    let mut cur = String::from(abs);
    while cur != "/" {
        let up = path::parent(&cur);
        match fs::stat(&up) {
            Ok(s) if s.dev == dev => cur = up,
            _ => break,
        }
    }
    cur
}

/// Which Trash `abs` goes to: (folder, shared); Err says why there is none.
fn trash_for(abs: &str, st: &Stat) -> Result<(String, bool), String> {
    let name = path::basename(abs);
    if fs::stat(HOME).is_ok_and(|h| h.dev == st.dev) {
        return Ok((home_trash(), false));
    }
    let root = volume_root(abs, st.dev);
    if root == abs {
        return Err(format!("{name} is a disk; eject it instead"));
    }
    let sf = fs::statfs(&root).ok();
    if sf.is_some_and(|f| f.flags & crate::fs::node::ST_RDONLY != 0) {
        return Err(format!("{name} is on a read-only disk"));
    }
    let ty = sf.map_or(0, |f| f.fs_type);
    let a_disk = root.starts_with("/Volumes/") && root.matches('/').count() == 2;
    use crate::fs::node::{MILKFS_MAGIC, MSDOS_MAGIC, TMPFS_MAGIC, V9FS_MAGIC};
    if a_disk && (ty == MILKFS_MAGIC || ty == TMPFS_MAGIC) {
        return Ok((path::join(&root, TRASH_DIR), true));
    }
    Err(match ty {
        MSDOS_MAGIC => format!("{name} is on a FAT disk, which has no Trash"),
        V9FS_MAGIC => format!("{name} is in the Mac's shared folder, which has no Trash"),
        _ => format!("{name} is in memory, where there is no Trash"),
    })
}

fn ensure(dir: &str, shared: bool) -> Result<(), String> {
    let mode = if shared { 0o1777 } else { 0o700 };
    for d in [String::from(dir), path::join(dir, INFO_DIR)] {
        match fs::mkdir(&d, mode) {
            Ok(()) => {
                let _ = fs::chmod(&d, mode);
            }
            Err(_) if is_dir(&d) => {}
            Err(e) => return Err(format!("can't make the Trash: {e}")),
        }
    }
    Ok(())
}

/// Move `abs` to its Trash. Returns its name there; Err is a sentence for a toast.
pub fn move_to_trash(abs: &str) -> Result<String, String> {
    let st = fs::lstat(abs).map_err(|e| say(path::basename(abs), e.0))?;
    let (dir, shared) = trash_for(abs, &st)?;
    ensure(&dir, shared)?;
    let info_dir = path::join(&dir, INFO_DIR);
    let text = Info {
        path: String::from(abs),
        deleted: now(),
        uid: me(),
    }
    .encode();
    for _ in 0..64 {
        let name = tk::free_name(path::basename(abs), |n| {
            exists(&path::join(&dir, n)) || exists(&path::join(&info_dir, n))
        });
        let ip = path::join(&info_dir, &name);
        let f = match fs::open(
            &ip,
            fs::abi::O_WRONLY | fs::abi::O_CREAT | fs::abi::O_EXCL,
            0o600,
        ) {
            Ok(f) => f,
            Err(e) if e.0 == 17 => continue,
            Err(e) => return Err(format!("can't write to the Trash: {e}")),
        };
        if let Err(e) = f.write(text.as_bytes()) {
            drop(f);
            let _ = fs::unlink(&ip);
            return Err(format!("can't write to the Trash: {e}"));
        }
        drop(f);
        match fs::rename_noreplace(abs, &path::join(&dir, &name)) {
            Ok(()) => {
                refresh();
                return Ok(name);
            }
            Err(e) => {
                let _ = fs::unlink(&ip);
                if e.0 != 17 {
                    return Err(say(path::basename(abs), e.0));
                }
            }
        }
    }
    Err(String::from("no free name in the Trash"))
}

/// One thing in the Trash.
#[derive(Clone)]
pub struct Item {
    pub trash: String,
    pub name: String,
    pub info: Option<Info>,
    pub dir: bool,
    pub size: u64,
    pub ctime: i64,
}

impl Item {
    pub fn path(&self) -> String {
        path::join(&self.trash, &self.name)
    }
    fn info_path(&self) -> String {
        path::join(&path::join(&self.trash, INFO_DIR), &self.name)
    }
    pub fn deleted(&self) -> i64 {
        self.info.as_ref().map_or(self.ctime, |i| i.deleted)
    }
    /// Where it came from ("" if nobody knows).
    pub fn from(&self) -> &str {
        self.info.as_ref().map_or("", |i| i.path.as_str())
    }
}

/// Bytes in everything in a folder (links count as themselves).
fn tree_size(p: &str) -> u64 {
    fs::readdir(p).map_or(0, |list| {
        list.into_iter()
            .map(|d| {
                let q = path::join(p, &d.name);
                match fs::lstat(&q) {
                    Ok(s) if s.kind == InodeType::Dir => tree_size(&q),
                    Ok(s) => s.size,
                    Err(_) => 0,
                }
            })
            .sum()
    })
}

fn read(dir: &str, only: Option<u32>) -> Vec<Item> {
    let Ok(list) = fs::readdir(dir) else {
        return Vec::new();
    };
    let info_dir = path::join(dir, INFO_DIR);
    let mut v = Vec::new();
    for d in list {
        if tk::reserved(&d.name) {
            continue;
        }
        let Ok(st) = fs::lstat(&path::join(dir, &d.name)) else {
            continue;
        };
        let info = fs::read_to_string(&path::join(&info_dir, &d.name))
            .ok()
            .and_then(|t| Info::decode(&t));
        let uid = info.as_ref().map_or(st.uid, |i| i.uid);
        let size = if st.kind == InodeType::Dir {
            tree_size(&path::join(dir, &d.name))
        } else {
            st.size
        };
        if only.is_some_and(|u| u != uid) {
            continue;
        }
        v.push(Item {
            trash: String::from(dir),
            name: d.name,
            info,
            dir: st.kind == InodeType::Dir,
            size,
            ctime: st.ctime.sec,
        });
    }
    v
}

/// Everything in the user's Trash (home, and their items in the disks' Trashes), newest
/// first.
pub fn items() -> Vec<Item> {
    let mut v = read(&home_trash(), None);
    let uid = me();
    if let Ok(vols) = fs::readdir("/Volumes") {
        for d in vols {
            let t = format!("/Volumes/{}/{TRASH_DIR}", d.name);
            if fs::lstat(&t).is_ok_and(|s| s.kind == InodeType::Dir) && t != home_trash() {
                v.extend(read(&t, Some(uid)));
            }
        }
    }
    v.sort_by_key(|i| core::cmp::Reverse(i.deleted()));
    v
}

fn mkdir_all(p: &str) -> Result<(), String> {
    fs::mkdir_all(p, 0o755).map_err(|e| say(p, e.0))
}

/// Put an item back. Refuses (Err, a sentence) if something is there now.
pub fn restore(it: &Item) -> Result<String, String> {
    let Some(info) = &it.info else {
        return Err(format!(
            "Nobody knows where {} came from: drag it out of the Trash",
            it.name
        ));
    };
    let to = info.path.clone();
    let taken = || {
        format!(
            "{} is back in its place already: move or rename that first",
            path::basename(&to)
        )
    };
    if exists(&to) {
        return Err(taken());
    }
    let up = path::parent(&to);
    if !is_dir(&up) {
        mkdir_all(&up)?;
    }
    match fs::rename_noreplace(&it.path(), &to) {
        Ok(()) => {
            let _ = fs::unlink(&it.info_path());
            refresh();
            Ok(to)
        }
        Err(e) if e.0 == 17 => Err(taken()),
        Err(e) => Err(say(&to, e.0)),
    }
}

/// Delete for good, everything inside too (links removed, never followed).
fn remove_tree(p: &str) -> Result<(), String> {
    let st = fs::lstat(p).map_err(|e| say(p, e.0))?;
    if st.kind != InodeType::Dir {
        return fs::unlink(p).map_err(|e| say(p, e.0));
    }
    let mut first = None;
    for d in fs::readdir(p).unwrap_or_default() {
        if let Err(m) = remove_tree(&path::join(p, &d.name)) {
            first.get_or_insert(m);
        }
    }
    if let Some(m) = first {
        return Err(m);
    }
    fs::rmdir(p).map_err(|e| say(p, e.0))
}

/// Delete one item for good.
pub fn delete(it: &Item) -> Result<(), String> {
    if exists(&it.path()) {
        remove_tree(&it.path())?;
    }
    let _ = fs::unlink(&it.info_path());
    refresh();
    Ok(())
}

/// Empty the Trash: (deleted, failed).
pub fn empty() -> (usize, usize) {
    let (mut ok, mut bad) = (0, 0);
    for it in items() {
        if delete(&it).is_ok() {
            ok += 1;
        } else {
            bad += 1;
        }
    }
    let _ = fs::sync_all();
    refresh();
    (ok, bad)
}

/// Days until `it` goes for good (None: never).
pub fn days_left(it: &Item) -> Option<i64> {
    keep().days_left(it.deleted(), now())
}

/// "today 10:02" etc. for when it was deleted.
pub fn when(it: &Item) -> String {
    filekit::date::when_short(it.deleted(), now())
}
