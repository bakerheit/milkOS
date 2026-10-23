//! The file system side of `files`, `trash` and `trashd` (the rules are in `filekit`):
//! which Trash something goes to, moving it there and back, listing and emptying, copying
//! and deleting whole folders. Errors come back as plain-English messages.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use filekit::path::{self, Kind};
use filekit::trash::{self as tk, INFO_DIR, Info, Keep, TRASH_DIR};
use filekit::words::{say, say_read};
use rt::sys::{self, Stat};

/// Seconds since 1970 (milkOS time).
pub fn now() -> i64 {
    (sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000) as i64
}

pub fn kind(st: &Stat) -> Kind {
    Kind::from_mode(st.mode)
}

/// Something is there (not following a final link).
pub fn exists(p: &str) -> bool {
    sys::stat_path(p, false).is_ok()
}

pub fn is_folder(p: &str) -> bool {
    sys::stat_path(p, true).is_ok_and(|s| s.is_dir())
}

/// The caller's home folder: $HOME, else the account's.
pub fn home(args: &rt::Args) -> String {
    args.env("HOME")
        .filter(|h| h.starts_with('/'))
        .map(String::from)
        .or_else(|| rt::users::by_uid(sys::geteuid()).map(|p| p.home))
        .unwrap_or_else(|| String::from("/"))
}

pub fn cwd() -> String {
    sys::getcwd().unwrap_or_else(|_| String::from("/"))
}

/// `shown` in double quotes when it has spaces or shell characters in it, for a command the
/// user can copy.
pub fn quote(shown: &str) -> String {
    if shown
        .chars()
        .any(|c| c.is_whitespace() || "'\"$*?;&|<>()\\".contains(c))
    {
        format!("\"{}\"", shown.replace('"', "\\\""))
    } else {
        String::from(shown)
    }
}

/// The top folder of the file system `abs` is on (where that disk is mounted). `dev` is
/// `abs`'s own device number.
pub fn volume_root(abs: &str, dev: u64) -> String {
    let mut cur = String::from(abs);
    while cur != "/" {
        let up = path::parent(&cur);
        match sys::stat_path(&up, true) {
            Ok(s) if s.dev == dev => cur = up,
            _ => break,
        }
    }
    cur
}

/// Is `abs` a Trash (yours, or a disk's)?
pub fn is_a_trash(abs: &str, home: &str) -> bool {
    abs == path::join(home, TRASH_DIR) || {
        let p: Vec<&str> = abs.split('/').collect();
        p.len() == 4 && p[1] == "Volumes" && p[3] == TRASH_DIR
    }
}

/// If `abs` is in a Trash already: (that Trash, the item's name there, whether `abs` is the
/// item itself rather than something inside it).
pub fn trash_of(abs: &str, home: &str) -> Option<(String, String, bool)> {
    let parts: Vec<&str> = abs.split('/').collect();
    for i in 1..parts.len().saturating_sub(1) {
        let dir = parts[..=i].join("/");
        if parts[i] == TRASH_DIR && is_a_trash(&dir, home) {
            return Some((dir, String::from(parts[i + 1]), i + 2 == parts.len()));
        }
    }
    None
}

/// Which Trash something at `abs` (lstat `st`) goes to: (the Trash folder, shared). Your
/// home's Trash if it is on the same disk as your home folder, else that disk's own
/// `/Volumes/<disk>/.Trash` (shared: everyone's, each item's metadata says whose), so
/// nothing is ever copied between disks. Err: why there is none, in plain English.
pub fn trash_for(abs: &str, shown: &str, st: &Stat, home: &str) -> Result<(String, bool), String> {
    if let Ok(h) = sys::stat_path(home, true)
        && h.is_dir()
        && h.dev == st.dev
    {
        return Ok((path::join(home, TRASH_DIR), false));
    }
    let root = volume_root(abs, st.dev);
    let fs = sys::statfs(&root).ok();
    if fs.is_some_and(|f| f.read_only()) {
        return Err(format!(
            "{shown} is on a read-only disk ({root}), so it can't be deleted"
        ));
    }
    let ty = fs.map_or("?", |f| f.type_name());
    let a_disk = root.starts_with("/Volumes/") && root.matches('/').count() == 2;
    if a_disk && matches!(ty, "milkfs" | "tmpfs") {
        return Ok((path::join(&root, TRASH_DIR), true));
    }
    let why = match ty {
        "vfat" => format!("{root} is a FAT disk, which has no Trash"),
        "9p" => format!("{root} is the Mac's shared folder, which has no Trash"),
        "tmpfs" => String::from(
            "it is in memory (gone at the next restart anyway), where there is no Trash",
        ),
        "proc" | "sysfs" | "devfs" => {
            format!("{root} is made up by the system, not stored anywhere")
        }
        _ => format!("{root} has no Trash"),
    };
    Err(format!(
        "{shown} can't go to the Trash: {why}. To delete it for good: files delete --now {}",
        quote(shown)
    ))
}

/// Make a Trash and its metadata folder if they are missing: 0700 in a home (owned by the
/// home's owner, even when root makes it), 1777 (sticky, like /tmp) on a disk.
pub fn ensure_trash(dir: &str, shared: bool, home: &str) -> Result<(), String> {
    let info = path::join(dir, INFO_DIR);
    let mode = if shared { 0o1777 } else { 0o700 };
    for d in [dir, info.as_str()] {
        match sys::mkdir(d, mode) {
            Ok(()) => {
                let _ = sys::chmod(d, mode);
                if !shared
                    && sys::geteuid() == 0
                    && let Ok(h) = sys::stat_path(home, true)
                    && h.uid != 0
                {
                    let _ = sys::chown(d, h.uid, h.gid);
                }
            }
            Err(e) if e.0 == 17 && is_folder(d) => {}
            Err(e) => return Err(format!("can't make the Trash: {}", say(d, e.0))),
        }
    }
    Ok(())
}

/// Move `abs` (lstat `st`; `shown` as typed) into its Trash: write its metadata
/// (create-exclusive: two deletes never pick the same name), then rename it in (never
/// replacing anything). Returns its name in the Trash.
pub fn move_to_trash(abs: &str, shown: &str, st: &Stat, home: &str) -> Result<String, String> {
    let (dir, shared) = trash_for(abs, shown, st, home)?;
    ensure_trash(&dir, shared, home)?;
    let info_dir = path::join(&dir, INFO_DIR);
    let text = Info {
        path: String::from(abs),
        deleted: now(),
        uid: sys::getuid(),
    }
    .encode();
    let owner = sys::stat_path(&dir, true).ok();
    for _ in 0..64 {
        let name = tk::free_name(path::basename(abs), |n| {
            exists(&path::join(&dir, n)) || exists(&path::join(&info_dir, n))
        });
        let ip = path::join(&info_dir, &name);
        let fd = match sys::open_mode(
            &ip,
            sys::O_WRONLY | sys::O_CREAT | sys::O_EXCL | sys::O_CLOEXEC,
            0o600,
        ) {
            Ok(fd) => fd,
            Err(e) if e.0 == 17 => continue,
            Err(e) => return Err(format!("can't write to the Trash: {}", say(&ip, e.0))),
        };
        let wrote = sys::write_all(fd, text.as_bytes());
        let _ = sys::close(fd);
        if let Err(e) = wrote {
            let _ = sys::unlink(&ip);
            return Err(format!("can't write to the Trash: {}", say(&ip, e.0)));
        }
        // Root deleting into someone's home Trash: the record is theirs, like the Trash.
        if !shared
            && sys::geteuid() == 0
            && let Some(o) = owner
            && o.uid != 0
        {
            let _ = sys::chown(&ip, o.uid, o.gid);
        }
        match sys::rename_noreplace(abs, &path::join(&dir, &name)) {
            Ok(()) => return Ok(name),
            Err(e) if e.0 == 17 => {
                let _ = sys::unlink(&ip);
            }
            Err(e) => {
                let _ = sys::unlink(&ip);
                return Err(say(shown, e.0));
            }
        }
    }
    Err(format!("{shown}: no free name in the Trash"))
}

/// One thing in a Trash.
pub struct Item {
    /// The Trash folder it is in.
    pub trash: String,
    /// Its name there.
    pub name: String,
    /// Where it came from and when (None: the metadata is missing).
    pub info: Option<Info>,
    pub st: Stat,
}

impl Item {
    pub fn path(&self) -> String {
        path::join(&self.trash, &self.name)
    }
    pub fn info_path(&self) -> String {
        path::join(&path::join(&self.trash, INFO_DIR), &self.name)
    }
    /// When it was deleted (no metadata: when it last changed, which a rename sets).
    pub fn deleted(&self) -> i64 {
        self.info.as_ref().map_or(self.st.ctime, |i| i.deleted)
    }
    /// Whose it is: who deleted it (no metadata: its owner).
    pub fn uid(&self) -> u32 {
        self.info.as_ref().map_or(self.st.uid, |i| i.uid)
    }
}

/// Every item in the Trash folder `dir`.
pub fn read_trash(dir: &str) -> Vec<Item> {
    let Ok(list) = sys::read_dir(dir) else {
        return Vec::new();
    };
    let info_dir = path::join(dir, INFO_DIR);
    let mut v = Vec::new();
    for (name, _) in list {
        if name == "." || name == ".." || tk::reserved(&name) {
            continue;
        }
        let Ok(st) = sys::stat_path(&path::join(dir, &name), false) else {
            continue;
        };
        let info = sys::read_file(&path::join(&info_dir, &name))
            .ok()
            .and_then(|b| Info::decode(&String::from_utf8_lossy(&b)));
        v.push(Item {
            trash: String::from(dir),
            name,
            info,
            st,
        });
    }
    v
}

/// The disks' own Trashes that exist (`/Volumes/<disk>/.Trash`).
pub fn disk_trashes() -> Vec<String> {
    let mut v = Vec::new();
    for (n, _) in sys::read_dir("/Volumes").unwrap_or_default() {
        if n == "." || n == ".." {
            continue;
        }
        let t = path::join(&path::join("/Volumes", &n), TRASH_DIR);
        if sys::stat_path(&t, false).is_ok_and(|s| s.is_dir()) {
            v.push(t);
        }
    }
    v
}

/// The Trash folders that exist for someone with home `home`: theirs, then each disk's,
/// as (folder, shared).
pub fn trashes(home: &str) -> Vec<(String, bool)> {
    let mut v = Vec::new();
    let mine = path::join(home, TRASH_DIR);
    if is_folder(&mine) {
        v.push((mine, false));
    }
    for t in disk_trashes() {
        if !v.iter().any(|(d, _)| *d == t) {
            v.push((t, true));
        }
    }
    v
}

/// Everything in your Trash (home, plus your items in the disks' Trashes; root sees all of
/// theirs too), newest first.
pub fn my_items(home: &str) -> Vec<Item> {
    let uid = sys::getuid();
    let mut v = Vec::new();
    for (dir, shared) in trashes(home) {
        v.extend(
            read_trash(&dir)
                .into_iter()
                .filter(|it| !shared || uid == 0 || it.uid() == uid),
        );
    }
    v.sort_by_key(|it| core::cmp::Reverse(it.deleted()));
    v
}

/// The settings for how long things stay: `trash = ...` in `<home>/.config/milk/settings`.
pub fn keep_for(home: &str) -> Keep {
    let f = path::join(home, ".config/milk/settings");
    sys::read_file(&f).map_or(Keep::DEFAULT, |b| {
        Keep::from_settings(&String::from_utf8_lossy(&b))
    })
}

/// Put an item back where it came from. Refuses if something is there now. Returns where.
pub fn restore(it: &Item) -> Result<String, String> {
    let Some(info) = &it.info else {
        return Err(format!(
            "{} has no record of where it came from; move it out yourself: files move {} FOLDER",
            it.name,
            quote(&it.path())
        ));
    };
    let to = info.path.clone();
    let taken = || {
        format!(
            "{to} already exists, so {} can't go back there; move or rename that one first",
            it.name
        )
    };
    if exists(&to) {
        return Err(taken());
    }
    let up = path::parent(&to);
    if !is_folder(&up) {
        make_folders(&up)?;
    }
    match sys::rename_noreplace(&it.path(), &to) {
        Ok(()) => {
            let _ = sys::unlink(&it.info_path());
            Ok(to)
        }
        Err(e) if e.0 == 17 => Err(taken()),
        Err(e) if e.0 == 18 => Err(format!(
            "{up} is on another disk now; move {} out yourself: files move {} {}",
            it.name,
            quote(&it.path()),
            quote(&up)
        )),
        Err(e) => Err(say(&to, e.0)),
    }
}

/// Delete an item for good (and its metadata).
pub fn delete_item(it: &Item) -> Result<(), String> {
    if exists(&it.path()) {
        remove_tree(&it.path())?;
    }
    let _ = sys::unlink(&it.info_path());
    Ok(())
}

/// `mkdir -p`: every missing folder on the way. A file in the way is an error.
pub fn make_folders(p: &str) -> Result<(), String> {
    let mut acc = String::new();
    for part in p.split('/') {
        if part.is_empty() {
            if acc.is_empty() {
                acc.push('/');
            }
            continue;
        }
        if !acc.is_empty() && !acc.ends_with('/') {
            acc.push('/');
        }
        acc.push_str(part);
        match sys::mkdir(&acc, 0o755) {
            Ok(()) => {}
            // Already there. (The kernel checks write permission on the parent before it
            // looks, so an existing folder in one you can't write, /tmp in /, is EACCES.)
            Err(_) if is_folder(&acc) => {}
            Err(e) if e.0 == 17 => return Err(format!("{acc} is a file, not a folder")),
            Err(e) => return Err(say(&acc, e.0)),
        }
    }
    Ok(())
}

/// Delete for good: a file or link, or a folder with everything in it (links are removed,
/// never followed). Keeps going past failures; the first one is the error.
pub fn remove_tree(p: &str) -> Result<(), String> {
    let st = sys::stat_path(p, false).map_err(|e| say(p, e.0))?;
    if !st.is_dir() {
        return sys::unlink(p).map_err(|e| say(p, e.0));
    }
    let mut first: Option<String> = None;
    match sys::read_dir(p) {
        Ok(list) => {
            for (n, _) in list {
                if n == "." || n == ".." {
                    continue;
                }
                if let Err(m) = remove_tree(&path::join(p, &n)) {
                    first.get_or_insert(m);
                }
            }
        }
        Err(e) => first = Some(say_read(p, e.0)),
    }
    if let Some(m) = first {
        return Err(m);
    }
    sys::rmdir(p).map_err(|e| say(p, e.0))
}

/// Bytes in a file, or in everything in a folder (links count as themselves).
pub fn tree_size(p: &str) -> u64 {
    let Ok(st) = sys::stat_path(p, false) else {
        return 0;
    };
    if !st.is_dir() {
        return st.size;
    }
    sys::read_dir(p).map_or(0, |list| {
        list.into_iter()
            .filter(|(n, _)| n != "." && n != "..")
            .map(|(n, _)| tree_size(&path::join(p, &n)))
            .sum()
    })
}

/// Copy-on-write copies (`files copy --reflink`): milkfs only, both ends on one disk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reflink {
    Never,
    /// Share blocks when possible, else copy.
    Auto,
    /// Share blocks or fail.
    Always,
}

#[derive(Clone, Copy)]
pub struct CopyOpts {
    pub reflink: Reflink,
    /// Overwrite a file already at the destination.
    pub replace: bool,
}

fn reflink_why(e: i32) -> &'static str {
    match e {
        18 => "it is on another disk",
        95 | 25 | 22 => "copy-on-write copies only work on milkfs",
        16 => "the disk is busy (being converted)",
        _ => "the disk said no",
    }
}

fn copy_file(src: &str, dst: &str, perm: u32, o: &CopyOpts) -> Result<(), String> {
    let inf = sys::open(src, sys::O_RDONLY | sys::O_CLOEXEC).map_err(|e| say_read(src, e.0))?;
    let excl = if o.replace { sys::O_TRUNC } else { sys::O_EXCL };
    let outf = match sys::open_mode(
        dst,
        sys::O_WRONLY | sys::O_CREAT | sys::O_CLOEXEC | excl,
        perm,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = sys::close(inf);
            return Err(say(dst, e.0));
        }
    };
    let done = |r: Result<(), String>| {
        let _ = (sys::close(inf), sys::close(outf));
        r
    };
    if o.reflink != Reflink::Never {
        match sys::ficlone(inf, outf) {
            Ok(()) => return done(Ok(())),
            Err(e) if o.reflink == Reflink::Always => {
                let _ = sys::unlink(dst);
                return done(Err(format!(
                    "can't reflink {dst}: {} ({})",
                    reflink_why(e.0),
                    e.name()
                )));
            }
            Err(_) => {}
        }
    }
    let mut buf = alloc::vec![0u8; 64 * 1024];
    loop {
        match sys::read(inf, &mut buf) {
            Ok(0) => return done(Ok(())),
            Ok(n) => {
                if let Err(e) = sys::write_all(outf, &buf[..n]) {
                    return done(Err(say(dst, e.0)));
                }
            }
            Err(e) if e.0 == 4 => {}
            Err(e) => return done(Err(say_read(src, e.0))),
        }
    }
}

/// Copy `src` to `dst` (which must not exist, unless `replace` and it is a file): a file, a
/// link (as a link), or a folder and everything in it. Stops at the first failure.
pub fn copy_tree(src: &str, dst: &str, o: &CopyOpts) -> Result<(), String> {
    let st = sys::stat_path(src, false).map_err(|e| say_read(src, e.0))?;
    match kind(&st) {
        Kind::Link => {
            let target = sys::readlink(src).map_err(|e| say_read(src, e.0))?;
            if o.replace && sys::stat_path(dst, false).is_ok_and(|d| !d.is_dir()) {
                let _ = sys::unlink(dst);
            }
            sys::symlink(&target, dst).map_err(|e| say(dst, e.0))
        }
        Kind::Folder => {
            sys::mkdir(dst, (st.mode & 0o7777) | 0o700).map_err(|e| say(dst, e.0))?;
            let list = sys::read_dir(src).map_err(|e| say_read(src, e.0))?;
            for (n, _) in list {
                if n == "." || n == ".." {
                    continue;
                }
                copy_tree(&path::join(src, &n), &path::join(dst, &n), o)?;
            }
            let _ = sys::chmod(dst, st.mode & 0o7777);
            Ok(())
        }
        Kind::File => copy_file(src, dst, st.mode & 0o7777, o),
        Kind::Special => Err(format!(
            "{src} is a special file (a device, pipe or socket) and can't be copied"
        )),
    }
}

/// What `trashd` did to one Trash: (deleted for good, failures, metadata files cleaned up).
pub struct Expired {
    pub deleted: usize,
    pub failed: Vec<String>,
    pub cleaned: usize,
}

/// Delete for good everything in Trash `dir` that `keep` says is due at `now` (only `uid`'s
/// items, if given), and metadata files left without an item by a crash (an hour old, so a
/// delete in progress keeps its record).
pub fn expire(dir: &str, keep: Keep, now: i64, uid: Option<u32>) -> Expired {
    let mut r = Expired {
        deleted: 0,
        failed: Vec::new(),
        cleaned: 0,
    };
    let items = read_trash(dir);
    for it in &items {
        if uid.is_some_and(|u| it.uid() != u) || !keep.expired(it.deleted(), now) {
            continue;
        }
        match delete_item(it) {
            Ok(()) => r.deleted += 1,
            Err(m) => r.failed.push(m),
        }
    }
    let info_dir = path::join(dir, INFO_DIR);
    for (n, _) in sys::read_dir(&info_dir).unwrap_or_default() {
        if n == "." || n == ".." || exists(&path::join(dir, &n)) {
            continue;
        }
        let ip = path::join(&info_dir, &n);
        let Ok(st) = sys::stat_path(&ip, false) else {
            continue;
        };
        if uid.is_some_and(|u| st.uid != u) || now - st.mtime < 3600 {
            continue;
        }
        if sys::unlink(&ip).is_ok() {
            r.cleaned += 1;
        }
    }
    r
}
