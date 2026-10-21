//! Per-process file-descriptor table. An fd points at a shared `fs::OpenFile`
//! (`Arc<OpenFile>`), so `dup` and `fork` share the file offset, as in Linux.
//! Close-on-exec is a property of the fd, kept here.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::errno::*;
use crate::fs::{self, OpenFile, abi};

pub const MAX_FDS: usize = 256;

#[derive(Clone)]
pub struct FdEntry {
    pub file: Arc<OpenFile>,
    pub cloexec: bool,
}

#[derive(Clone, Default)]
pub struct FdTable {
    fds: Vec<Option<FdEntry>>,
}

impl FdTable {
    pub fn new() -> Self {
        Self { fds: Vec::new() }
    }

    /// stdin, stdout, stderr: one shared open of /dev/console.
    pub fn with_console() -> KResult<Self> {
        Self::with_tty("/dev/console")
    }

    /// stdin, stdout, stderr: one shared open of the terminal device at `path` (e.g. the
    /// desktop Terminal's `/dev/tty1`).
    pub fn with_tty(path: &str) -> KResult<Self> {
        let c = Arc::new(fs::open(path, abi::O_RDWR, 0)?);
        let mut t = Self::new();
        for _ in 0..3 {
            t.alloc(c.clone(), false)?;
        }
        Ok(t)
    }

    pub fn get(&self, fd: i64) -> KResult<Arc<OpenFile>> {
        usize::try_from(fd)
            .ok()
            .and_then(|i| self.fds.get(i))
            .and_then(|e| e.as_ref())
            .map(|e| e.file.clone())
            .ok_or(EBADF)
    }

    /// Lowest free fd at or above `min`.
    pub fn alloc_from(&mut self, file: Arc<OpenFile>, cloexec: bool, min: usize) -> KResult<i64> {
        let i = (min..MAX_FDS)
            .find(|&i| self.fds.get(i).is_none_or(|e| e.is_none()))
            .ok_or(EMFILE)?;
        if i >= self.fds.len() {
            self.fds.resize(i + 1, None);
        }
        self.fds[i] = Some(FdEntry { file, cloexec });
        Ok(i as i64)
    }

    pub fn alloc(&mut self, file: Arc<OpenFile>, cloexec: bool) -> KResult<i64> {
        self.alloc_from(file, cloexec, 0)
    }

    /// Put `file` at exactly `fd` (dup3), returning what was there.
    pub fn set(&mut self, fd: i64, file: Arc<OpenFile>, cloexec: bool) -> KResult<Option<FdEntry>> {
        let i = usize::try_from(fd)
            .ok()
            .filter(|&i| i < MAX_FDS)
            .ok_or(EBADF)?;
        if i >= self.fds.len() {
            self.fds.resize(i + 1, None);
        }
        Ok(self.fds[i].replace(FdEntry { file, cloexec }))
    }

    /// Remove `fd`; the caller drops the entry (outside any lock).
    pub fn close(&mut self, fd: i64) -> KResult<FdEntry> {
        usize::try_from(fd)
            .ok()
            .and_then(|i| self.fds.get_mut(i))
            .and_then(|e| e.take())
            .ok_or(EBADF)
    }

    /// Drop every close-on-exec fd; returns them so the caller can drop them unlocked.
    pub fn take_cloexec(&mut self) -> Vec<FdEntry> {
        self.fds
            .iter_mut()
            .filter(|e| e.as_ref().is_some_and(|e| e.cloexec))
            .filter_map(|e| e.take())
            .collect()
    }

    pub fn cloexec(&self, fd: i64) -> KResult<bool> {
        self.entry(fd).map(|e| e.cloexec)
    }

    pub fn set_cloexec(&mut self, fd: i64, on: bool) -> KResult<()> {
        usize::try_from(fd)
            .ok()
            .and_then(|i| self.fds.get_mut(i))
            .and_then(|e| e.as_mut())
            .map(|e| e.cloexec = on)
            .ok_or(EBADF)
    }

    fn entry(&self, fd: i64) -> KResult<&FdEntry> {
        usize::try_from(fd)
            .ok()
            .and_then(|i| self.fds.get(i))
            .and_then(|e| e.as_ref())
            .ok_or(EBADF)
    }

    pub fn count(&self) -> usize {
        self.fds.iter().filter(|e| e.is_some()).count()
    }
}
