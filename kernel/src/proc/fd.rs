//! Per-process file-descriptor table. An fd points at a shared open-file object
//! (`Arc<dyn File>`), so `dup` and `fork` share the file offset, as in Unix.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::console::Console;
use super::errno::*;
use super::fs::File;

pub const MAX_FDS: usize = 256;

#[derive(Clone)]
pub struct FdEntry {
    pub file: Arc<dyn File>,
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

    /// stdin, stdout, stderr on the console.
    pub fn with_console() -> Self {
        let c: Arc<dyn File> = Arc::new(Console);
        let mut t = Self::new();
        for _ in 0..3 {
            t.alloc(c.clone(), false).unwrap();
        }
        t
    }

    pub fn get(&self, fd: i64) -> KResult<Arc<dyn File>> {
        usize::try_from(fd)
            .ok()
            .and_then(|i| self.fds.get(i))
            .and_then(|e| e.as_ref())
            .map(|e| e.file.clone())
            .ok_or(EBADF)
    }

    /// Lowest free fd at or above `min`.
    pub fn alloc_from(&mut self, file: Arc<dyn File>, cloexec: bool, min: usize) -> KResult<i64> {
        let i = (min..MAX_FDS)
            .find(|&i| self.fds.get(i).is_none_or(|e| e.is_none()))
            .ok_or(EMFILE)?;
        if i >= self.fds.len() {
            self.fds.resize(i + 1, None);
        }
        self.fds[i] = Some(FdEntry { file, cloexec });
        Ok(i as i64)
    }

    pub fn alloc(&mut self, file: Arc<dyn File>, cloexec: bool) -> KResult<i64> {
        self.alloc_from(file, cloexec, 0)
    }

    /// Put `file` at exactly `fd` (dup3), returning what was there.
    pub fn set(&mut self, fd: i64, file: Arc<dyn File>, cloexec: bool) -> KResult<Option<FdEntry>> {
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

    pub fn count(&self) -> usize {
        self.fds.iter().filter(|e| e.is_some()).count()
    }
}
