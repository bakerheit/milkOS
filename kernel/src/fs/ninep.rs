//! 9P2000.L client: the Mac shared folder at `/Volumes/Mac` (virtio-9p, mount tag `mac`).
//!
//! Each inode holds a *path fid* (walked from the root, never opened) and, lazily, an
//! open fid for reading and one for writing. `..` is a 9P walk. Inode numbers are the
//! server's qid paths, so the VFS's `(dev, ino)` identity works. Fids an inode no longer
//! needs are clunked on the next request (never from `Drop`, which may run anywhere).
//!
//! Read-only unless the boot script set `-fw_cfg name=opt/milkos/share-rw,string=1`
//! (MILK_SHARE_RW=1), in which case qemu's export is writable too. Errors are the Linux
//! errnos qemu sends in Rlerror.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use super::errno::{Errno, Result};
use super::node::{
    DirEntry, FileSystem, Inode, InodeRef, InodeType, SetAttr, Stat, Timespec, alloc_dev,
    check_name, downcast,
};
use crate::sched::Mutex;
use crate::sync::SpinLock;
use crate::virtio::p9::{MSIZE, P9Device};
use crate::{fwcfg, kprintln};

// Message types (T; the R reply is T + 1). Rlerror = 7.
const TLERROR: u8 = 6;
const TSTATFS: u8 = 8;
const TLOPEN: u8 = 12;
const TLCREATE: u8 = 14;
const TSYMLINK: u8 = 16;
const TREADLINK: u8 = 22;
const TGETATTR: u8 = 24;
const TSETATTR: u8 = 26;
const TREADDIR: u8 = 40;
const TFSYNC: u8 = 50;
const TMKDIR: u8 = 72;
const TRENAMEAT: u8 = 74;
const TUNLINKAT: u8 = 76;
const TVERSION: u8 = 100;
const TATTACH: u8 = 104;
const TWALK: u8 = 110;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;

const NOFID: u32 = !0;
const QTDIR: u8 = 0x80;
const QTSYMLINK: u8 = 0x02;
// 9P2000.L open flags (Linux generic values).
const L_RDONLY: u32 = 0;
const L_RDWR: u32 = 2;
const L_CREAT: u32 = 0o100;
const L_EXCL: u32 = 0o200;
const L_DIRECTORY: u32 = 0o200000;
const GETATTR_BASIC: u64 = 0x7ff;
const SETATTR_MODE: u32 = 0x1;
const SETATTR_UID: u32 = 0x2;
const SETATTR_GID: u32 = 0x4;
const SETATTR_SIZE: u32 = 0x8;
const SETATTR_ATIME: u32 = 0x10;
const SETATTR_MTIME: u32 = 0x20;
const SETATTR_ATIME_SET: u32 = 0x80;
const SETATTR_MTIME_SET: u32 = 0x100;
const AT_REMOVEDIR: u32 = 0x200;
/// Bytes of a Twrite/Rread beyond the payload.
const IO_OVERHEAD: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
pub struct Qid {
    pub ty: u8,
    pub version: u32,
    pub path: u64,
}

impl Qid {
    fn kind(&self) -> InodeType {
        if self.ty & QTDIR != 0 {
            InodeType::Dir
        } else if self.ty & QTSYMLINK != 0 {
            InodeType::Symlink
        } else {
            InodeType::File
        }
    }
}

// ---- wire format --------------------------------------------------------------------

struct W(Vec<u8>);

impl W {
    fn new(ty: u8) -> W {
        let mut v = alloc::vec![0u8; 4];
        v.push(ty);
        v.extend_from_slice(&1u16.to_le_bytes()); // tag: one request at a time
        W(v)
    }
    fn u32(mut self, x: u32) -> W {
        self.0.extend_from_slice(&x.to_le_bytes());
        self
    }
    fn u64(mut self, x: u64) -> W {
        self.0.extend_from_slice(&x.to_le_bytes());
        self
    }
    fn u16(mut self, x: u16) -> W {
        self.0.extend_from_slice(&x.to_le_bytes());
        self
    }
    fn str(mut self, s: &str) -> W {
        self.0.extend_from_slice(&(s.len() as u16).to_le_bytes());
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn bytes(mut self, b: &[u8]) -> W {
        self.0.extend_from_slice(b);
        self
    }
    fn done(mut self) -> Vec<u8> {
        let n = self.0.len() as u32;
        self.0[..4].copy_from_slice(&n.to_le_bytes());
        self.0
    }
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n).ok_or(Errno::EIO)?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn str(&mut self) -> Result<String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn qid(&mut self) -> Result<Qid> {
        Ok(Qid {
            ty: self.u8()?,
            version: self.u32()?,
            path: self.u64()?,
        })
    }
}

// ---- client -------------------------------------------------------------------------

pub struct Client {
    dev: Arc<P9Device>,
    pub rw: bool,
    msize: usize,
    next_fid: AtomicU32,
    free_fids: SpinLock<Vec<u32>>,
    /// Fids to clunk on the next request.
    clunk_q: SpinLock<Vec<u32>>,
}

impl Client {
    /// One request; returns the reply body (after size/type/tag). Rlerror -> its errno.
    fn call(&self, msg: Vec<u8>) -> Result<Vec<u8>> {
        let pending = core::mem::take(&mut *self.clunk_q.lock());
        for f in pending {
            if self.dev.rpc(&W::new(TCLUNK).u32(f).done()).is_ok() {
                self.free_fids.lock().push(f);
            }
        }
        let ty = msg[4];
        let reply = self.dev.rpc(&msg)?;
        let rty = reply[4];
        let body = reply[7..].to_vec();
        if rty == TLERROR + 1 {
            let e = R { b: &body, p: 0 }.u32()?;
            return Err(Errno(e as i32));
        }
        if rty != ty + 1 {
            return Err(Errno::EIO);
        }
        Ok(body)
    }

    fn fid(&self) -> u32 {
        self.free_fids
            .lock()
            .pop()
            .unwrap_or_else(|| self.next_fid.fetch_add(1, Ordering::Relaxed))
    }

    fn clunk_later(&self, fid: u32) {
        self.clunk_q.lock().push(fid);
    }

    fn ro(&self) -> Result<()> {
        if self.rw { Ok(()) } else { Err(Errno::EROFS) }
    }

    /// Walk `names` from `fid` into a new fid. Returns it and the last qid.
    fn walk(&self, fid: u32, names: &[&str]) -> Result<(u32, Option<Qid>)> {
        let nf = self.fid();
        let mut w = W::new(TWALK).u32(fid).u32(nf).u16(names.len() as u16);
        for n in names {
            w = w.str(n);
        }
        match self.call(w.done()) {
            Ok(b) => {
                let mut r = R { b: &b, p: 0 };
                let n = r.u16()? as usize;
                if n != names.len() {
                    // A partial walk doesn't create newfid.
                    self.free_fids.lock().push(nf);
                    return Err(Errno::ENOENT);
                }
                let mut last = None;
                for _ in 0..n {
                    last = Some(r.qid()?);
                }
                Ok((nf, last))
            }
            Err(e) => {
                self.free_fids.lock().push(nf);
                Err(e)
            }
        }
    }

    /// A fresh fid on the same file, opened with `flags`.
    fn open(&self, fid: u32, flags: u32) -> Result<u32> {
        let (nf, _) = self.walk(fid, &[])?;
        match self.call(W::new(TLOPEN).u32(nf).u32(flags).done()) {
            Ok(_) => Ok(nf),
            Err(e) => {
                self.clunk_later(nf);
                Err(e)
            }
        }
    }

    fn getattr(&self, fid: u32) -> Result<Stat9> {
        let b = self.call(W::new(TGETATTR).u32(fid).u64(GETATTR_BASIC).done())?;
        let mut r = R { b: &b, p: 0 };
        let _valid = r.u64()?;
        let qid = r.qid()?;
        let mode = r.u32()?;
        let uid = r.u32()?;
        let gid = r.u32()?;
        let nlink = r.u64()?;
        let rdev = r.u64()?;
        let size = r.u64()?;
        let blksize = r.u64()?;
        let blocks = r.u64()?;
        let mut t = || -> Result<Timespec> {
            Ok(Timespec {
                sec: r.u64()? as i64,
                nsec: r.u64()? as i64,
            })
        };
        let (atime, mtime, ctime) = (t()?, t()?, t()?);
        Ok(Stat9 {
            qid,
            mode,
            uid,
            gid,
            nlink,
            rdev,
            size,
            blksize,
            blocks,
            atime,
            mtime,
            ctime,
        })
    }
}

struct Stat9 {
    qid: Qid,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u64,
    rdev: u64,
    size: u64,
    blksize: u64,
    blocks: u64,
    atime: Timespec,
    mtime: Timespec,
    ctime: Timespec,
}

// ---- VFS objects --------------------------------------------------------------------

pub struct NineFs {
    shared: Arc<Shared>,
    /// Kept alive for the mount's lifetime (it owns fid 0).
    root: Arc<NineInode>,
}

struct Shared {
    c: Client,
    dev: u64,
    inodes: SpinLock<BTreeMap<u64, Weak<NineInode>>>,
}

impl Shared {
    /// The one object for `qid`, taking ownership of `fid` (clunked if an object exists).
    fn obj(self: &Arc<Self>, fid: u32, qid: Qid) -> Arc<NineInode> {
        let mut m = self.inodes.lock();
        if let Some(i) = m.get(&qid.path).and_then(|w| w.upgrade()) {
            drop(m);
            self.c.clunk_later(fid);
            return i;
        }
        if m.len() > 1024 {
            m.retain(|_, w| w.strong_count() > 0);
        }
        let i = Arc::new(NineInode {
            fs: self.clone(),
            fid,
            ino: qid.path,
            kind: qid.kind(),
            rfid: Mutex::new(None),
            wfid: Mutex::new(None),
        });
        m.insert(qid.path, Arc::downgrade(&i));
        i
    }
}

impl FileSystem for NineFs {
    fn name(&self) -> &'static str {
        "9p"
    }
    fn dev(&self) -> u64 {
        self.shared.dev
    }
    fn root(&self) -> InodeRef {
        self.root.clone()
    }
}

pub struct NineInode {
    fs: Arc<Shared>,
    /// Path fid (walked, not opened).
    fid: u32,
    ino: u64,
    kind: InodeType,
    rfid: Mutex<Option<u32>>,
    wfid: Mutex<Option<u32>>,
}

impl Drop for NineInode {
    fn drop(&mut self) {
        let c = &self.fs.c;
        for f in [
            Some(self.fid).filter(|&f| f != 0),
            *self.rfid.lock(),
            *self.wfid.lock(),
        ]
        .into_iter()
        .flatten()
        {
            c.clunk_later(f);
        }
    }
}

fn kind_from_dt(dt: u8) -> InodeType {
    match dt {
        4 => InodeType::Dir,
        10 => InodeType::Symlink,
        2 => InodeType::CharDev,
        6 => InodeType::BlockDev,
        1 => InodeType::Fifo,
        12 => InodeType::Socket,
        _ => InodeType::File,
    }
}

impl NineInode {
    fn c(&self) -> &Client {
        &self.fs.c
    }

    fn read_fid(&self) -> Result<u32> {
        let mut g = self.rfid.lock();
        if let Some(f) = *g {
            return Ok(f);
        }
        let f = self.c().open(self.fid, L_RDONLY)?;
        *g = Some(f);
        Ok(f)
    }

    fn write_fid(&self) -> Result<u32> {
        self.c().ro()?;
        let mut g = self.wfid.lock();
        if let Some(f) = *g {
            return Ok(f);
        }
        let f = self.c().open(self.fid, L_RDWR)?;
        *g = Some(f);
        Ok(f)
    }

    fn child(&self, name: &str) -> Result<Arc<NineInode>> {
        let (f, q) = self.c().walk(self.fid, &[name])?;
        Ok(self.fs.obj(f, q.ok_or(Errno::EIO)?))
    }
}

impl Inode for NineInode {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.fs.dev
    }
    fn kind(&self) -> InodeType {
        self.kind
    }

    fn stat(&self) -> Stat {
        match self.c().getattr(self.fid) {
            Ok(a) => {
                let mut s = Stat::new(self.fs.dev, a.qid.path, self.kind, a.mode & 0o7777);
                s.nlink = a.nlink as u32;
                s.uid = a.uid;
                s.gid = a.gid;
                s.rdev = a.rdev;
                s.size = a.size;
                s.blksize = a.blksize as u32;
                s.blocks = a.blocks;
                s.atime = a.atime;
                s.mtime = a.mtime;
                s.ctime = a.ctime;
                s
            }
            Err(_) => Stat::new(self.fs.dev, self.ino, self.kind, 0),
        }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        if self.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        let f = self.read_fid()?;
        let mut done = 0;
        while done < buf.len() {
            let want = (buf.len() - done).min(self.c().msize - IO_OVERHEAD);
            let b = self.c().call(
                W::new(TREAD)
                    .u32(f)
                    .u64(off + done as u64)
                    .u32(want as u32)
                    .done(),
            )?;
            let mut r = R { b: &b, p: 0 };
            let n = r.u32()? as usize;
            let data = r.take(n)?;
            buf[done..done + n].copy_from_slice(data);
            done += n;
            if n < want {
                break; // EOF
            }
        }
        Ok(done)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize> {
        if self.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        let f = self.write_fid()?;
        let mut done = 0;
        while done < buf.len() {
            let n = (buf.len() - done).min(self.c().msize - IO_OVERHEAD);
            let b = self.c().call(
                W::new(TWRITE)
                    .u32(f)
                    .u64(off + done as u64)
                    .u32(n as u32)
                    .bytes(&buf[done..done + n])
                    .done(),
            )?;
            let wrote = R { b: &b, p: 0 }.u32()? as usize;
            done += wrote;
            if wrote < n {
                break;
            }
        }
        Ok(done)
    }

    fn truncate(&self, len: u64) -> Result<()> {
        self.c().ro()?;
        self.c()
            .call(
                W::new(TSETATTR)
                    .u32(self.fid)
                    .u32(SETATTR_SIZE)
                    .u32(0)
                    .u32(0)
                    .u32(0)
                    .u64(len)
                    .u64(0)
                    .u64(0)
                    .u64(0)
                    .u64(0)
                    .done(),
            )
            .map(|_| ())
    }

    fn lookup(&self, name: &str) -> Result<InodeRef> {
        if self.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        Ok(self.child(name)?)
    }

    fn readdir(&self) -> Result<Vec<DirEntry>> {
        if self.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        let f = self.c().open(self.fid, L_RDONLY | L_DIRECTORY)?;
        let mut out = Vec::new();
        let mut cookie = 0u64;
        let r = (|| -> Result<()> {
            loop {
                let b = self.c().call(
                    W::new(TREADDIR)
                        .u32(f)
                        .u64(cookie)
                        .u32((self.c().msize - IO_OVERHEAD) as u32)
                        .done(),
                )?;
                let mut r = R { b: &b, p: 0 };
                let count = r.u32()? as usize;
                if count == 0 {
                    return Ok(());
                }
                let end = r.p + count;
                while r.p < end {
                    let q = r.qid()?;
                    cookie = r.u64()?;
                    let dt = r.u8()?;
                    let name = r.str()?;
                    if name != "." && name != ".." {
                        out.push(DirEntry {
                            name,
                            ino: q.path,
                            kind: if dt == 0 { q.kind() } else { kind_from_dt(dt) },
                        });
                    }
                }
            }
        })();
        self.c().clunk_later(f);
        r.map(|_| out)
    }

    fn create(&self, name: &str, perm: u32) -> Result<InodeRef> {
        self.c().ro()?;
        check_name(name)?;
        // Tlcreate turns a clone of the dir fid into an open fid on the new file.
        let (nf, _) = self.c().walk(self.fid, &[])?;
        let b = self.c().call(
            W::new(TLCREATE)
                .u32(nf)
                .str(name)
                .u32(L_RDWR | L_CREAT | L_EXCL)
                .u32(perm & 0o7777)
                .u32(0)
                .done(),
        );
        if let Err(e) = b {
            self.c().clunk_later(nf);
            return Err(e);
        }
        let node = self.child(name)?;
        let mut w = node.wfid.lock();
        match *w {
            None => *w = Some(nf),
            Some(_) => self.c().clunk_later(nf),
        }
        drop(w);
        Ok(node)
    }

    fn mkdir(&self, name: &str, perm: u32) -> Result<InodeRef> {
        self.c().ro()?;
        check_name(name)?;
        self.c().call(
            W::new(TMKDIR)
                .u32(self.fid)
                .str(name)
                .u32(perm & 0o7777)
                .u32(0)
                .done(),
        )?;
        Ok(self.child(name)?)
    }

    fn symlink(&self, name: &str, target: &str) -> Result<InodeRef> {
        self.c().ro()?;
        check_name(name)?;
        self.c().call(
            W::new(TSYMLINK)
                .u32(self.fid)
                .str(name)
                .str(target)
                .u32(0)
                .done(),
        )?;
        Ok(self.child(name)?)
    }

    fn unlink(&self, name: &str) -> Result<()> {
        self.c().ro()?;
        if self.child(name)?.kind == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        self.c()
            .call(W::new(TUNLINKAT).u32(self.fid).str(name).u32(0).done())
            .map(|_| ())
    }

    fn rmdir(&self, name: &str) -> Result<()> {
        self.c().ro()?;
        if self.child(name)?.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        self.c()
            .call(
                W::new(TUNLINKAT)
                    .u32(self.fid)
                    .str(name)
                    .u32(AT_REMOVEDIR)
                    .done(),
            )
            .map(|_| ())
    }

    fn rename(&self, old: &str, new_dir: &InodeRef, new: &str) -> Result<()> {
        self.c().ro()?;
        let nd = downcast::<NineInode>(new_dir).ok_or(Errno::EXDEV)?;
        if !Arc::ptr_eq(&nd.fs, &self.fs) {
            return Err(Errno::EXDEV);
        }
        check_name(new)?;
        self.c()
            .call(
                W::new(TRENAMEAT)
                    .u32(self.fid)
                    .str(old)
                    .u32(nd.fid)
                    .str(new)
                    .done(),
            )
            .map(|_| ())
    }

    fn readlink(&self) -> Result<String> {
        if self.kind != InodeType::Symlink {
            return Err(Errno::EINVAL);
        }
        let b = self.c().call(W::new(TREADLINK).u32(self.fid).done())?;
        R { b: &b, p: 0 }.str()
    }

    fn setattr(&self, a: &SetAttr) -> Result<()> {
        self.c().ro()?;
        let mut valid = 0;
        if a.perm.is_some() {
            valid |= SETATTR_MODE;
        }
        if a.uid.is_some() {
            valid |= SETATTR_UID;
        }
        if a.gid.is_some() {
            valid |= SETATTR_GID;
        }
        if a.atime.is_some() {
            valid |= SETATTR_ATIME | SETATTR_ATIME_SET;
        }
        if a.mtime.is_some() {
            valid |= SETATTR_MTIME | SETATTR_MTIME_SET;
        }
        let (at, mt) = (a.atime.unwrap_or_default(), a.mtime.unwrap_or_default());
        self.c()
            .call(
                W::new(TSETATTR)
                    .u32(self.fid)
                    .u32(valid)
                    .u32(a.perm.unwrap_or(0))
                    .u32(a.uid.unwrap_or(0))
                    .u32(a.gid.unwrap_or(0))
                    .u64(0)
                    .u64(at.sec as u64)
                    .u64(at.nsec as u64)
                    .u64(mt.sec as u64)
                    .u64(mt.nsec as u64)
                    .done(),
            )
            .map(|_| ())
    }

    fn sync(&self) -> Result<()> {
        if let Some(f) = *self.wfid.lock() {
            self.c().call(W::new(TFSYNC).u32(f).u32(0).done())?;
        }
        Ok(())
    }
}

/// The share is writable only when the boot script says so (MILK_SHARE_RW=1).
fn rw_requested() -> bool {
    let Some(f) = fwcfg::find_file("opt/milkos/share-rw") else {
        return false;
    };
    let mut b = alloc::vec![0u8; f.size as usize];
    fwcfg::read_file(f, &mut b) && b.first() == Some(&b'1')
}

/// Connect to the virtio-9p export tagged `tag` and mount it at `path`.
pub fn mount(dev: Arc<P9Device>, path: &str, rw: bool) -> Result<()> {
    let c = Client {
        dev,
        rw,
        msize: MSIZE,
        next_fid: AtomicU32::new(1),
        free_fids: SpinLock::new(Vec::new()),
        clunk_q: SpinLock::new(Vec::new()),
    };
    let b = c.call(W::new(TVERSION).u32(MSIZE as u32).str("9P2000.L").done())?;
    let mut r = R { b: &b, p: 0 };
    let msize = r.u32()? as usize;
    let version = r.str()?;
    if version != "9P2000.L" {
        kprintln!("9p: server speaks {version:?}, not 9P2000.L");
        return Err(Errno::EOPNOTSUPP);
    }
    let c = Client {
        msize: msize.min(MSIZE),
        ..c
    };
    let b = c.call(
        W::new(TATTACH)
            .u32(0)
            .u32(NOFID)
            .str("milk")
            .str("")
            .u32(0)
            .done(),
    )?;
    let root_qid = R { b: &b, p: 0 }.qid()?;
    let _ = TSTATFS;
    let shared = Arc::new(Shared {
        c,
        dev: alloc_dev(),
        inodes: SpinLock::new(BTreeMap::new()),
    });
    let root = shared.obj(0, root_qid);
    let fs = Arc::new(NineFs { shared, root });
    super::ops::mkdir_all(path, 0o755)?;
    super::path::mount(path, fs)?;
    Ok(())
}

static RW: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Was the Mac shared folder mounted read-write?
pub fn is_rw() -> bool {
    RW.load(Ordering::Relaxed)
}

/// Mount the Mac shared folder (tag `mac`) at /Volumes/Mac, if qemu provides it.
pub fn automount() {
    for d in crate::virtio::p9::devices() {
        if d.tag != "mac" {
            continue;
        }
        let rw = rw_requested();
        RW.store(rw, Ordering::Relaxed);
        match mount(d, "/Volumes/Mac", rw) {
            Ok(()) => kprintln!(
                "9p: Mac shared folder at /Volumes/Mac ({})",
                if rw { "read-write" } else { "read-only" }
            ),
            Err(e) => kprintln!("9p: mounting the Mac shared folder failed: {e}"),
        }
    }
}
