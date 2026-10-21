//! The kernel-facing path API. Every `*_at` function takes an optional base directory for
//! relative paths (a process's cwd or a dirfd's inode); `None` means `/`. Absolute paths
//! ignore the base. No permission checks yet: everything runs as root.

use alloc::string::String;
use alloc::vec::Vec;

use super::abi::{
    O_ACCMODE, O_CREAT, O_DIRECTORY, O_EXCL, O_NOCTTY, O_NOFOLLOW, O_PATH, O_RDONLY, O_TRUNC,
    O_WRONLY,
};
use super::errno::{Errno, Result};
use super::file::OpenFile;
use super::node::{DirEntry, InodeRef, InodeType, SetAttr, Stat, check_name};
use super::path::{self, Walker};

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
                break dir.create(&name, mode & 0o7777)?;
            }
            Err(e) => return Err(e),
        }
    };
    let is_dir = node.kind() == InodeType::Dir;
    if (trailing || flags & O_DIRECTORY != 0) && !is_dir {
        return Err(Errno::ENOTDIR);
    }
    if is_dir && (writable || flags & O_CREAT != 0) {
        return Err(Errno::EISDIR);
    }
    if flags & O_TRUNC != 0 && writable && node.kind() == InodeType::File {
        node.truncate(0)?;
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
    p.dir.mkdir(&p.name, mode & 0o7777).map(|_| ())
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
    p.dir.unlink(&p.name)
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
    p.dir.rmdir(&p.name)
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
    po.dir.rename(&po.name, &pn.dir, &pn.name)
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
    p.dir.symlink(&p.name, target).map(|_| ())
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
    p.dir.link(&p.name, &src)
}

/// `truncate(2)`.
pub fn truncate(path: &str, len: u64) -> Result<()> {
    let n = lookup(path)?;
    match n.kind() {
        InodeType::Dir => Err(Errno::EISDIR),
        InodeType::File => n.truncate(len),
        _ => Err(Errno::EINVAL),
    }
}

/// chmod / chown / utimensat.
pub fn setattr_at(base: Option<&InodeRef>, path: &str, follow: bool, attr: &SetAttr) -> Result<()> {
    lookup_at(base, path, follow)?.setattr(attr)
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

/// Read a whole file (reads until EOF, so synthetic files with size 0 work too).
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
