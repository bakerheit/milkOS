//! The kernel clipboard: one piece of text, shared by every app (Cmd/Ctrl+C / X / V in Notes,
//! Cmd+C / V in the Terminal) and by programs through `/dev/clipboard` (read it to paste,
//! write it to copy).

use alloc::string::String;
use alloc::sync::Arc;

use crate::fs::{self, Inode, InodeType, Stat};
use crate::sync::SpinLock;

static CLIP: SpinLock<String> = SpinLock::new(String::new());

pub fn set(s: &str) {
    *CLIP.lock() = String::from(s);
}

pub fn get() -> String {
    CLIP.lock().clone()
}

struct ClipNode;

impl Inode for ClipNode {
    fn ino(&self) -> u64 {
        0x636c
    }
    fn dev(&self) -> u64 {
        0
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(0, 0x636c, InodeType::CharDev, 0o666);
        st.rdev = fs::node::makedev(10, 0x63);
        st
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        let c = CLIP.lock();
        let b = c.as_bytes();
        let off = (off as usize).min(b.len());
        let n = buf.len().min(b.len() - off);
        buf[..n].copy_from_slice(&b[off..off + n]);
        Ok(n)
    }
    /// A write at offset 0 replaces the clipboard; later chunks of the same write append.
    fn write_at(&self, off: u64, buf: &[u8]) -> fs::Result<usize> {
        let mut c = CLIP.lock();
        if off == 0 {
            c.clear();
        }
        c.push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        CLIP.lock().clear();
        Ok(())
    }
}

pub fn init() {
    if fs::stat("/dev/clipboard").is_err()
        && let Err(e) = fs::devfs::add_device("clipboard", Arc::new(ClipNode))
    {
        crate::kprintln!("wm: cannot publish /dev/clipboard: {e}");
    }
}
