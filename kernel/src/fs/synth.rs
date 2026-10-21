//! Building blocks for synthetic (kernel-generated) file systems: devfs, procfs, sysfs.
//!
//! A [`SynthFs`] is a tree of [`SynthDir`]s whose entries are any inodes: generated text
//! files ([`GenFile`], content produced on every read), symlinks, or custom nodes such as
//! devices. Entries are added by kernel code, never by userspace (create/mkdir/unlink give
//! EROFS).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::clock;
use super::errno::{Errno, Result};
use super::node::{DirEntry, FileSystem, Inode, InodeRef, InodeType, Stat, Timespec, alloc_dev};
use crate::sync::SpinLock;

/// Inode numbering shared by every node of one synthetic fs.
pub struct Ids {
    pub dev: u64,
    next: AtomicU64,
}

impl Ids {
    pub fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

pub struct SynthFs {
    name: &'static str,
    ids: Arc<Ids>,
    root: Arc<SynthDir>,
}

impl SynthFs {
    pub fn new(name: &'static str) -> Arc<SynthFs> {
        let ids = Arc::new(Ids {
            dev: alloc_dev(),
            next: AtomicU64::new(1),
        });
        let root = SynthDir::new(&ids, Weak::new(), 0o555);
        Arc::new(SynthFs { name, ids, root })
    }

    pub fn root_dir(&self) -> &Arc<SynthDir> {
        &self.root
    }

    pub fn ids(&self) -> &Arc<Ids> {
        &self.ids
    }
}

impl FileSystem for SynthFs {
    fn name(&self) -> &'static str {
        self.name
    }
    fn dev(&self) -> u64 {
        self.ids.dev
    }
    fn root(&self) -> InodeRef {
        self.root.clone()
    }
}

pub struct SynthDir {
    ids: Arc<Ids>,
    ino: u64,
    perm: u32,
    created: Timespec,
    me: Weak<SynthDir>,
    parent: Weak<SynthDir>,
    entries: SpinLock<Vec<(String, InodeRef)>>,
}

impl SynthDir {
    fn new(ids: &Arc<Ids>, parent: Weak<SynthDir>, perm: u32) -> Arc<SynthDir> {
        Arc::new_cyclic(|me| SynthDir {
            ids: ids.clone(),
            ino: ids.next(),
            perm,
            created: clock::now(),
            me: me.clone(),
            parent,
            entries: SpinLock::new(Vec::new()),
        })
    }

    /// Add (or replace) entry `name`.
    pub fn add(&self, name: &str, node: InodeRef) {
        let mut e = self.entries.lock();
        e.retain(|(n, _)| n != name);
        e.push((String::from(name), node));
    }

    pub fn remove(&self, name: &str) {
        self.entries.lock().retain(|(n, _)| n != name);
    }

    /// Get or create subdirectory `name`.
    pub fn add_dir(&self, name: &str) -> Arc<SynthDir> {
        if let Some((_, n)) = self.entries.lock().iter().find(|(n, _)| n == name)
            && let Some(d) = super::node::downcast::<SynthDir>(n)
        {
            return d.me.upgrade().unwrap();
        }
        let d = SynthDir::new(&self.ids, self.me.clone(), 0o555);
        self.add(name, d.clone());
        d
    }

    /// A read-only text file whose content `generate` produces on every read.
    pub fn add_file(&self, name: &str, generate: impl Fn() -> String + Send + Sync + 'static) {
        let f = Arc::new(GenFile {
            dev: self.ids.dev,
            ino: self.ids.next(),
            created: clock::now(),
            generate: Box::new(generate),
        });
        self.add(name, f);
    }

    /// A file with fixed content.
    pub fn add_text(&self, name: &str, text: String) {
        self.add_file(name, move || text.clone());
    }

    /// A control file: reads give `read()`, each `write(2)` passes its bytes to `write`
    /// (whose error is the write's error). Opening with O_TRUNC is fine.
    pub fn add_control(
        &self,
        name: &str,
        read: impl Fn() -> String + Send + Sync + 'static,
        write: impl Fn(&[u8]) -> Result<()> + Send + Sync + 'static,
    ) {
        let f = Arc::new(CtlFile {
            dev: self.ids.dev,
            ino: self.ids.next(),
            read: Box::new(read),
            write: Box::new(write),
        });
        self.add(name, f);
    }

    pub fn add_symlink(&self, name: &str, target: &str) {
        let l = Arc::new(SynthLink {
            dev: self.ids.dev,
            ino: self.ids.next(),
            target: String::from(target),
        });
        self.add(name, l);
    }

    pub fn ids(&self) -> &Arc<Ids> {
        &self.ids
    }
}

impl Inode for SynthDir {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.ids.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::Dir
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(self.ids.dev, self.ino, InodeType::Dir, self.perm);
        let e = self.entries.lock();
        st.nlink = 2 + e.iter().filter(|(_, n)| n.kind() == InodeType::Dir).count() as u32;
        st.atime = self.created;
        st.mtime = self.created;
        st.ctime = self.created;
        st
    }
    fn lookup(&self, name: &str) -> Result<InodeRef> {
        if name == ".." {
            return Ok(match self.parent.upgrade() {
                Some(p) => p,
                None => self.me.upgrade().unwrap(),
            });
        }
        self.entries
            .lock()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, i)| i.clone())
            .ok_or(Errno::ENOENT)
    }
    fn readdir(&self) -> Result<Vec<DirEntry>> {
        Ok(self
            .entries
            .lock()
            .iter()
            .map(|(name, n)| DirEntry {
                name: name.clone(),
                ino: n.ino(),
                kind: n.kind(),
            })
            .collect())
    }
}

/// A read-only file generated on each read. `st_size` is 0, like Linux procfs: read
/// until a 0-byte read.
pub struct GenFile {
    dev: u64,
    ino: u64,
    created: Timespec,
    generate: Box<dyn Fn() -> String + Send + Sync>,
}

impl Inode for GenFile {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::File
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(self.dev, self.ino, InodeType::File, 0o444);
        st.atime = self.created;
        st.mtime = self.created;
        st.ctime = self.created;
        st
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        let text = (self.generate)();
        let bytes = text.as_bytes();
        let Some(rest) = bytes.get(off as usize..) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
    fn write_at(&self, _off: u64, _buf: &[u8]) -> Result<usize> {
        Err(Errno::EACCES)
    }
    fn truncate(&self, _len: u64) -> Result<()> {
        Err(Errno::EACCES)
    }
}

pub struct SynthLink {
    dev: u64,
    ino: u64,
    target: String,
}

impl Inode for SynthLink {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::Symlink
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(self.dev, self.ino, InodeType::Symlink, 0o777);
        st.size = self.target.len() as u64;
        st
    }
    fn readlink(&self) -> Result<String> {
        Ok(self.target.clone())
    }
}

type ReadFn = Box<dyn Fn() -> String + Send + Sync>;
type WriteFn = Box<dyn Fn(&[u8]) -> Result<()> + Send + Sync>;

/// See [`SynthDir::add_control`].
pub struct CtlFile {
    dev: u64,
    ino: u64,
    read: ReadFn,
    write: WriteFn,
}

impl Inode for CtlFile {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::File
    }
    fn stat(&self) -> Stat {
        Stat::new(self.dev, self.ino, InodeType::File, 0o644)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        let text = (self.read)();
        let Some(rest) = text.as_bytes().get(off as usize..) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> Result<usize> {
        // Who may write was checked at open (fs/perm.rs); what it does is kernel work.
        super::perm::as_kernel(|| (self.write)(buf))?;
        Ok(buf.len())
    }
    fn truncate(&self, _len: u64) -> Result<()> {
        Ok(())
    }
}
