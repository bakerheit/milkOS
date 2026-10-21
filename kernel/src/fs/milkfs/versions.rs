//! Per-file version history ("Browse All Versions"): the distinct contents a file had
//! across the snapshots of its volume, and restoring one of them.
//!
//! A file is followed by inode number (so its history survives renames); in a snapshot
//! where that inode doesn't exist, the file's current path is tried instead (editors that
//! save with write-temp-then-rename get a new inode every time). A version's content hash is
//! xxh64 over the size and every extent's block checksum (the checksums already cover the
//! data), so identical versions collapse without reading any file data.

use alloc::string::String;
use alloc::vec::Vec;

use super::format::*;
use super::items::LIVE;
use super::volume::State;
use crate::fs::errno::{Errno, Result};
use crate::fs::node::{S_IFMT, S_IFREG};

/// One distinct version of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// The (oldest) snapshot that has this content; `None` = the current file.
    pub snapshot: Option<String>,
    /// When that snapshot was taken (unix seconds); for the current file: now.
    pub taken: u64,
    /// The file's own mtime in that version.
    pub mtime: i64,
    pub size: u64,
    /// Content hash (equal hashes = identical contents).
    pub hash: u64,
}

impl State {
    /// Content hash of `ino` in `view`, from its extent checksums.
    pub fn content_hash(&self, view: u64, ino: u64) -> Result<(u64, InodeItem)> {
        let i = self.inode(view, ino)?;
        if i.mode & S_IFMT != S_IFREG {
            return Err(Errno::EISDIR);
        }
        let mut bytes = Vec::with_capacity(16 + 16 * 32);
        bytes.extend_from_slice(&i.size.to_le_bytes());
        for (idx, p) in self.extents(view, ino, 0)? {
            bytes.extend_from_slice(&idx.to_le_bytes());
            bytes.extend_from_slice(&p.csum.to_le_bytes());
        }
        bytes.extend_from_slice(&self.inline_bytes(view, ino)?.unwrap_or_default());
        Ok((xxh64(&bytes), i))
    }

    /// Resolve a volume-relative path (`/home/milk/x`) in `view` to an inode.
    pub fn resolve_in(&self, view: u64, path: &str) -> Option<u64> {
        let mut cur = ROOT_INO;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            cur = self.lookup(view, cur, part).ok()??.ino;
        }
        Some(cur)
    }

    /// The distinct versions of live inode `ino`, oldest first, the current one last.
    pub fn versions_of(&self, ino: u64) -> Result<Vec<Version>> {
        let path = self.path_in(LIVE, ino);
        let mut out: Vec<Version> = Vec::new();
        let push = |v: Version, out: &mut Vec<Version>| {
            if out.last().is_none_or(|l| l.hash != v.hash) {
                out.push(v);
            }
        };
        for s in &self.snaps {
            let found = match self.inode(s.generation, ino) {
                Ok(i) if i.mode & S_IFMT == S_IFREG => Some(ino),
                _ => path
                    .as_deref()
                    .and_then(|p| self.resolve_in(s.generation, p)),
            };
            let Some(i2) = found else { continue };
            let Ok((hash, item)) = self.content_hash(s.generation, i2) else {
                continue;
            };
            push(
                Version {
                    snapshot: Some(s.name.clone()),
                    taken: s.created,
                    mtime: item.mtime.0,
                    size: item.size,
                    hash,
                },
                &mut out,
            );
        }
        let (hash, item) = self.content_hash(LIVE, ino)?;
        let now = crate::fs::clock::now().sec as u64;
        if out.last().is_some_and(|l| l.hash == hash) {
            // The current content is the newest snapshot's: show it as current.
            out.pop();
        }
        out.push(Version {
            snapshot: None,
            taken: now,
            mtime: item.mtime.0,
            size: item.size,
            hash,
        });
        Ok(out)
    }
}
