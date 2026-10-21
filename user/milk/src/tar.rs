//! Read a ustar archive (what `scripts/mkpkg.py` writes): regular files and directories.

use alloc::string::String;
use alloc::vec::Vec;

pub struct Entry<'a> {
    pub path: String,
    pub mode: u32,
    pub data: &'a [u8],
}

fn octal(b: &[u8]) -> Option<u64> {
    let s = core::str::from_utf8(b).ok()?;
    let s = s.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn cstr(b: &[u8]) -> &str {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..end]).unwrap_or("")
}

/// All regular-file entries, in order. Err on a malformed archive or an unsafe path
/// (absolute, or with `..`).
pub fn entries(t: &[u8]) -> Result<Vec<Entry<'_>>, String> {
    let mut out = Vec::new();
    let mut k = 0;
    while k + 512 <= t.len() {
        let h = &t[k..k + 512];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        // Header checksum: sum of the header with the checksum field as spaces.
        let want = octal(&h[148..156]).ok_or("bad tar checksum field")?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    b as u64
                }
            })
            .sum();
        if sum != want {
            return Err(String::from("tar header checksum mismatch"));
        }
        let name = cstr(&h[0..100]);
        let prefix = if &h[257..262] == b"ustar" {
            cstr(&h[345..500])
        } else {
            ""
        };
        let path = if prefix.is_empty() {
            String::from(name)
        } else {
            alloc::format!("{prefix}/{name}")
        };
        let size = octal(&h[124..136]).ok_or("bad tar size")? as usize;
        let mode = octal(&h[100..108]).unwrap_or(0o644) as u32;
        let typ = h[156];
        let data_at = k + 512;
        if data_at + size > t.len() {
            return Err(String::from("truncated tar"));
        }
        if path.starts_with('/') || path.split('/').any(|p| p == "..") {
            return Err(alloc::format!("unsafe path in package: {path}"));
        }
        if typ == b'0' || typ == 0 {
            out.push(Entry {
                path,
                mode,
                data: &t[data_at..data_at + size],
            });
        } else if typ != b'5' {
            return Err(alloc::format!(
                "unsupported tar entry type {:?} for {path}",
                typ as char
            ));
        }
        k = data_at + size.div_ceil(512) * 512;
    }
    Ok(out)
}
