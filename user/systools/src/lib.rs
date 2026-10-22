//! The system calls behind `processes`, `services`, `system`, `users` and `admin`; the rules
//! (help, arguments, plain-English messages, formatting) are in user/syskit.
#![no_std]

extern crate alloc;

pub mod watch;

use alloc::string::String;
use alloc::vec::Vec;

use rt::sys;
use rt::unix::{self, Name};
use syskit::procs::Row;

/// A file's text ("" if it can't be read).
pub fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Every process in /proc, by PID.
pub fn processes() -> Vec<Row> {
    let mut pids: Vec<u32> = sys::read_dir("/proc")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(n, _)| n.parse().ok())
        .collect();
    pids.sort_unstable();
    pids.into_iter()
        .filter_map(|pid| syskit::procs::parse_status(&text(&alloc::format!("/proc/{pid}/status"))))
        .collect()
}

/// One request to milkd (/System/Services/Sockets/milkd.sock): its reply lines, or why not.
pub fn ask_milkd(req: &str) -> Result<Vec<String>, String> {
    let sock = syskit::services::SOCK;
    let fd = unix::socket(unix::SOCK_STREAM | unix::SOCK_CLOEXEC)
        .map_err(|e| alloc::format!("can't make a socket ({e})"))?;
    if let Err(e) = unix::connect(fd, &Name::Path(sock)) {
        let _ = sys::close(fd);
        return Err(alloc::format!(
            "can't reach milkd at {sock} ({e}): is it running?"
        ));
    }
    let sent = sys::write_all(fd, alloc::format!("{req}\n").as_bytes());
    let mut data = Vec::new();
    let mut b = [0u8; 4096];
    while sent.is_ok() {
        match sys::read(fd, &mut b) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                data.extend_from_slice(&b[..n]);
                if syskit::services::complete(&String::from_utf8_lossy(&data)) {
                    break;
                }
            }
        }
    }
    let _ = sys::close(fd);
    syskit::services::reply(&String::from_utf8_lossy(&data))
}

/// An audit record through /dev/audit (the kernel files it as `user.<event>` with this
/// process's pid and program, the time and the hash chain). `detail` is cut to 200 printable
/// ASCII characters (anything else becomes `?`), so a name can never make a bad record.
pub fn audit(event: &str, detail: &str) {
    let clean: String = detail
        .chars()
        .take(200)
        .map(|c| {
            if c == ' ' || c.is_ascii_graphic() {
                c
            } else {
                '?'
            }
        })
        .collect();
    if let Ok(fd) = sys::open("/dev/audit", sys::O_WRONLY | sys::O_CLOEXEC) {
        let _ = sys::write_all(fd, alloc::format!("{event} {clean}\n").as_bytes());
        let _ = sys::close(fd);
    }
}

/// Give up the root a set-user-ID program starts with, for good: back to the caller's real
/// user id (real, effective and saved). For the subcommands that need no privilege.
pub fn drop_root() -> bool {
    let r = sys::getuid();
    sys::setresuid(r, r, r).is_ok() && sys::geteuid() == r
}
