//! Scrub: read every block reachable from a volume's committed state (live tree, every
//! snapshot and clone, shared subtrees and shared data once, dead-list chains, the space
//! map) straight from the disk, bypassing the caches, and check its checksum.
//!
//! The state it checks is the one committed when it starts. Blocks freed while it runs
//! are held back from reuse (`Store::hold_frees`), so nothing it is about to read can be
//! overwritten; they count as free in the space map and are released at the end. The
//! volume lock is only taken to start, to end and to name a damaged file ([`scrub`] goes
//! through a [`StateAccess`]); reads go straight to the device.
//!
//! A checksum error is reported with what the block is (data of a file, with its path and
//! the snapshot or clone it is in; a tree node; a dead-list or space-map block). Nothing is
//! repaired: a damaged file can be restored from a snapshot that has a good copy
//! (`versions restore`), a damaged node needs `check` (it can't be repaired either).
//!
//! The kernel runs this from a low-priority thread with a rate limit, a status file and the
//! audit log (`kernel/src/fs/milkfs/scrub.rs`): those come in through a [`ScrubRunner`].

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::format::*;
use super::items::{LIVE, clone_view};
use super::volume::StateAccess;
use crate::blockdev::BlockRef;
use crate::errno::{Errno, Result};

/// Blocks per read request (128 KiB).
const CHUNK: u64 = 32;

#[derive(Clone, Debug)]
pub struct ScrubError {
    pub block: u64,
    /// e.g. "data of /Volumes/Milk/docs/b.bin at offset 0 (live)".
    pub what: String,
}

/// What runs a scrub: told how far it got after every read (progress, rate limiting), and
/// asked before every read whether to stop.
pub trait ScrubRunner {
    /// The scrub has started: `total` blocks were in use (the progress denominator).
    fn started(&mut self, _total: u64) {}
    /// Stop now (the scrub ends with EINTR)?
    fn stop(&self) -> bool {
        false
    }
    /// A read of `n` bytes is done; `done` blocks have been read so far.
    fn read(&mut self, _n: usize, _done: u64) {}
    /// Asked at the end (volume locked): does a complete scrub count as the volume's last
    /// full one (superblock `last_scrub`, written with the next commit)?
    fn records_last_scrub(&self) -> bool {
        false
    }
}

/// A runner that never stops and doesn't care about progress.
pub struct Unlimited;

impl ScrubRunner for Unlimited {}

/// What a scrub did.
#[derive(Debug)]
pub struct Scrubbed {
    /// Ok: every block was read. EINTR: stopped. Another error: the walk failed.
    pub walked: Result<()>,
    pub errors: Vec<ScrubError>,
    /// Blocks and bytes read.
    pub done: u64,
    pub bytes: u64,
}

/// Where a block was found (for naming it in a report).
#[derive(Clone)]
enum Owner {
    Node(String),
    /// View it was found in (names the file), ino, file offset. Every tree holding the
    /// block is looked up when the error is named.
    Data(u64, u64, u64),
    Meta(&'static str),
}

struct Scan<'a> {
    dev: BlockRef,
    /// Encrypted volume: blocks are opened (decrypted + tag checked) with this.
    key: Option<Arc<super::crypt::VolumeKey>>,
    per: u64,
    nodes: BTreeSet<u64>,
    data: BTreeSet<u64>,
    bad: Vec<(u64, Owner)>,
    done: u64,
    bytes: u64,
    run: &'a mut dyn ScrubRunner,
    buf: Vec<u8>,
}

impl Scan<'_> {
    /// Read `n` consecutive blocks from `block` raw into `self.buf`.
    fn read(&mut self, block: u64, n: u64) -> Result<()> {
        if self.run.stop() {
            return Err(Errno::EINTR);
        }
        let len = (n as usize) * BLOCK;
        self.buf.resize(len, 0);
        let r = self.dev.read_blocks(block * self.per, &mut self.buf[..len]);
        self.bytes += len as u64;
        self.done += n;
        self.run.read(len, self.done);
        r
    }

    /// Is the `i`-th block of the last read (disk block `block`, born `birth`) intact?
    /// (Encrypted: decrypts it in place, so a node can be decoded afterwards.)
    fn check(&mut self, i: u64, block: u64, birth: u64, csum: u64) -> bool {
        let b = &mut self.buf[(i as usize) * BLOCK..(i as usize + 1) * BLOCK];
        match &self.key {
            Some(k) => k.open(block, birth, csum, b),
            None => xxh64(b) == csum,
        }
    }

    fn tree(&mut self, label: &str, view: u64, root: BlockPtr) -> Result<()> {
        let mut stack = alloc::vec![root];
        while let Some(p) = stack.pop() {
            if p.is_none() || !self.nodes.insert(p.block) {
                continue;
            }
            let ok = match self.read(p.block, 1) {
                Ok(()) => self.check(0, p.block, p.birth, p.csum),
                Err(Errno::EINTR) => return Err(Errno::EINTR),
                Err(_) => false,
            };
            if !ok {
                self.bad.push((p.block, Owner::Node(String::from(label))));
                continue; // can't trust what it points to
            }
            let node = decode_node(self.buf[..BLOCK].try_into().unwrap())?;
            match node {
                DiskNode::Internal(_, kids) => stack.extend(kids.iter().map(|(_, c)| *c)),
                DiskNode::Leaf(items) => {
                    for (k, v) in &items {
                        if let Some(e) = Ext::decode(k, v) {
                            self.run(view, k.obj, &e)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&mut self, view: u64, ino: u64, e: &Ext) -> Result<()> {
        let n = e.len();
        let mut i = 0;
        while i < n {
            let c = CHUNK.min(n - i);
            if (i..i + c).all(|j| self.data.contains(&(e.block + j))) {
                i += c;
                continue;
            }
            let read = self.read(e.block + i, c);
            if read == Err(Errno::EINTR) {
                return read;
            }
            for j in 0..c {
                let b = e.block + i + j;
                if !self.data.insert(b) {
                    continue;
                }
                if read.is_err() || !self.check(j, b, e.birth, e.csums[(i + j) as usize]) {
                    let off = (e.start + i + j) * BLOCK as u64;
                    self.bad.push((b, Owner::Data(view, ino, off)));
                }
            }
            i += c;
        }
        Ok(())
    }

    fn meta(&mut self, p: BlockPtr, what: &'static str) -> Result<bool> {
        if p.is_none() || !self.nodes.insert(p.block) {
            return Ok(false);
        }
        let ok = match self.read(p.block, 1) {
            Ok(()) => self.check(0, p.block, p.birth, p.csum),
            Err(Errno::EINTR) => return Err(Errno::EINTR),
            Err(_) => false,
        };
        if !ok {
            self.bad.push((p.block, Owner::Meta(what)));
        }
        Ok(ok)
    }
}

/// Scrub one volume (`path`: where it is mounted, for naming files). Errors from
/// committing first are returned as such; whatever happens after that is in [`Scrubbed`].
pub fn scrub<A: StateAccess>(v: &A, path: &str, run: &mut dyn ScrubRunner) -> Result<Scrubbed> {
    let key = v.with_state(|st| st.store.crypt.clone());
    let (dev, trees, heads, map, total, keyring, uuid) = v.with_state(|st| {
        st.commit()?;
        st.store.hold_frees();
        let mut trees = alloc::vec![(String::from("live"), LIVE, st.sb.root)];
        trees.extend(
            st.snaps
                .iter()
                .map(|s| (format!("snapshot {}", s.name), s.generation, s.root)),
        );
        trees.extend(
            st.clones
                .iter()
                .map(|c| (format!("clone {}", c.name), clone_view(c.id), c.root)),
        );
        let mut heads = alloc::vec![st.deadlist];
        heads.extend(st.snaps.iter().map(|s| s.deadlist));
        let total = st.store.total() - st.store.free_blocks();
        Ok::<_, Errno>((
            st.store.cache.device().clone(),
            trees,
            heads,
            st.sb.spacemap,
            total,
            st.keyring,
            st.sb.uuid,
        ))
    })?;
    run.started(total);
    let per = (BLOCK / dev.block_size()) as u64;
    let mut sc = Scan {
        dev,
        key,
        per,
        nodes: BTreeSet::new(),
        data: BTreeSet::new(),
        bad: Vec::new(),
        done: 0,
        bytes: 0,
        run,
        buf: Vec::new(),
    };
    let walked = (|| -> Result<()> {
        for (label, view, root) in &trees {
            sc.tree(label, *view, *root)?;
        }
        for h in heads {
            let mut p = h;
            while sc.meta(p, "dead-list block")? {
                let b: &[u8; BLOCK] = sc.buf[..BLOCK].try_into().unwrap();
                p = decode_dead(b)
                    .map(|(_, next)| next)
                    .unwrap_or(BlockPtr::NONE);
            }
        }
        // The keyring (encrypted volumes): plaintext, so its checksum is a plain xxh64
        // even with the key; and it must still decode, for this volume.
        if !keyring.is_none() && sc.nodes.insert(keyring.block) {
            let ok = match sc.read(keyring.block, 1) {
                Ok(()) => {
                    let b: &[u8; BLOCK] = sc.buf[..BLOCK].try_into().unwrap();
                    xxh64(b) == keyring.csum
                        && Keyring::decode(b).is_some_and(|r| r.uuid == uuid && !r.slots.is_empty())
                }
                Err(Errno::EINTR) => return Err(Errno::EINTR),
                Err(_) => false,
            };
            if !ok {
                sc.bad
                    .push((keyring.block, Owner::Meta("keyring (the wrapped keys)")));
            }
        }
        if sc.meta(map, "space-map index")? {
            let b: &[u8; BLOCK] = sc.buf[..BLOCK].try_into().unwrap();
            for c in MapIndex::decode(b).map(|i| i.chunks).unwrap_or_default() {
                sc.meta(c, "space-map block")?;
            }
        }
        Ok(())
    })();
    // Done reading: free what was held, and name what was found.
    let errors = v.with_state(|st| {
        let mut errors = Vec::new();
        st.store.release_held();
        for (block, owner) in &sc.bad {
            let what = match owner {
                Owner::Node(t) => format!("tree node of {t}"),
                Owner::Meta(m) => String::from(*m),
                Owner::Data(view, ino, off) => {
                    let p = st
                        .path_in(*view, *ino)
                        .map(|p| format!("{path}{p}"))
                        .unwrap_or_else(|| format!("inode {ino}"));
                    // Every tree that has this block there (shared subtrees were read once).
                    let idx = off / BLOCK as u64;
                    let holders: Vec<&str> = trees
                        .iter()
                        .filter(|(_, tv, _)| {
                            st.exts(*tv, *ino, idx, idx + 1).is_ok_and(|es| {
                                es.iter().any(|e| e.block + (idx - e.start) == *block)
                            })
                        })
                        .map(|(l, _, _)| l.as_str())
                        .collect();
                    format!("data of {p} at offset {off} ({})", holders.join(", "))
                }
            };
            errors.push(ScrubError {
                block: *block,
                what,
            });
        }
        if walked.is_ok() && sc.run.records_last_scrub() {
            st.scrub.0 = crate::platform::now().sec.max(0) as u64;
            st.meta_dirty = true; // persisted with the next commit
        }
        errors
    });
    Ok(Scrubbed {
        walked,
        errors,
        done: sc.done,
        bytes: sc.bytes,
    })
}
