//! Installed `milk` packages on the desktop (docs/packages.md): every
//! `/Applications/<name>/manifest` with a `dock Label #RRGGBB` line gets a dock tile (after
//! the built-in ones) and a Skim entry. Its program is the manifest's `exec`.
//!
//! Window apps (they link `user/gui`) start like Paint, as a process whose window is the
//! app; terminal programs (fortune, snake) open in a new Terminal tab. Which is which: the
//! manifest doesn't say, so we look for the gui crate's font table (the kernel's Unifont
//! glyphs, `include!`d by user/gui) in the executable. A trailing `window` / `terminal` word
//! on the dock line, if packages ever grow one, overrides that.
//!
//! The list is rescanned every few seconds by the desktop (`refresh`), so an install from
//! the Terminal shows up in the dock without a restart.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs;
use crate::gfx::{Color, rgb};
use crate::sync::SpinLock;

pub const DIR: &str = "/Applications";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pkg {
    pub name: String,
    /// Dock label.
    pub label: String,
    pub color: Color,
    pub description: String,
    /// The program: /Applications/<name>/<exec>.
    pub exec: String,
    /// Opens its own window (else it runs in a Terminal tab).
    pub window: bool,
}

struct Cache {
    /// Per manifest: (path, its size, the parsed package). Exec checks are not repeated.
    pkgs: Vec<(String, u64, Pkg)>,
}

static CACHE: SpinLock<Cache> = SpinLock::new(Cache { pkgs: Vec::new() });

fn parse_color(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Does the executable link the gui crate? (Its 8x16 font table is in there.)
fn links_gui(path: &str) -> bool {
    let Ok(bytes) = fs::read_file(path) else {
        return false;
    };
    // Glyphs 'A'..'H': 128 bytes that only a copy of the font table contains.
    let start = ('A' as u32 - crate::font_data::FIRST_CHAR) as usize;
    let needle: Vec<u8> = crate::font_data::GLYPHS[start..start + 8]
        .iter()
        .flatten()
        .copied()
        .collect();
    bytes.windows(needle.len()).any(|w| w == needle.as_slice())
}

fn parse(name: &str, text: &str) -> Option<Pkg> {
    let (mut label, mut color, mut exec, mut desc, mut kind) =
        (None, None, None, String::new(), None);
    let mut pname = String::from(name);
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.trim().split_once(' ') else {
            continue;
        };
        let v = v.trim();
        match k {
            "name" => pname = String::from(v),
            "exec" => exec = Some(String::from(v.split_whitespace().next()?)),
            "description" => desc = String::from(v),
            "dock" => {
                // dock Label #RRGGBB [window|terminal]
                let mut parts: Vec<&str> = v.split_whitespace().collect();
                if let Some(last) = parts.last()
                    && matches!(*last, "window" | "terminal")
                {
                    kind = Some(*last == "window");
                    parts.pop();
                }
                if let Some(c) = parts.last().and_then(|c| parse_color(c)) {
                    color = Some(c);
                    parts.pop();
                }
                if !parts.is_empty() {
                    label = Some(parts.join(" "));
                }
            }
            _ => {}
        }
    }
    let exec = format!("{DIR}/{name}/{}", exec?);
    // Only real files under the package (no .. tricks; milk refuses those anyway).
    if exec.contains("/../") || fs::stat(&exec).is_err() {
        return None;
    }
    let window = kind.unwrap_or_else(|| links_gui(&exec));
    Some(Pkg {
        label: label?,
        color: color.unwrap_or(rgb(0xEB, 0xB6, 0x50)),
        description: desc,
        exec,
        window,
        name: pname,
    })
}

/// Rescan /Applications. True if the list changed.
pub fn refresh() -> bool {
    let mut found: Vec<(String, u64, Pkg)> = Vec::new();
    let mut entries = fs::readdir(DIR).unwrap_or_default();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let old: Vec<(String, u64, Pkg)> = CACHE.lock().pkgs.clone();
    for e in entries {
        if e.name.starts_with('.') {
            continue;
        }
        let path = format!("{DIR}/{}/manifest", e.name);
        let Ok(st) = fs::stat(&path) else {
            continue;
        };
        if let Some(p) = old.iter().find(|(p, sz, _)| *p == path && *sz == st.size) {
            found.push(p.clone());
            continue;
        }
        if let Some(pkg) = fs::read_to_string(&path)
            .ok()
            .and_then(|t| parse(&e.name, &t))
        {
            found.push((path, st.size, pkg));
        }
    }
    let changed = found.len() != old.len() || found.iter().zip(&old).any(|(a, b)| a.2 != b.2);
    if changed {
        CACHE.lock().pkgs = found;
    }
    changed
}

/// The packages with dock entries, in name order.
pub fn all() -> Vec<Pkg> {
    CACHE
        .lock()
        .pkgs
        .iter()
        .map(|(_, _, p)| p.clone())
        .collect()
}

pub fn get(i: usize) -> Option<Pkg> {
    CACHE.lock().pkgs.get(i).map(|(_, _, p)| p.clone())
}

pub fn count() -> usize {
    CACHE.lock().pkgs.len()
}
