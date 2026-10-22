//! tmpfs: a RAM file system.
//!
//! File data lives in whole 4 KiB physical frames straight from the PMM (one `Option` per
//! page, so files can be sparse and growing never reallocates a big buffer). Frames are
//! page-aligned, which is what a future `mmap` of a tmpfs file wants. Directories are a
//! `BTreeMap` (sorted names). Each node has its own `SpinLock`; every namespace change
//! (create, mkdir, link, unlink, rmdir, rename) also holds the fs-wide `ns` lock, which is
//! what makes multi-node changes like rename atomic and keeps lock nesting deadlock-free.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use super::clock;
use super::errno::{Errno, Result};
use super::node::{
    DirEntry, FileSystem, Inode, InodeRef, InodeType, SetAttr, Stat, Timespec, alloc_dev,
    check_name, downcast,
};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::pmm;
use crate::sync::SpinLock;

/// Largest file tmpfs accepts (keeps the page vector of a sparse file bounded).
pub const MAX_FILE_SIZE: u64 = 1 << 30;

struct Shared {
    dev: u64,
    next_ino: AtomicU64,
    /// Data pages in use, and the cap (0 = no cap beyond free RAM).
    pages: AtomicU64,
    max_pages: u64,
    ns: SpinLock<()>,
}

pub struct TmpFs {
    shared: Arc<Shared>,
    root: Arc<TmpNode>,
}

impl TmpFs {
    /// A new, empty tmpfs. `max_bytes == 0`: limited only by free memory.
    pub fn new(max_bytes: u64) -> Arc<TmpFs> {
        let shared = Arc::new(Shared {
            dev: alloc_dev(),
            next_ino: AtomicU64::new(1),
            pages: AtomicU64::new(0),
            max_pages: max_bytes.div_ceil(PAGE_SIZE as u64),
            ns: SpinLock::new(()),
        });
        let root = TmpNode::new(&shared, InodeType::Dir, 0o755, None, |_| Data::Dir {
            entries: BTreeMap::new(),
            parent: Weak::new(),
        });
        Arc::new(TmpFs { shared, root })
    }

    /// Bytes of file data currently stored.
    pub fn used_bytes(&self) -> u64 {
        self.shared.pages.load(Ordering::Relaxed) * PAGE_SIZE as u64
    }
}

impl FileSystem for TmpFs {
    fn name(&self) -> &'static str {
        "tmpfs"
    }
    fn dev(&self) -> u64 {
        self.shared.dev
    }
    fn root(&self) -> InodeRef {
        self.root.clone()
    }
    /// Pages: the cap (or, without one, what is used plus the free RAM), free = what is
    /// left of that.
    fn statfs(&self) -> Result<super::node::StatFs> {
        let used = self.shared.pages.load(Ordering::Relaxed);
        let ram_free = pmm::stats().free_frames as u64;
        let (blocks, free) = match self.shared.max_pages {
            0 => (used + ram_free, ram_free),
            cap => (cap, cap.saturating_sub(used).min(ram_free)),
        };
        let mut s = super::node::StatFs::empty(super::node::TMPFS_MAGIC, self.shared.dev);
        s.bsize = PAGE_SIZE as u64;
        s.blocks = blocks;
        s.bfree = free;
        s.bavail = free;
        Ok(s)
    }
}

enum Data {
    File {
        size: u64,
        pages: Vec<Option<PhysAddr>>,
    },
    Dir {
        entries: BTreeMap<String, Arc<TmpNode>>,
        /// Empty for the root, which is its own parent.
        parent: Weak<TmpNode>,
    },
    Symlink(String),
    /// A socket or FIFO name: no contents (fs::node::Inode::mknod).
    Special,
}

struct Inner {
    perm: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    atime: Timespec,
    mtime: Timespec,
    ctime: Timespec,
    data: Data,
}

pub struct TmpNode {
    ino: u64,
    kind: InodeType,
    shared: Arc<Shared>,
    me: Weak<TmpNode>,
    inner: SpinLock<Inner>,
}

impl TmpNode {
    fn new(
        shared: &Arc<Shared>,
        kind: InodeType,
        perm: u32,
        parent: Option<&TmpNode>,
        data: impl FnOnce(&Weak<TmpNode>) -> Data,
    ) -> Arc<TmpNode> {
        let now = clock::now();
        let (uid, gid) = parent.map_or((0, 0), |p| {
            let i = p.inner.lock();
            (i.uid, i.gid)
        });
        Arc::new_cyclic(|me| TmpNode {
            ino: shared.next_ino.fetch_add(1, Ordering::Relaxed),
            kind,
            shared: shared.clone(),
            me: me.clone(),
            inner: SpinLock::new(Inner {
                perm,
                uid,
                gid,
                nlink: if kind == InodeType::Dir { 2 } else { 1 },
                atime: now,
                mtime: now,
                ctime: now,
                data: data(me),
            }),
        })
    }

    fn arc(&self) -> Arc<TmpNode> {
        self.me.upgrade().expect("tmpfs: node used after drop")
    }

    fn child(&self, name: &str) -> Result<Arc<TmpNode>> {
        match &self.inner.lock().data {
            Data::Dir { entries, .. } => entries.get(name).cloned().ok_or(Errno::ENOENT),
            _ => Err(Errno::ENOTDIR),
        }
    }

    fn parent_node(&self) -> Arc<TmpNode> {
        match &self.inner.lock().data {
            Data::Dir { parent, .. } => parent.upgrade(),
            _ => None,
        }
        .unwrap_or_else(|| self.arc())
    }

    /// Add a new node `name` (built by `make`) to this directory.
    fn add(
        &self,
        name: &str,
        kind: InodeType,
        perm: u32,
        make: impl FnOnce(&Weak<TmpNode>) -> Data,
    ) -> Result<InodeRef> {
        check_name(name)?;
        let _ns = self.shared.ns.lock();
        if self.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        if self.child(name).is_ok() {
            return Err(Errno::EEXIST);
        }
        let node = TmpNode::new(&self.shared, kind, perm, Some(self), make);
        let mut me = self.inner.lock();
        let now = clock::now();
        me.mtime = now;
        me.ctime = now;
        if kind == InodeType::Dir {
            me.nlink += 1;
        }
        if let Data::Dir { entries, .. } = &mut me.data {
            entries.insert(String::from(name), node.clone());
        }
        Ok(node)
    }

    fn touch_dir(&self) {
        let now = clock::now();
        let mut i = self.inner.lock();
        i.mtime = now;
        i.ctime = now;
    }

    fn reserve_page(&self) -> Result<PhysAddr> {
        let s = &self.shared;
        if s.max_pages != 0 && s.pages.load(Ordering::Relaxed) >= s.max_pages {
            return Err(Errno::ENOSPC);
        }
        let pa = pmm::alloc_frame_zeroed_as(pmm::Use::Files).ok_or(Errno::ENOSPC)?;
        s.pages.fetch_add(1, Ordering::Relaxed);
        Ok(pa)
    }

    fn release_page(&self, pa: PhysAddr) {
        pmm::free_frame(pa);
        self.shared.pages.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Drop for TmpNode {
    fn drop(&mut self) {
        let pages = match &mut self.inner.lock().data {
            Data::File { pages, .. } => core::mem::take(pages),
            _ => return,
        };
        for pa in pages.into_iter().flatten() {
            self.release_page(pa);
        }
    }
}

fn page_ptr(pa: PhysAddr) -> *mut u8 {
    phys_to_virt(pa) as *mut u8
}

impl Inode for TmpNode {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.shared.dev
    }
    fn kind(&self) -> InodeType {
        self.kind
    }

    fn stat(&self) -> Stat {
        let i = self.inner.lock();
        let mut st = Stat::new(self.shared.dev, self.ino, self.kind, i.perm);
        st.nlink = i.nlink;
        st.uid = i.uid;
        st.gid = i.gid;
        st.atime = i.atime;
        st.mtime = i.mtime;
        st.ctime = i.ctime;
        (st.size, st.blocks) = match &i.data {
            Data::File { size, pages } => (
                *size,
                pages.iter().flatten().count() as u64 * (PAGE_SIZE as u64 / 512),
            ),
            Data::Dir { .. } => (PAGE_SIZE as u64, 0),
            Data::Symlink(t) => (t.len() as u64, 0),
            Data::Special => (0, 0),
        };
        st
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        let i = self.inner.lock();
        let Data::File { size, pages } = &i.data else {
            return Err(if self.kind == InodeType::Dir {
                Errno::EISDIR
            } else {
                Errno::EINVAL
            });
        };
        if off >= *size {
            return Ok(0);
        }
        let n = buf.len().min((*size - off) as usize);
        let mut done = 0;
        while done < n {
            let pos = off as usize + done;
            let (pg, in_pg) = (pos / PAGE_SIZE, pos % PAGE_SIZE);
            let chunk = (PAGE_SIZE - in_pg).min(n - done);
            let dst = &mut buf[done..done + chunk];
            match pages.get(pg).copied().flatten() {
                // SAFETY: the frame is ours and PAGE_SIZE long; in_pg + chunk <= PAGE_SIZE.
                Some(pa) => unsafe {
                    ptr::copy_nonoverlapping(page_ptr(pa).add(in_pg), dst.as_mut_ptr(), chunk)
                },
                None => dst.fill(0), // hole
            }
            done += chunk;
        }
        Ok(n)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize> {
        let end = off.checked_add(buf.len() as u64).ok_or(Errno::EFBIG)?;
        if end > MAX_FILE_SIZE {
            return Err(Errno::EFBIG);
        }
        let mut i = self.inner.lock();
        let i = &mut *i;
        let Data::File { size, pages } = &mut i.data else {
            return Err(if self.kind == InodeType::Dir {
                Errno::EISDIR
            } else {
                Errno::EINVAL
            });
        };
        let need = (end as usize).div_ceil(PAGE_SIZE);
        if pages.len() < need {
            pages.resize(need, None);
        }
        let mut done = 0;
        while done < buf.len() {
            let pos = off as usize + done;
            let (pg, in_pg) = (pos / PAGE_SIZE, pos % PAGE_SIZE);
            let chunk = (PAGE_SIZE - in_pg).min(buf.len() - done);
            let pa = match pages[pg] {
                Some(pa) => pa,
                None => match self.reserve_page() {
                    Ok(pa) => {
                        pages[pg] = Some(pa);
                        pa
                    }
                    Err(e) if done == 0 => {
                        // Nothing written: drop the slots we added past the old size.
                        pages.truncate((*size as usize).div_ceil(PAGE_SIZE));
                        return Err(e);
                    }
                    Err(_) => break,
                },
            };
            // SAFETY: as in read_at.
            unsafe {
                ptr::copy_nonoverlapping(buf[done..].as_ptr(), page_ptr(pa).add(in_pg), chunk)
            };
            done += chunk;
        }
        *size = (*size).max(off + done as u64);
        pages.truncate((*size as usize).div_ceil(PAGE_SIZE));
        let now = clock::now();
        i.mtime = now;
        i.ctime = now;
        Ok(done)
    }

    fn truncate(&self, len: u64) -> Result<()> {
        if len > MAX_FILE_SIZE {
            return Err(Errno::EFBIG);
        }
        let mut freed = Vec::new();
        {
            let mut i = self.inner.lock();
            let i = &mut *i;
            let Data::File { size, pages } = &mut i.data else {
                return Err(if self.kind == InodeType::Dir {
                    Errno::EISDIR
                } else {
                    Errno::EINVAL
                });
            };
            if len < *size {
                let keep = (len as usize).div_ceil(PAGE_SIZE);
                freed.extend(pages.drain(keep.min(pages.len())..).flatten());
                // Zero the tail of the last kept page so a later grow reads zeros.
                let tail = len as usize % PAGE_SIZE;
                if tail != 0
                    && let Some(Some(pa)) = pages.get(keep - 1)
                {
                    // SAFETY: tail < PAGE_SIZE, frame is ours.
                    unsafe { ptr::write_bytes(page_ptr(*pa).add(tail), 0, PAGE_SIZE - tail) };
                }
            }
            *size = len;
            let now = clock::now();
            i.mtime = now;
            i.ctime = now;
        }
        for pa in freed {
            self.release_page(pa);
        }
        Ok(())
    }

    fn lookup(&self, name: &str) -> Result<InodeRef> {
        if name == ".." {
            return Ok(self.parent_node());
        }
        Ok(self.child(name)?)
    }

    fn readdir(&self) -> Result<Vec<DirEntry>> {
        match &self.inner.lock().data {
            Data::Dir { entries, .. } => Ok(entries
                .iter()
                .map(|(name, n)| DirEntry {
                    name: name.clone(),
                    ino: n.ino,
                    kind: n.kind,
                })
                .collect()),
            _ => Err(Errno::ENOTDIR),
        }
    }

    fn create(&self, name: &str, perm: u32) -> Result<InodeRef> {
        self.add(name, InodeType::File, perm, |_| Data::File {
            size: 0,
            pages: Vec::new(),
        })
    }

    fn mkdir(&self, name: &str, perm: u32) -> Result<InodeRef> {
        let parent = self.me.clone();
        self.add(name, InodeType::Dir, perm, |_| Data::Dir {
            entries: BTreeMap::new(),
            parent,
        })
    }

    fn symlink(&self, name: &str, target: &str) -> Result<InodeRef> {
        self.add(name, InodeType::Symlink, 0o777, |_| {
            Data::Symlink(String::from(target))
        })
    }

    fn mknod(&self, name: &str, kind: InodeType, perm: u32) -> Result<InodeRef> {
        if !matches!(kind, InodeType::Socket | InodeType::Fifo) {
            return Err(Errno::EINVAL);
        }
        self.add(name, kind, perm, |_| Data::Special)
    }

    fn link(&self, name: &str, target: &InodeRef) -> Result<()> {
        check_name(name)?;
        let t = downcast::<TmpNode>(target).ok_or(Errno::EXDEV)?;
        if !Arc::ptr_eq(&t.shared, &self.shared) {
            return Err(Errno::EXDEV);
        }
        if t.kind == InodeType::Dir {
            return Err(Errno::EPERM);
        }
        let _ns = self.shared.ns.lock();
        if self.child(name).is_ok() {
            return Err(Errno::EEXIST);
        }
        if let Data::Dir { entries, .. } = &mut self.inner.lock().data {
            entries.insert(String::from(name), t.arc());
        } else {
            return Err(Errno::ENOTDIR);
        }
        self.touch_dir();
        let mut ti = t.inner.lock();
        ti.nlink += 1;
        ti.ctime = clock::now();
        Ok(())
    }

    fn unlink(&self, name: &str) -> Result<()> {
        let _ns = self.shared.ns.lock();
        let child = self.child(name)?;
        if child.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        if let Data::Dir { entries, .. } = &mut self.inner.lock().data {
            entries.remove(name);
        }
        self.touch_dir();
        let mut ci = child.inner.lock();
        ci.nlink = ci.nlink.saturating_sub(1);
        ci.ctime = clock::now();
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<()> {
        let _ns = self.shared.ns.lock();
        let child = self.child(name)?;
        if child.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        {
            let mut ci = child.inner.lock();
            if let Data::Dir { entries, .. } = &ci.data
                && !entries.is_empty()
            {
                return Err(Errno::ENOTEMPTY);
            }
            ci.nlink = 0;
        }
        let mut me = self.inner.lock();
        if let Data::Dir { entries, .. } = &mut me.data {
            entries.remove(name);
        }
        me.nlink -= 1;
        let now = clock::now();
        me.mtime = now;
        me.ctime = now;
        Ok(())
    }

    fn rename(&self, old: &str, new_dir: &InodeRef, new: &str) -> Result<()> {
        check_name(new)?;
        let nd = downcast::<TmpNode>(new_dir).ok_or(Errno::EXDEV)?;
        if !Arc::ptr_eq(&nd.shared, &self.shared) {
            return Err(Errno::EXDEV);
        }
        if nd.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        let _ns = self.shared.ns.lock();
        let src = self.child(old)?;
        let dst = nd.child(new).ok();
        if let Some(dst) = &dst {
            if Arc::ptr_eq(&src, dst) {
                return Ok(()); // same inode (or same name): nothing to do
            }
            match (src.kind == InodeType::Dir, dst.kind == InodeType::Dir) {
                (true, false) => return Err(Errno::ENOTDIR),
                (false, true) => return Err(Errno::EISDIR),
                (true, true) => {
                    if let Data::Dir { entries, .. } = &dst.inner.lock().data
                        && !entries.is_empty()
                    {
                        return Err(Errno::ENOTEMPTY);
                    }
                }
                (false, false) => {}
            }
        }
        let moving_dir = src.kind == InodeType::Dir;
        if moving_dir {
            // A directory can't move into its own subtree.
            let mut cur = nd.arc();
            loop {
                if Arc::ptr_eq(&cur, &src) {
                    return Err(Errno::EINVAL);
                }
                let up = cur.parent_node();
                if Arc::ptr_eq(&up, &cur) {
                    break;
                }
                cur = up;
            }
        }
        let same_dir = ptr::eq(self, nd);
        // Detach from the old directory.
        {
            let mut me = self.inner.lock();
            if let Data::Dir { entries, .. } = &mut me.data {
                entries.remove(old);
            }
            if moving_dir && !same_dir {
                me.nlink -= 1;
            }
        }
        // Attach to the new one, replacing `dst`.
        {
            let mut ni = nd.inner.lock();
            if let Data::Dir { entries, .. } = &mut ni.data {
                entries.insert(String::from(new), src.clone());
            }
            if moving_dir && !same_dir {
                ni.nlink += 1;
            }
            if dst.as_ref().is_some_and(|d| d.kind == InodeType::Dir) {
                ni.nlink -= 1; // the replaced empty directory's ".." is gone
            }
        }
        if let Some(dst) = dst {
            let mut di = dst.inner.lock();
            di.nlink = if dst.kind == InodeType::Dir {
                0
            } else {
                di.nlink.saturating_sub(1)
            };
            di.ctime = clock::now();
        }
        {
            let mut si = src.inner.lock();
            si.ctime = clock::now();
            if let Data::Dir { parent, .. } = &mut si.data {
                *parent = nd.me.clone();
            }
        }
        self.touch_dir();
        if !same_dir {
            nd.touch_dir();
        }
        Ok(())
    }

    fn readlink(&self) -> Result<String> {
        match &self.inner.lock().data {
            Data::Symlink(t) => Ok(t.clone()),
            _ => Err(Errno::EINVAL),
        }
    }

    fn setattr(&self, a: &SetAttr) -> Result<()> {
        let mut i = self.inner.lock();
        if let Some(p) = a.perm {
            i.perm = p & 0o7777;
        }
        if let Some(u) = a.uid {
            i.uid = u;
        }
        if let Some(g) = a.gid {
            i.gid = g;
        }
        if let Some(t) = a.atime {
            i.atime = t;
        }
        if let Some(t) = a.mtime {
            i.mtime = t;
        }
        i.ctime = clock::now();
        Ok(())
    }
}
