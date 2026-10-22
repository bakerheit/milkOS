//! The boot initramfs: a cpio "newc" archive built from `rootfs/` + `initramfs.list` by
//! `kernel/build.rs`, embedded in the kernel image and unpacked into the root tmpfs.
//! Unpacking copies no file data: each tmpfs file points at its bytes in the archive
//! (`TmpNode::set_rom`) until something writes to it.

use alloc::string::String;

use super::abi::{O_CREAT, O_TRUNC, O_WRONLY};
use super::errno::{Errno, Result};
use super::node::{S_IFDIR, S_IFLNK, S_IFMT, S_IFREG, downcast};
use super::ops;
use super::tmpfs::TmpNode;
use crate::kprintln;

static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.cpio"));

/// Size of the built-in archive (it lives in the kernel image's read-only data).
pub fn archive_len() -> usize {
    ARCHIVE.len()
}

/// One archive member.
pub struct Entry<'a> {
    pub name: &'a str,
    pub mode: u32,
    pub mtime: u32,
    pub data: &'a [u8],
}

fn hex(field: &[u8]) -> Option<u32> {
    let s = core::str::from_utf8(field).ok()?;
    u32::from_str_radix(s, 16).ok()
}

/// Iterate the members of a newc archive. Stops at `TRAILER!!!` or on a malformed header.
pub fn entries(archive: &[u8]) -> impl Iterator<Item = Result<Entry<'_>>> {
    let mut off = 0usize;
    let mut done = false;
    core::iter::from_fn(move || {
        if done {
            return None;
        }
        let r = (|| {
            let h = archive.get(off..off + 110).ok_or(Errno::EINVAL)?;
            if &h[..6] != b"070701" {
                return Err(Errno::EINVAL);
            }
            let field = |i: usize| hex(&h[6 + i * 8..14 + i * 8]).ok_or(Errno::EINVAL);
            let mode = field(1)?;
            let mtime = field(5)?;
            let size = field(6)? as usize;
            let namesize = field(11)? as usize;
            let name_start = off + 110;
            let name_bytes = archive
                .get(name_start..name_start + namesize.saturating_sub(1))
                .ok_or(Errno::EINVAL)?;
            let name = core::str::from_utf8(name_bytes).map_err(|_| Errno::EINVAL)?;
            let data_start = (name_start + namesize).next_multiple_of(4);
            let data = archive
                .get(data_start..data_start + size)
                .ok_or(Errno::EINVAL)?;
            off = (data_start + size).next_multiple_of(4);
            Ok(Entry {
                name,
                mode,
                mtime,
                data,
            })
        })();
        match r {
            Ok(e) if e.name == "TRAILER!!!" => {
                done = true;
                None
            }
            Err(e) => {
                done = true;
                Some(Err(e))
            }
            ok => Some(ok),
        }
    })
}

/// Create or replace the file at `path` with `data`, which outlives the file (the archive):
/// on tmpfs the file just points at it, elsewhere the bytes are written.
fn put_file(path: &str, data: &'static [u8]) -> Result<()> {
    let f = ops::open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)?;
    if let Some(node) = downcast::<TmpNode>(f.inode()) {
        return node.set_rom(data);
    }
    drop(f);
    ops::write_file(path, data)
}

/// Unpack `archive` under `/`. Directories that already exist are fine.
/// Returns (files, dirs, symlinks, bytes).
pub fn unpack(archive: &'static [u8]) -> Result<(usize, usize, usize, usize)> {
    let (mut files, mut dirs, mut links, mut bytes) = (0, 0, 0, 0);
    for e in entries(archive) {
        let e = e?;
        let name = e.name.trim_start_matches("./").trim_start_matches('/');
        if name.is_empty() || name == "." {
            continue;
        }
        let mut path = String::from("/");
        path.push_str(name);
        if let Some(slash) = path.rfind('/')
            && slash > 0
        {
            ops::mkdir_all(&path[..slash], 0o755)?;
        }
        let perm = e.mode & 0o7777;
        match e.mode & S_IFMT {
            S_IFDIR => {
                match ops::mkdir(&path, perm) {
                    Ok(()) | Err(Errno::EEXIST) => {}
                    Err(err) => return Err(err),
                }
                dirs += 1;
            }
            S_IFREG => {
                put_file(&path, e.data)?;
                ops::chmod(&path, perm)?;
                files += 1;
                bytes += e.data.len();
            }
            S_IFLNK => {
                let target = core::str::from_utf8(e.data).map_err(|_| Errno::EINVAL)?;
                ops::symlink(target, &path)?;
                links += 1;
            }
            _ => kprintln!("initramfs: skipping {path} (unsupported type {:o})", e.mode),
        }
    }
    Ok((files, dirs, links, bytes))
}

/// Unpack the built-in archive into the root file system.
pub fn load() {
    match unpack(ARCHIVE) {
        Ok((f, d, l, b)) => kprintln!(
            "fs: initramfs unpacked: {f} files, {d} dirs, {l} symlinks, {b} bytes (archive {} bytes)",
            ARCHIVE.len()
        ),
        Err(e) => kprintln!("fs: initramfs unpack failed: {e}"),
    }
}
