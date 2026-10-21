//! Minimal local stand-in for the vfs: the [`File`] trait the syscalls are written against,
//! and a tiny read-only tree so `openat`/`getdents64`/`execve` have something to work on:
//!
//! ```text
//! /bin/<program>   the user programs embedded in the kernel image (see initrd.rs)
//! /dev/console     the UART tty (console.rs)
//! /dev/null
//! /etc/motd
//! ```
//!
//! MERGE NOTE: the vfs agent owns the real thing (`vfs::File`, `vfs::open(path, flags)`).
//! At merge, replace this module with re-exports of theirs, keep `Console` as a device
//! node, and mount the embedded programs at /bin (or move them into the initramfs). Only
//! `syscall.rs` and `fd.rs` use this interface.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::console::Console;
use super::errno::*;
use crate::sync::SpinLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileType {
    Regular,
    Directory,
    CharDevice,
}

impl FileType {
    /// `d_type` value for getdents64.
    pub fn dt(self) -> u8 {
        match self {
            FileType::Regular => 8,
            FileType::Directory => 4,
            FileType::CharDevice => 2,
        }
    }
    /// The S_IFMT bits of st_mode.
    pub fn mode_bits(self) -> u32 {
        match self {
            FileType::Regular => 0o100000,
            FileType::Directory => 0o040000,
            FileType::CharDevice => 0o020000,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub ino: u64,
    pub kind: FileType,
    /// Permission bits (0o755 etc).
    pub perm: u32,
    pub size: u64,
    pub rdev: u64,
}

pub enum SeekFrom {
    Start(u64),
    Current(i64),
    End(i64),
}

pub struct DirEntry {
    pub ino: u64,
    pub name: String,
    pub kind: FileType,
}

/// An open file. Implementations keep their own position (one object per `open`), so
/// fds that share an object (dup, fork) share the offset, as in Unix. Methods may block.
pub trait File: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> KResult<usize>;
    fn write(&self, buf: &[u8]) -> KResult<usize>;
    fn seek(&self, _pos: SeekFrom) -> KResult<u64> {
        Err(ESPIPE)
    }
    fn stat(&self) -> KResult<Stat>;
    /// Next directory entry, or None at the end. Not a directory: ENOTDIR.
    fn readdir(&self) -> KResult<Option<DirEntry>> {
        Err(ENOTDIR)
    }
    fn is_tty(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Default)]
pub struct OpenFlags {
    pub write: bool,
    pub directory: bool,
}

// ---- the tree ----------------------------------------------------------------------

enum Node {
    Dir(&'static [(&'static str, NodeRef)]),
    Bytes(&'static [u8]),
    Console,
    Null,
}

#[derive(Clone, Copy)]
enum NodeRef {
    Static(&'static Node),
    Bin,
}

static MOTD: &[u8] = b"Welcome to milkOS. Type 'help' in msh for built-ins.\n";

static DEV: Node = Node::Dir(&[
    ("console", NodeRef::Static(&Node::Console)),
    ("null", NodeRef::Static(&Node::Null)),
]);
static ETC: Node = Node::Dir(&[("motd", NodeRef::Static(&Node::Bytes(MOTD)))]);
static ROOT: Node = Node::Dir(&[
    ("bin", NodeRef::Bin),
    ("dev", NodeRef::Static(&DEV)),
    ("etc", NodeRef::Static(&ETC)),
]);

/// Inode numbers: stable per node address (good enough for a static tree).
fn ino_of(r: NodeRef, name: &str) -> u64 {
    match r {
        NodeRef::Static(n) => n as *const Node as u64 & 0xffff_ffff,
        NodeRef::Bin => 2,
    }
    .wrapping_add(name.len() as u64)
}

fn kind_of(r: NodeRef) -> FileType {
    match r {
        NodeRef::Bin => FileType::Directory,
        NodeRef::Static(Node::Dir(_)) => FileType::Directory,
        NodeRef::Static(Node::Console) | NodeRef::Static(Node::Null) => FileType::CharDevice,
        NodeRef::Static(Node::Bytes(_)) => FileType::Regular,
    }
}

enum Found {
    Node(NodeRef),
    Program(&'static [u8]),
}

fn children(r: NodeRef) -> Vec<(String, Found)> {
    match r {
        NodeRef::Static(Node::Dir(ents)) => ents
            .iter()
            .map(|(n, r)| (String::from(*n), Found::Node(*r)))
            .collect(),
        NodeRef::Bin => super::initrd::PROGRAMS
            .iter()
            .map(|(n, d)| (String::from(*n), Found::Program(d)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Resolve `.` and `..` in an absolute path; returns its components.
pub fn normalize(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts
}

/// Open an absolute path.
pub fn open(path: &str, flags: OpenFlags) -> KResult<Arc<dyn File>> {
    if !path.starts_with('/') {
        return Err(EINVAL);
    }
    let mut cur = Found::Node(NodeRef::Static(&ROOT));
    let mut ino = 1;
    for comp in normalize(path) {
        let Found::Node(dir) = cur else {
            return Err(ENOTDIR);
        };
        if kind_of(dir) != FileType::Directory {
            return Err(ENOTDIR);
        }
        let (name, next) = children(dir)
            .into_iter()
            .find(|(n, _)| n == comp)
            .ok_or(ENOENT)?;
        ino = match &next {
            Found::Node(r) => ino_of(*r, &name),
            Found::Program(d) => 0x1000 + (d.as_ptr() as u64 & 0xffff),
        };
        cur = next;
    }
    let is_dir = matches!(cur, Found::Node(r) if kind_of(r) == FileType::Directory);
    if flags.directory && !is_dir {
        return Err(ENOTDIR);
    }
    if flags.write && is_dir {
        return Err(EISDIR);
    }
    Ok(match cur {
        Found::Program(d) => {
            if flags.write {
                return Err(EROFS);
            }
            Arc::new(RamFile::new(d, ino, 0o755))
        }
        Found::Node(r @ NodeRef::Bin) | Found::Node(r @ NodeRef::Static(Node::Dir(_))) => {
            Arc::new(RamDir::new(r, ino))
        }
        Found::Node(NodeRef::Static(Node::Bytes(d))) => {
            if flags.write {
                return Err(EROFS);
            }
            Arc::new(RamFile::new(d, ino, 0o644))
        }
        Found::Node(NodeRef::Static(Node::Console)) => Arc::new(Console),
        Found::Node(NodeRef::Static(Node::Null)) => Arc::new(Null),
    })
}

/// Read a whole file (for execve).
pub fn read_all(path: &str) -> KResult<Vec<u8>> {
    let f = open(path, OpenFlags::default())?;
    let st = f.stat()?;
    if st.kind == FileType::Directory {
        return Err(EACCES);
    }
    let mut v = alloc::vec![0u8; st.size as usize];
    let mut done = 0;
    while done < v.len() {
        let n = f.read(&mut v[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    v.truncate(done);
    Ok(v)
}

// ---- file objects ----------------------------------------------------------------

struct RamFile {
    data: &'static [u8],
    pos: SpinLock<u64>,
    ino: u64,
    perm: u32,
}

impl RamFile {
    fn new(data: &'static [u8], ino: u64, perm: u32) -> Self {
        Self {
            data,
            pos: SpinLock::new(0),
            ino,
            perm,
        }
    }
}

impl File for RamFile {
    fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        let mut pos = self.pos.lock();
        let start = (*pos as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        *pos += n as u64;
        Ok(n)
    }
    fn write(&self, _buf: &[u8]) -> KResult<usize> {
        Err(EBADF)
    }
    fn seek(&self, p: SeekFrom) -> KResult<u64> {
        let mut pos = self.pos.lock();
        let new = match p {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::Current(o) => *pos as i64 + o,
            SeekFrom::End(o) => self.data.len() as i64 + o,
        };
        if new < 0 {
            return Err(EINVAL);
        }
        *pos = new as u64;
        Ok(*pos)
    }
    fn stat(&self) -> KResult<Stat> {
        Ok(Stat {
            ino: self.ino,
            kind: FileType::Regular,
            perm: self.perm,
            size: self.data.len() as u64,
            rdev: 0,
        })
    }
}

struct RamDir {
    node: NodeRef,
    ino: u64,
    pos: SpinLock<usize>,
}

impl RamDir {
    fn new(node: NodeRef, ino: u64) -> Self {
        Self {
            node,
            ino,
            pos: SpinLock::new(0),
        }
    }
}

impl File for RamDir {
    fn read(&self, _buf: &mut [u8]) -> KResult<usize> {
        Err(EISDIR)
    }
    fn write(&self, _buf: &[u8]) -> KResult<usize> {
        Err(EISDIR)
    }
    fn seek(&self, p: SeekFrom) -> KResult<u64> {
        match p {
            SeekFrom::Start(0) => {
                *self.pos.lock() = 0;
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }
    fn stat(&self) -> KResult<Stat> {
        Ok(Stat {
            ino: self.ino,
            kind: FileType::Directory,
            perm: 0o755,
            size: 0,
            rdev: 0,
        })
    }
    fn readdir(&self) -> KResult<Option<DirEntry>> {
        let i = {
            let mut pos = self.pos.lock();
            *pos += 1;
            *pos - 1
        };
        // "." and ".." first, like a real directory.
        if i < 2 {
            return Ok(Some(DirEntry {
                ino: self.ino,
                name: String::from(if i == 0 { "." } else { ".." }),
                kind: FileType::Directory,
            }));
        }
        Ok(children(self.node)
            .into_iter()
            .nth(i - 2)
            .map(|(name, f)| DirEntry {
                ino: match &f {
                    Found::Node(r) => ino_of(*r, &name),
                    Found::Program(d) => 0x1000 + (d.as_ptr() as u64 & 0xffff),
                },
                kind: match &f {
                    Found::Node(r) => kind_of(*r),
                    Found::Program(_) => FileType::Regular,
                },
                name,
            }))
    }
}

struct Null;

impl File for Null {
    fn read(&self, _buf: &mut [u8]) -> KResult<usize> {
        Ok(0)
    }
    fn write(&self, buf: &[u8]) -> KResult<usize> {
        Ok(buf.len())
    }
    fn stat(&self) -> KResult<Stat> {
        Ok(Stat {
            ino: 3,
            kind: FileType::CharDevice,
            perm: 0o666,
            size: 0,
            rdev: (1 << 8) | 3,
        })
    }
}
