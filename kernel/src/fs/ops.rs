//! The kernel-facing path API. Every `*_at` function takes an optional base directory for
//! relative paths (a process's cwd or a dirfd's inode); `None` means `/`. Absolute paths
//! ignore the base. Permission checks (rwx, sticky, ownership rules) are in fs/perm.rs and
//! are made for the calling process; kernel threads are root.

use alloc::string::String;
use alloc::vec::Vec;

use super::abi::{
    O_ACCMODE, O_CREAT, O_DIRECTORY, O_EXCL, O_NOCTTY, O_NOFOLLOW, O_PATH, O_RDONLY, O_TRUNC,
    O_WRONLY,
};
use super::errno::{Errno, Result};
use super::file::OpenFile;
use super::node::{DirEntry, InodeRef, InodeType, SetAttr, Stat, check_name};
use super::notify::{self, Event};
use super::path::{self, Walker};
use super::perm::{self, R, W};

/// `open(2)`. `mode` is used with O_CREAT (permission bits; apply the umask before).
pub fn open(path: &str, flags: u32, mode: u32) -> Result<OpenFile> {
    open_at(None, path, flags, mode)
}

/// `openat(2)` once the caller has turned `dirfd` into a base inode.
pub fn open_at(base: Option<&InodeRef>, path: &str, flags: u32, mode: u32) -> Result<OpenFile> {
    if flags & O_ACCMODE == O_ACCMODE {
        return Err(Errno::EINVAL);
    }
    let writable = flags & O_PATH == 0 && flags & O_ACCMODE != O_RDONLY;
    let mut w = Walker::new();
    let p = w.parent(base, path)?;
    let (mut dir, mut name, mut trailing) = (p.dir, p.name, p.trailing_slash);
    let mut created = false;
    let node = loop {
        match path::step(&dir, &name) {
            Ok(node) => {
                if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                    return Err(Errno::EEXIST);
                }
                // O_PATH | O_NOFOLLOW opens the link itself.
                let link_itself = flags & O_PATH != 0 && flags & O_NOFOLLOW != 0;
                if node.kind() == InodeType::Symlink && !link_itself {
                    if flags & O_NOFOLLOW != 0 {
                        return Err(Errno::ELOOP);
                    }
                    // Follow by re-resolving the target's parent, so O_CREAT through a
                    // dangling symlink creates the target (like Linux).
                    w.take_link()?;
                    let target = node.readlink()?;
                    let p2 = w.parent(Some(&dir), &target)?;
                    dir = p2.dir;
                    name = p2.name;
                    trailing |= p2.trailing_slash;
                    continue;
                }
                break node;
            }
            Err(Errno::ENOENT) if flags & O_CREAT != 0 => {
                if trailing {
                    return Err(Errno::EISDIR);
                }
                check_name(&name)?;
                perm::may(&dir, W)?;
                let n = dir.create(&name, mode & 0o7777)?;
                perm::own_new(&n, &dir);
                created = true;
                if notify::active() {
                    notify::emit(Event::Created {
                        dir: notify::id(&dir),
                        name: name.clone(),
                        obj: notify::id(&n),
                        kind: InodeType::File,
                    });
                }
                break n;
            }
            Err(e) => return Err(e),
        }
    };
    if (writable || flags & O_TRUNC != 0) && sealed(&node) {
        return Err(Errno::EPERM);
    }
    // The creator may open what it just made whatever its mode says (like Unix).
    if !created && flags & O_PATH == 0 {
        let mut mask = match flags & O_ACCMODE {
            O_RDONLY => R,
            O_WRONLY => W,
            _ => R | W,
        };
        if flags & O_TRUNC != 0 {
            mask |= W;
        }
        perm::may(&node, mask)?;
    }
    let is_dir = node.kind() == InodeType::Dir;
    if (trailing || flags & O_DIRECTORY != 0) && !is_dir {
        return Err(Errno::ENOTDIR);
    }
    if is_dir && (writable || flags & O_CREAT != 0) {
        return Err(Errno::EISDIR);
    }
    if flags & O_TRUNC != 0 && writable && node.kind() == InodeType::File {
        node.truncate(0)?;
        notify::emit(Event::Modified {
            obj: notify::id(&node),
        });
    }
    Ok(OpenFile::new(
        node,
        flags & !(O_CREAT | O_EXCL | O_TRUNC | O_NOCTTY),
    ))
}

/// Resolve a path to its inode (follows a final symlink). Handy for `chdir`.
pub fn lookup(path: &str) -> Result<InodeRef> {
    lookup_at(None, path, true)
}

pub fn lookup_at(base: Option<&InodeRef>, path: &str, follow: bool) -> Result<InodeRef> {
    Walker::new().resolve(base, path, follow)
}

/// `stat(2)`.
pub fn stat(path: &str) -> Result<Stat> {
    stat_at(None, path, true)
}

/// `lstat(2)`.
pub fn lstat(path: &str) -> Result<Stat> {
    stat_at(None, path, false)
}

/// `newfstatat(2)`; `follow == false` for AT_SYMLINK_NOFOLLOW.
pub fn stat_at(base: Option<&InodeRef>, path: &str, follow: bool) -> Result<Stat> {
    Ok(lookup_at(base, path, follow)?.stat())
}

/// Entries of a directory, without `.` and `..`, in the file system's order (sorted by
/// name for tmpfs).
pub fn readdir(path: &str) -> Result<Vec<DirEntry>> {
    let d = lookup(path)?;
    if d.kind() != InodeType::Dir {
        return Err(Errno::ENOTDIR);
    }
    d.readdir()
}

/// `mkdir(2)`.
pub fn mkdir(path: &str, mode: u32) -> Result<()> {
    mkdir_at(None, path, mode)
}

pub fn mkdir_at(base: Option<&InodeRef>, path: &str, mode: u32) -> Result<()> {
    let p = Walker::new().parent(base, path)?;
    if p.is_special() {
        return Err(Errno::EEXIST);
    }
    check_name(&p.name)?;
    perm::may(&p.dir, W)?;
    let n = p.dir.mkdir(&p.name, mode & 0o7777)?;
    perm::own_new(&n, &p.dir);
    notify::emit(Event::Created {
        dir: notify::id(&p.dir),
        name: p.name,
        obj: notify::id(&n),
        kind: InodeType::Dir,
    });
    Ok(())
}

/// `mkdir -p`: create `path` and any missing parents. Existing directories are fine.
pub fn mkdir_all(path: &str, mode: u32) -> Result<()> {
    let mut cur = String::new();
    for c in path.split('/').filter(|c| !c.is_empty()) {
        cur.push('/');
        cur.push_str(c);
        match mkdir(&cur, mode) {
            Ok(()) => {}
            Err(Errno::EEXIST) => {
                if lookup(&cur)?.kind() != InodeType::Dir {
                    return Err(Errno::ENOTDIR);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `unlink(2)`: remove a non-directory.
pub fn unlink(path: &str) -> Result<()> {
    unlink_at(None, path)
}

pub fn unlink_at(base: Option<&InodeRef>, path: &str) -> Result<()> {
    let p = Walker::new().parent(base, path)?;
    if p.is_special() {
        return Err(Errno::EISDIR);
    }
    if p.trailing_slash {
        let n = path::step(&p.dir, &p.name)?;
        return Err(if n.kind() == InodeType::Dir {
            Errno::EISDIR
        } else {
            Errno::ENOTDIR
        });
    }
    // Only its id: holding the inode across the unlink would keep it "open" (deferred
    // delete).
    let obj = match path::step_raw(&p.dir, &p.name) {
        Ok(n) if sealed(&n) => return Err(Errno::EPERM),
        Ok(n) => {
            perm::may_delete(&p.dir, &n)?;
            Some(notify::id(&n))
        }
        Err(_) => {
            perm::may(&p.dir, W)?;
            None
        }
    };
    p.dir.unlink(&p.name)?;
    if let Some(obj) = obj {
        notify::emit(Event::Removed {
            dir: notify::id(&p.dir),
            name: p.name,
            obj,
        });
    }
    Ok(())
}

/// `rmdir(2)`: remove an empty directory.
pub fn rmdir(path: &str) -> Result<()> {
    rmdir_at(None, path)
}

pub fn rmdir_at(base: Option<&InodeRef>, path: &str) -> Result<()> {
    let p = Walker::new().parent(base, path)?;
    match p.name.as_str() {
        "" => return Err(Errno::EBUSY),
        "." => return Err(Errno::EINVAL),
        ".." => return Err(Errno::ENOTEMPTY),
        _ => {}
    }
    let raw = path::step_raw(&p.dir, &p.name)?;
    if path::is_mount_boundary(&raw) {
        return Err(Errno::EBUSY);
    }
    perm::may_delete(&p.dir, &raw)?;
    let obj = notify::id(&raw);
    drop(raw);
    p.dir.rmdir(&p.name)?;
    notify::emit(Event::Removed {
        dir: notify::id(&p.dir),
        name: p.name,
        obj,
    });
    Ok(())
}

/// `rename(2)`.
pub fn rename(old: &str, new: &str) -> Result<()> {
    rename_at(None, old, None, new)
}

/// `renameat(2)` (flags 0).
pub fn rename_at(
    old_base: Option<&InodeRef>,
    old: &str,
    new_base: Option<&InodeRef>,
    new: &str,
) -> Result<()> {
    let po = Walker::new().parent(old_base, old)?;
    let pn = Walker::new().parent(new_base, new)?;
    if po.is_special() || pn.is_special() {
        return Err(Errno::EBUSY);
    }
    check_name(&pn.name)?;
    if po.dir.dev() != pn.dir.dev() {
        return Err(Errno::EXDEV);
    }
    let src = path::step_raw(&po.dir, &po.name)?;
    if path::is_mount_boundary(&src) {
        return Err(Errno::EBUSY);
    }
    if (po.trailing_slash || pn.trailing_slash) && src.kind() != InodeType::Dir {
        return Err(Errno::ENOTDIR);
    }
    if let Ok(dst) = path::step_raw(&pn.dir, &pn.name)
        && path::is_mount_boundary(&dst)
    {
        return Err(Errno::EBUSY);
    }
    if sealed(&src) || path::step_raw(&pn.dir, &pn.name).is_ok_and(|n| sealed(&n)) {
        return Err(Errno::EPERM);
    }
    perm::may_delete(&po.dir, &src)?;
    match path::step_raw(&pn.dir, &pn.name) {
        Ok(dst) => perm::may_delete(&pn.dir, &dst)?,
        Err(_) => perm::may(&pn.dir, W)?,
    }
    // A directory moving to another parent rewrites its `..`: needs write on it too.
    if src.kind() == InodeType::Dir && !alloc::sync::Arc::ptr_eq(&po.dir, &pn.dir) {
        perm::may(&src, W)?;
    }
    po.dir.rename(&po.name, &pn.dir, &pn.name)?;
    notify::emit(Event::Renamed {
        from_dir: notify::id(&po.dir),
        from: po.name,
        to_dir: notify::id(&pn.dir),
        to: pn.name,
        obj: notify::id(&src),
    });
    Ok(())
}

/// `symlink(2)`: create `linkpath` pointing at `target` (not resolved now).
pub fn symlink(target: &str, linkpath: &str) -> Result<()> {
    symlink_at(target, None, linkpath)
}

pub fn symlink_at(target: &str, base: Option<&InodeRef>, linkpath: &str) -> Result<()> {
    if target.is_empty() {
        return Err(Errno::ENOENT);
    }
    let p = Walker::new().parent(base, linkpath)?;
    if p.is_special() {
        return Err(Errno::EEXIST);
    }
    check_name(&p.name)?;
    perm::may(&p.dir, W)?;
    let n = p.dir.symlink(&p.name, target)?;
    perm::own_new(&n, &p.dir);
    notify::emit(Event::Created {
        dir: notify::id(&p.dir),
        name: p.name,
        obj: notify::id(&n),
        kind: InodeType::Symlink,
    });
    Ok(())
}

/// `mknod(2)` for a socket or FIFO name (AF_UNIX `bind`): needs write on the directory;
/// the new node is the caller's. EEXIST if the name exists (Linux's EADDRINUSE is the
/// caller's to report).
pub fn mknod_at(
    base: Option<&InodeRef>,
    path: &str,
    kind: InodeType,
    mode: u32,
) -> Result<InodeRef> {
    let p = Walker::new().parent(base, path)?;
    if p.is_special() {
        return Err(Errno::EEXIST);
    }
    check_name(&p.name)?;
    perm::may(&p.dir, W)?;
    let n = p.dir.mknod(&p.name, kind, mode & 0o7777)?;
    perm::own_new(&n, &p.dir);
    notify::emit(Event::Created {
        dir: notify::id(&p.dir),
        name: p.name,
        obj: notify::id(&n),
        kind,
    });
    Ok(n)
}

/// `readlink(2)`.
pub fn readlink(path: &str) -> Result<String> {
    readlink_at(None, path)
}

pub fn readlink_at(base: Option<&InodeRef>, path: &str) -> Result<String> {
    lookup_at(base, path, false)?.readlink()
}

/// `link(2)`: hard link `new` to the file at `old`.
pub fn link(old: &str, new: &str) -> Result<()> {
    link_at(None, old, None, new, false)
}

/// `linkat(2)`; `follow` for AT_SYMLINK_FOLLOW.
pub fn link_at(
    old_base: Option<&InodeRef>,
    old: &str,
    new_base: Option<&InodeRef>,
    new: &str,
    follow: bool,
) -> Result<()> {
    let src = lookup_at(old_base, old, follow)?;
    if src.kind() == InodeType::Dir {
        return Err(Errno::EPERM);
    }
    let p = Walker::new().parent(new_base, new)?;
    if p.is_special() {
        return Err(Errno::EEXIST);
    }
    check_name(&p.name)?;
    if p.dir.dev() != src.dev() {
        return Err(Errno::EXDEV);
    }
    perm::may(&p.dir, W)?;
    p.dir.link(&p.name, &src)?;
    notify::emit(Event::Created {
        dir: notify::id(&p.dir),
        name: p.name,
        obj: notify::id(&src),
        kind: src.kind(),
    });
    Ok(())
}

/// The audit log file is append-only for the kernel: user processes may not open it for
/// writing, truncate, rename, replace or unlink it (EPERM). See `crate::audit`.
fn sealed(node: &InodeRef) -> bool {
    crate::audit::sealed(node) && crate::audit::user_context()
}

/// `truncate(2)`.
pub fn truncate(path: &str, len: u64) -> Result<()> {
    let n = lookup(path)?;
    if sealed(&n) {
        return Err(Errno::EPERM);
    }
    perm::may(&n, W)?;
    match n.kind() {
        InodeType::Dir => Err(Errno::EISDIR),
        InodeType::File => {
            n.truncate(len)?;
            notify::emit(Event::Modified {
                obj: notify::id(&n),
            });
            Ok(())
        }
        _ => Err(Errno::EINVAL),
    }
}

/// chmod / chown / utimensat.
pub fn setattr_at(base: Option<&InodeRef>, path: &str, follow: bool, attr: &SetAttr) -> Result<()> {
    let n = lookup_at(base, path, follow)?;
    let a = perm::check_setattr(&n, attr)?;
    n.setattr(&a)?;
    notify::emit(Event::Attr {
        obj: notify::id(&n),
    });
    Ok(())
}

pub fn chmod(path: &str, perm: u32) -> Result<()> {
    setattr_at(
        None,
        path,
        true,
        &SetAttr {
            perm: Some(perm & 0o7777),
            ..SetAttr::default()
        },
    )
}

/// Copy the tree at `src` into directory `dst` (created if missing): files, directories,
/// symlinks. Existing files in `dst` are replaced.
pub fn copy_tree(src: &str, dst: &str) -> Result<()> {
    mkdir_all(dst, 0o755)?;
    for e in readdir(src)? {
        let (s, d) = (
            alloc::format!("{src}/{}", e.name),
            alloc::format!("{dst}/{}", e.name),
        );
        match e.kind {
            InodeType::Dir => copy_tree(&s, &d)?,
            InodeType::Symlink => match symlink(&readlink(&s)?, &d) {
                Ok(()) | Err(Errno::EEXIST) => {}
                Err(err) => return Err(err),
            },
            InodeType::File => {
                write_file(&d, &read_file(&s)?)?;
                let _ = chmod(&d, lstat(&s)?.perm);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Read a whole file (reads until EOF, so synthetic files with size 0 work too).
/// `FICLONE`: `dst` (open for writing) gets a copy-on-write copy of `src` (open for
/// reading). EBADF for wrong modes; the file system decides the rest (milkfs: EXDEV across
/// volumes or views, EINVAL for non-files; anything else: EOPNOTSUPP).
pub fn ficlone(src: &OpenFile, dst: &OpenFile) -> Result<()> {
    if !src.readable() || !dst.writable() {
        return Err(Errno::EBADF);
    }
    super::milkfs::inode::reflink(src.inode(), dst.inode())?;
    notify::emit(Event::Modified {
        obj: notify::id(dst.inode()),
    });
    Ok(())
}

pub fn read_file(path: &str) -> Result<Vec<u8>> {
    let f = open(path, O_RDONLY, 0)?;
    let mut out = Vec::with_capacity(f.stat().size as usize);
    let mut chunk = [0u8; 512];
    loop {
        let n = f.read(&mut chunk)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

/// [`read_file`] as UTF-8 (lossy).
pub fn read_to_string(path: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&read_file(path)?).into_owned())
}

/// Create or replace a file with `data`.
pub fn write_file(path: &str, data: &[u8]) -> Result<()> {
    let f = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)?;
    let mut done = 0;
    while done < data.len() {
        match f.write(&data[done..])? {
            0 => return Err(Errno::EIO),
            n => done += n,
        }
    }
    Ok(())
}
