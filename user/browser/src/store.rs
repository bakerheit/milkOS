//! The Browser's profile, `~/Library/Browser`: `bookmarks`, `cookies` (the persistent ones)
//! and `history`, plus the `open` inbox the desktop drops addresses into for a Browser that
//! is already running (`open URL`). Files are written whole to a temporary name and renamed,
//! so a crash leaves the old file, never half of one.

use alloc::format;
use alloc::string::String;

use rt::sys;

pub struct Profile {
    pub home: String,
    pub lib: String,
    pub downloads: String,
}

impl Profile {
    pub fn new(home: &str) -> Profile {
        let home = String::from(home.trim_end_matches('/'));
        Profile {
            lib: format!("{home}/Library/Browser"),
            downloads: format!("{home}/Downloads"),
            home,
        }
    }

    /// Make the folders (before the sandbox closes everything else).
    pub fn ensure(&self) {
        let _ = sys::mkdir(&format!("{}/Library", self.home), 0o755);
        let _ = sys::mkdir(&self.lib, 0o700);
        let _ = sys::mkdir(&self.downloads, 0o755);
    }

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.lib)
    }

    /// A profile file as text ("" if there is none). At most 4 MB.
    pub fn read(&self, name: &str) -> String {
        let p = self.path(name);
        if sys::stat_path(&p, true).is_ok_and(|s| s.size > 4 << 20) {
            return String::new();
        }
        sys::read_file(&p)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// Replace a profile file (write a temporary, then rename over it).
    pub fn write(&self, name: &str, text: &str) -> bool {
        let tmp = self.path(&format!(".{name}.tmp"));
        let Ok(fd) = sys::open_mode(
            &tmp,
            sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
            0o600,
        ) else {
            return false;
        };
        let ok = sys::write_all(fd, text.as_bytes()).is_ok();
        let _ = sys::close(fd);
        ok && sys::rename(&tmp, &self.path(name)).is_ok()
    }

    /// Addresses the desktop asked us to open (`open URL` while we run), emptied.
    pub fn take_inbox(&self) -> alloc::vec::Vec<String> {
        let text = self.read("open");
        if text.is_empty() {
            return alloc::vec::Vec::new();
        }
        let _ = sys::unlink(&self.path("open"));
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(20)
            .map(String::from)
            .collect()
    }
}
