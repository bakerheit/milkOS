//! A RAM-backed block device (512-byte sectors over PMM frames). Used by the boot self-test
//! to build a throwaway milkfs volume, so destructive tests never touch a real disk.
//! Not registered in `blockdev` (no `/dev` node); the frames are freed on drop.

use alloc::string::String;
use alloc::vec::Vec;
use core::ptr;

use super::blockdev::BlockDevice;
use super::errno::{Errno, Result};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::pmm;
use crate::sched::Mutex;

const SECTOR: usize = 512;

pub struct RamDisk {
    name: String,
    frames: Mutex<Vec<PhysAddr>>,
    sectors: u64,
}

impl RamDisk {
    /// A zeroed disk of `bytes` (rounded up to whole pages). ENOMEM if RAM is short.
    pub fn new(name: &str, bytes: usize) -> Result<RamDisk> {
        let n = bytes.div_ceil(PAGE_SIZE);
        let mut frames = Vec::with_capacity(n);
        for _ in 0..n {
            match pmm::alloc_frame_zeroed() {
                Some(f) => frames.push(f),
                None => {
                    for f in frames {
                        pmm::free_frame(f);
                    }
                    return Err(Errno::ENOMEM);
                }
            }
        }
        Ok(RamDisk {
            name: String::from(name),
            sectors: (n * PAGE_SIZE / SECTOR) as u64,
            frames: Mutex::new(frames),
        })
    }

    fn io(&self, lba: u64, len: usize, mut f: impl FnMut(*mut u8, usize, usize)) -> Result<()> {
        if !len.is_multiple_of(SECTOR) || lba + (len / SECTOR) as u64 > self.sectors {
            return Err(Errno::EINVAL);
        }
        let frames = self.frames.lock();
        let mut done = 0;
        while done < len {
            let pos = lba as usize * SECTOR + done;
            let (pg, off) = (pos / PAGE_SIZE, pos % PAGE_SIZE);
            let chunk = (PAGE_SIZE - off).min(len - done);
            // SAFETY: our frame, `off + chunk <= PAGE_SIZE`.
            let p = unsafe { (phys_to_virt(frames[pg]) as *mut u8).add(off) };
            f(p, done, chunk);
            done += chunk;
        }
        Ok(())
    }
}

impl Drop for RamDisk {
    fn drop(&mut self) {
        for f in core::mem::take(&mut *self.frames.lock()) {
            pmm::free_frame(f);
        }
    }
}

impl BlockDevice for RamDisk {
    fn name(&self) -> &str {
        &self.name
    }
    fn block_size(&self) -> usize {
        SECTOR
    }
    fn num_blocks(&self) -> u64 {
        self.sectors
    }
    fn read_only(&self) -> bool {
        false
    }
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();
        let dst = buf.as_mut_ptr();
        // SAFETY: `at + n <= len`, ranges don't overlap (disk frames vs caller buffer).
        self.io(lba, len, |p, at, n| unsafe {
            ptr::copy_nonoverlapping(p, dst.add(at), n)
        })
    }
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let src = buf.as_ptr();
        // SAFETY: as above.
        self.io(lba, buf.len(), |p, at, n| unsafe {
            ptr::copy_nonoverlapping(src.add(at), p, n)
        })
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}
