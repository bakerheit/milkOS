//! Mount table and path resolution.
//!
//! A mount covers a directory inode (identified by `(dev, ino)`) with another file
//! system's root. Walking down into a covered directory lands on the mounted root; `..`
//! from a mounted root climbs back to the covered directory first. Symlinks are followed
//! with Linux's limits: at most [`MAX_LINKS`] per resolution and [`MAX_NESTING`] nested.

use alloc::string::String;
use alloc::vec::Vec;

use super::errno::{Errno, Result};
use super::node::{FsRef, InodeRef, InodeType, NAME_MAX, PATH_MAX};
use crate::sync::SpinLock;

pub const MAX_LINKS: u32 = 40;
pub const MAX_NESTING: u32 = 8;

struct Mount {
    /// The directory this mount hides. `None` for the root mount.
    covered: Option<InodeRef>,
    root: InodeRef,
    fs: FsRef,
    path: String,
}

static MOUNTS: SpinLock<Vec<Mount>> = SpinLock::new(Vec::new());

fn same(a: &InodeRef, b: &InodeRef) -> bool {
    a.ino() == b.ino() && a.dev() == b.dev()
}

/// The root directory of the whole tree. Panics before `fs::init`.
pub fn root() -> InodeRef {
    MOUNTS
        .lock()
        .first()
        .map(|m| m.root.clone())
        .expect("fs: no root mounted")
}

/// Root of the most recent mount covering `dir`, if any.
fn mounted_on(dir: &InodeRef) -> Option<InodeRef> {
    let m = MOUNTS.lock();
    m.iter()
        .rev()
        .find(|m| m.covered.as_ref().is_some_and(|c| same(c, dir)))
        .map(|m| m.root.clone())
}

/// If `dir` is the root of a non-root mount, the directory it covers.
fn covered_by(dir: &InodeRef) -> Option<InodeRef> {
    let m = MOUNTS.lock();
    m.iter()
        .rev()
        .find(|m| same(&m.root, dir))
        .and_then(|m| m.covered.clone())
}

/// Is `inode` a mount point (covered) or the root of a mount? Such directories can't be
/// removed or renamed (EBUSY).
pub fn is_mount_boundary(inode: &InodeRef) -> bool {
    let m = MOUNTS.lock();
    m.iter()
        .any(|m| same(&m.root, inode) || m.covered.as_ref().is_some_and(|c| same(c, inode)))
}

/// Mount `fs` at `path`. The first mount must be `/`.
pub fn mount(path: &str, fs: FsRef) -> Result<()> {
    let first = MOUNTS.lock().is_empty();
    let covered = if first {
        if path != "/" {
            return Err(Errno::EINVAL);
        }
        None
    } else {
        let dir = Walker::new().resolve(None, path, true)?;
        if dir.kind() != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        Some(dir)
    };
    let root = fs.root();
    MOUNTS.lock().push(Mount {
        covered,
        root,
        fs,
        path: String::from(path),
    });
    Ok(())
}

/// One line of `/proc/mounts`: (mount point, fs type, device number).
pub struct MountInfo {
    pub path: String,
    pub fs_name: &'static str,
    pub dev: u64,
}

pub fn mounts() -> Vec<MountInfo> {
    MOUNTS
        .lock()
        .iter()
        .map(|m| MountInfo {
            path: m.path.clone(),
            fs_name: m.fs.name(),
            dev: m.fs.dev(),
        })
        .collect()
}

/// Look up one component in `dir`, crossing mount points both ways.
pub fn step(dir: &InodeRef, name: &str) -> Result<InodeRef> {
    if dir.kind() != InodeType::Dir {
        return Err(Errno::ENOTDIR);
    }
    let next = match name {
        "" | "." => return Ok(dir.clone()),
        ".." => {
            let mut d = dir.clone();
            while let Some(c) = covered_by(&d) {
                d = c;
            }
            d.lookup("..")?
        }
        _ => {
            if name.len() > NAME_MAX {
                return Err(Errno::ENAMETOOLONG);
            }
            dir.lookup(name)?
        }
    };
    let mut next = next;
    while let Some(r) = mounted_on(&next) {
        next = r;
    }
    Ok(next)
}

/// Like [`step`] but without crossing into a mount: the raw entry in `dir`'s own fs.
pub fn step_raw(dir: &InodeRef, name: &str) -> Result<InodeRef> {
    if dir.kind() != InodeType::Dir {
        return Err(Errno::ENOTDIR);
    }
    dir.lookup(name)
}

/// The directory a path's last component lives in, plus that component.
pub struct Parent {
    pub dir: InodeRef,
    /// Last component. Empty for `/`, can be `.` or `..`.
    pub name: String,
    /// The path ended in `/` (so the target must be a directory).
    pub trailing_slash: bool,
}

impl Parent {
    /// True if the last component is not a normal name (`/`, `.`, `..`).
    pub fn is_special(&self) -> bool {
        matches!(self.name.as_str(), "" | "." | "..")
    }
}

/// Path walker; carries the symlink budget across nested resolutions.
pub struct Walker {
    links: u32,
    depth: u32,
}

impl Default for Walker {
    fn default() -> Self {
        Self::new()
    }
}

impl Walker {
    pub fn new() -> Walker {
        Walker {
            links: MAX_LINKS,
            depth: 0,
        }
    }

    /// Count one symlink traversal against the budget.
    pub fn take_link(&mut self) -> Result<()> {
        if self.links == 0 {
            return Err(Errno::ELOOP);
        }
        self.links -= 1;
        Ok(())
    }

    /// Resolve everything but the last component of `path`, relative to `base` (or the
    /// root when `base` is `None` or the path is absolute).
    pub fn parent(&mut self, base: Option<&InodeRef>, path: &str) -> Result<Parent> {
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }
        if path.len() >= PATH_MAX {
            return Err(Errno::ENAMETOOLONG);
        }
        let mut cur = match base {
            Some(b) if !path.starts_with('/') => b.clone(),
            _ => root(),
        };
        let trailing_slash = path.ends_with('/');
        let mut comps = path.split('/').filter(|c| !c.is_empty()).peekable();
        let mut last = "";
        while let Some(c) = comps.next() {
            if comps.peek().is_none() {
                last = c;
                break;
            }
            let next = step(&cur, c)?;
            cur = if next.kind() == InodeType::Symlink {
                self.follow(&cur, &next)?
            } else {
                next
            };
        }
        if cur.kind() != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        if last.len() > NAME_MAX {
            return Err(Errno::ENAMETOOLONG);
        }
        Ok(Parent {
            dir: cur,
            name: String::from(last),
            trailing_slash,
        })
    }

    /// Resolve `path` to an inode. `follow`: follow a symlink in the last component.
    pub fn resolve(
        &mut self,
        base: Option<&InodeRef>,
        path: &str,
        follow: bool,
    ) -> Result<InodeRef> {
        let p = self.parent(base, path)?;
        let node = step(&p.dir, &p.name)?;
        let node = if node.kind() == InodeType::Symlink && (follow || p.trailing_slash) {
            self.follow(&p.dir, &node)?
        } else {
            node
        };
        if p.trailing_slash && node.kind() != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        Ok(node)
    }

    /// Resolve the target of symlink `link`, which lives in `dir`.
    pub fn follow(&mut self, dir: &InodeRef, link: &InodeRef) -> Result<InodeRef> {
        self.take_link()?;
        if self.depth >= MAX_NESTING {
            return Err(Errno::ELOOP);
        }
        let target = link.readlink()?;
        if target.is_empty() {
            return Err(Errno::ENOENT);
        }
        self.depth += 1;
        let r = self.resolve(Some(dir), &target, true);
        self.depth -= 1;
        r
    }
}

/// Absolute path of a directory (for `getcwd`) or any inode still linked in the tree.
/// Walks `..` and searches each parent for the child's inode number.
pub fn path_of(inode: &InodeRef) -> Result<String> {
    let root = root();
    let mut names: Vec<String> = Vec::new();
    let mut cur = inode.clone();
    loop {
        while let Some(c) = covered_by(&cur) {
            cur = c;
        }
        if same(&cur, &root) {
            break;
        }
        let parent = cur.lookup("..")?;
        if same(&parent, &cur) {
            break; // root of a detached fs
        }
        let name = parent
            .readdir()?
            .into_iter()
            .find(|e| e.ino == cur.ino())
            .ok_or(Errno::ENOENT)?
            .name;
        names.push(name);
        cur = parent;
    }
    if names.is_empty() {
        return Ok(String::from("/"));
    }
    let mut out = String::new();
    for n in names.iter().rev() {
        out.push('/');
        out.push_str(n);
    }
    Ok(out)
}
