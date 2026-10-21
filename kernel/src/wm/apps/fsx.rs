//! Small helpers over `crate::fs` for the desktop apps: paths, sorted listings, the home folder.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, InodeType};

use super::super::icons::Icon;

pub const HOME: &str = "/home/milk";
pub const DOCS: &str = "/home/milk/Documents";

/// The usual folders in the home directory, and a starter file, if they are missing.
pub fn ensure_home() {
    for d in ["Documents", "Downloads", "Desktop", "Pictures", "Music"] {
        let _ = fs::mkdir_all(&format!("{HOME}/{d}"), 0o755);
    }
    let todo = format!("{DOCS}/todo.txt");
    if fs::stat(&todo).is_err() {
        let _ = fs::write_file(
            &todo,
            b"- try Ctrl+Space\n- drag a window to another space (F3)\n- write a note\n",
        );
    }
}

/// Join `rel` onto `cwd` and normalise `.`, `..`, `~` and repeated slashes.
pub fn resolve(cwd: &str, rel: &str) -> String {
    let full = if rel == "~" || rel.starts_with("~/") {
        format!("{HOME}{}", &rel[1..])
    } else if rel.starts_with('/') {
        String::from(rel)
    } else {
        format!("{cwd}/{rel}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for p in full.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut s = String::new();
    for p in parts {
        s.push('/');
        s.push_str(p);
    }
    if s.is_empty() { String::from("/") } else { s }
}

/// `~` for the home folder in prompts and titles.
pub fn pretty(path: &str) -> String {
    if path == HOME {
        String::from("~")
    } else if let Some(rest) = path.strip_prefix(HOME)
        && rest.starts_with('/')
    {
        format!("~{rest}")
    } else {
        String::from(path)
    }
}

pub fn basename(path: &str) -> &str {
    match path.rsplit('/').next() {
        Some("") | None => "/",
        Some(b) => b,
    }
}

pub fn parent(path: &str) -> String {
    resolve(path, "..")
}

pub struct Entry {
    pub name: String,
    pub kind: InodeType,
    pub size: u64,
}

/// A directory listing, folders first, then by name.
pub fn list(path: &str) -> fs::Result<Vec<Entry>> {
    let mut v: Vec<Entry> = fs::readdir(path)?
        .into_iter()
        .map(|d| {
            let size = fs::stat(&resolve(path, &d.name)).map_or(0, |s| s.size);
            Entry {
                name: d.name,
                kind: d.kind,
                size,
            }
        })
        .collect();
    v.sort_by(|a, b| {
        let da = a.kind != InodeType::Dir;
        let db = b.kind != InodeType::Dir;
        (da, a.name.to_ascii_lowercase()).cmp(&(db, b.name.to_ascii_lowercase()))
    });
    Ok(v)
}

pub fn is_dir(path: &str) -> bool {
    fs::stat(path).is_ok_and(|s| s.kind == InodeType::Dir)
}

/// Text we are happy to open in Notes.
pub fn is_text(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".txt", ".md", ".sh", ".rs", ".toml", ".list", ".conf"]
        .iter()
        .any(|e| n.ends_with(e))
        || !n.contains('.')
}

/// The artist's file-type tile for an entry (crate::art: file-<kind>).
pub fn art_kind(e: &Entry) -> &'static str {
    art_kind_of(
        &e.name,
        e.kind == InodeType::Dir || e.kind == InodeType::Symlink,
    )
}

/// [`art_kind`] by name (and whether it is a folder).
pub fn art_kind_of(name: &str, dir: bool) -> &'static str {
    if dir {
        return "folder";
    }
    let n = name.to_ascii_lowercase();
    let ext = |xs: &[&str]| xs.iter().any(|x| n.ends_with(x));
    if ext(&[".png", ".jpg", ".jpeg", ".gif", ".bmp"]) {
        "image"
    } else if ext(&[".wav", ".mp3", ".ogg", ".flac"]) {
        "audio"
    } else if ext(&[".zip", ".tar", ".gz", ".tgz", ".xz"]) {
        "archive"
    } else if ext(&[".milk", ".pkg"]) {
        "package"
    } else {
        "document"
    }
}

pub fn icon_for(e: &Entry) -> Icon {
    match e.kind {
        InodeType::Dir => Icon::Folder,
        InodeType::Symlink => Icon::Folder,
        InodeType::CharDev | InodeType::BlockDev => Icon::Gear,
        _ => {
            let n = e.name.to_ascii_lowercase();
            if n.ends_with(".md") {
                Icon::Note
            } else if n.ends_with(".sh") {
                Icon::Term
            } else if n.ends_with(".png") || n.ends_with(".jpg") {
                Icon::Photo
            } else if n.ends_with(".wav") {
                Icon::Music
            } else {
                Icon::Doc
            }
        }
    }
}

pub fn human_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{} KB", n.div_ceil(1024))
    } else {
        format!("{} MB", n.div_ceil(1024 * 1024))
    }
}

/// Write `text` to an existing control file (sysfs / devfs): open for writing, no create /
/// truncate.
pub fn ctl_write(path: &str, text: &str) -> fs::Result<()> {
    let f = fs::open(path, fs::abi::O_WRONLY, 0)?;
    f.write(text.as_bytes())?;
    Ok(())
}
