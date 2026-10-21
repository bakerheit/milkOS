//! `/dev/wm`: a control file for the desktop. Write one command per line; fields are separated
//! by tabs (or, with no tabs, the first space):
//!
//! ```text
//! open <app|absolute path>    files, terminal, notes, about, paint, today, spaces; a folder
//!                             opens in Files, a text file in Notes, a /bin program runs
//! notify <title>[\t<subtitle>]
//! today | spaces | desktop | skim
//! ```
//!
//! Reading it gives a status snapshot (`mode`, `space`, one `window` line per window).
//! The `/bin/open`, `/bin/notify`, `/bin/today` and `/bin/spaces` programs are thin wrappers.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::fs::{self, Inode, InodeType, Stat};
use crate::sync::SpinLock;

use super::{Mode, Request, notify, request};

/// What a read of /dev/wm returns; the wm refreshes it.
static STATUS: SpinLock<String> = SpinLock::new(String::new());

pub fn set_status(s: String) {
    *STATUS.lock() = s;
}

/// Run one command line. Returns false for an unknown command.
pub fn command(line: &str) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    let (cmd, rest) = match line.split_once('\t') {
        Some((c, r)) => (c, r),
        None => line.split_once(' ').unwrap_or((line, "")),
    };
    match cmd.trim() {
        "" => true,
        "open" if !rest.trim().is_empty() => {
            request(Request::OpenAny(String::from(rest.trim())));
            true
        }
        "notify" => {
            let (t, s) = rest.split_once('\t').unwrap_or((rest, ""));
            notify(if t.is_empty() { "Hello" } else { t }, s);
            true
        }
        "today" => {
            request(Request::Mode(Mode::Home));
            true
        }
        "spaces" => {
            request(Request::Mode(Mode::Spaces));
            true
        }
        "desktop" => {
            request(Request::Mode(Mode::Desktop));
            true
        }
        "skim" => {
            request(Request::Skim);
            true
        }
        _ => false,
    }
}

struct WmNode;

impl Inode for WmNode {
    fn ino(&self) -> u64 {
        0x776d
    }
    fn dev(&self) -> u64 {
        0
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(0, 0x776d, InodeType::CharDev, 0o666);
        st.rdev = fs::node::makedev(10, 0x77); // misc, like Linux's small control devices
        st
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        let s = STATUS.lock().clone();
        let b = s.as_bytes();
        let off = (off as usize).min(b.len());
        let n = buf.len().min(b.len() - off);
        buf[..n].copy_from_slice(&b[off..off + n]);
        Ok(n)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> fs::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let lines: Vec<&str> = text.split('\n').collect();
        for l in lines {
            if !command(l) {
                return Err(fs::Errno::EINVAL);
            }
        }
        Ok(buf.len())
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Ok(())
    }
}

/// Publish `/dev/wm`.
pub fn init() {
    if fs::stat("/dev/wm").is_err()
        && let Err(e) = fs::devfs::add_device("wm", Arc::new(WmNode))
    {
        crate::kprintln!("wm: cannot publish /dev/wm: {e}");
    }
}
