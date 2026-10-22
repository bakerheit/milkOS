//! Files, the clipboard and settings for the Text Editor.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rt::sys;

/// The biggest file the editor opens.
pub const MAX_FILE: usize = 4 << 20;

pub fn read_fd(fd: i32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match sys::read(fd, &mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_FILE {
                    return Err(String::from(
                        "This file is too big to open here (over 4 MB).",
                    ));
                }
            }
            Err(e) => return Err(format!("Can't read the file: {}.", e.name())),
        }
    }
}

pub fn load_path(path: &str) -> Result<Vec<u8>, String> {
    let st = sys::stat_path(path, true).map_err(|e| format!("Can't open it: {}.", e.name()))?;
    if st.size as usize > MAX_FILE {
        return Err(String::from(
            "This file is too big to open here (over 4 MB).",
        ));
    }
    let fd = sys::open(path, sys::O_RDONLY | sys::O_CLOEXEC)
        .map_err(|e| format!("Can't open it: {}.", e.name()))?;
    let r = read_fd(fd);
    let _ = sys::close(fd);
    r
}

pub fn write_fd(fd: i32, bytes: &[u8]) -> Result<(), String> {
    sys::write_all(fd, bytes).map_err(|e| format!("Can't write the file: {}.", e.name()))
}

pub fn save_path(path: &str, bytes: &[u8]) -> Result<(), String> {
    let fd = sys::open_mode(
        path,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        0o644,
    )
    .map_err(|e| format!("Can't save it: {}.", e.name()))?;
    let r = write_fd(fd, bytes);
    let _ = sys::close(fd);
    r
}

/// The system clipboard (`/dev/clipboard`): paste reads it, copy writes it.
pub fn clipboard_get() -> String {
    let Ok(fd) = sys::open("/dev/clipboard", sys::O_RDONLY) else {
        return String::new();
    };
    let r = read_fd(fd).unwrap_or_default();
    let _ = sys::close(fd);
    String::from_utf8_lossy(&r).into_owned()
}

pub fn clipboard_set(s: &str) {
    if let Ok(fd) = sys::open("/dev/clipboard", sys::O_WRONLY) {
        let _ = sys::write_all(fd, s.as_bytes());
        let _ = sys::close(fd);
    }
}

/// The app's own folder: `~/Library/Text Editor`.
pub fn data_dir(home: &str) -> String {
    format!("{home}/Library/Text Editor")
}

fn ensure_dir(home: &str) {
    let _ = sys::mkdir(&format!("{home}/Library"), 0o755);
    let _ = sys::mkdir(&data_dir(home), 0o755);
}

/// Settings: `tab=2|4` and `gutter=0|1`.
pub struct Settings {
    pub tab: usize,
    pub gutter: bool,
}

pub fn load_settings(home: &str) -> Settings {
    let mut s = Settings {
        tab: 4,
        gutter: true,
    };
    let text = sys::read_file(&format!("{}/settings", data_dir(home))).unwrap_or_default();
    for line in String::from_utf8_lossy(&text).lines() {
        match line.split_once('=') {
            Some(("tab", "2")) => s.tab = 2,
            Some(("tab", "4")) => s.tab = 4,
            Some(("gutter", "0")) => s.gutter = false,
            Some(("gutter", "1")) => s.gutter = true,
            _ => {}
        }
    }
    s
}

pub fn save_settings(home: &str, s: &Settings) {
    ensure_dir(home);
    let text = format!("tab={}\ngutter={}\n", s.tab, u8::from(s.gutter));
    let _ = save_path(&format!("{}/settings", data_dir(home)), text.as_bytes());
}

/// A copy of unsaved work, kept when the window is closed on it (the closing can't be refused).
pub fn save_recovery(home: &str, name: &str, bytes: &[u8]) -> Option<String> {
    ensure_dir(home);
    let dir = format!("{}/Unsaved", data_dir(home));
    let _ = sys::mkdir(&dir, 0o755);
    let path = format!("{dir}/{name}");
    save_path(&path, bytes).ok().map(|_| path)
}
