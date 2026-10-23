//! Downloads: which answers are files to save rather than pages to show, what to call the
//! file, and sizes in words.

use alloc::format;
use alloc::string::String;

use milkweb::url::Url;

/// Content types the Browser shows itself.
fn shown(ct: &str) -> bool {
    let t = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        t.as_str(),
        "" | "text/html" | "application/xhtml+xml" | "text/plain" | "text/xml" | "application/xml"
    )
}

/// Is a response with these headers a download? (An attachment, or a type that isn't a page.)
pub fn is_download(content_type: &str, disposition: Option<&str>) -> bool {
    let attachment = disposition.is_some_and(|d| {
        d.split(';')
            .next()
            .is_some_and(|k| k.trim().eq_ignore_ascii_case("attachment"))
    });
    attachment || !shown(content_type)
}

/// `%XX` decoded (bad escapes kept as they are), as UTF-8 (lossy).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = alloc::vec::Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A safe file name: no folders, no control characters, no leading dot, at most 100
/// characters (the extension kept).
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    s = String::from(s.trim().trim_start_matches('.').trim());
    if s.chars().count() > 100 {
        let (stem, ext) = match s.rfind('.') {
            Some(i) if s.len() - i <= 10 => (String::from(&s[..i]), String::from(&s[i..])),
            _ => (s.clone(), String::new()),
        };
        let keep: String = stem.chars().take(100 - ext.chars().count()).collect();
        s = keep + &ext;
    }
    if s.is_empty() {
        String::from("download")
    } else {
        s
    }
}

/// The file name for a download from `url` (Content-Disposition's `filename*` or `filename`,
/// else the last part of the path), made safe.
pub fn file_name(url: &Url, disposition: Option<&str>) -> String {
    if let Some(d) = disposition {
        let mut plain = None;
        for part in d.split(';').skip(1) {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "filename*" {
                // RFC 5987: charset'lang'percent-encoded.
                if let Some(enc) = v.splitn(3, '\'').nth(2) {
                    return sanitize(&percent_decode(enc));
                }
            } else if k == "filename" {
                plain = Some(String::from(v.trim_matches('"')));
            }
        }
        if let Some(p) = plain {
            return sanitize(&p);
        }
    }
    let path = url.path.split('?').next().unwrap_or("");
    sanitize(&percent_decode(path.rsplit('/').next().unwrap_or("")))
}

/// `name`, or `name (2).ext`, `name (3).ext`, ... whichever `exists` says is free.
pub fn unique(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return String::from(name);
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..10_000 {
        let c = format!("{stem} ({n}){ext}");
        if !exists(&c) {
            return c;
        }
    }
    format!("{stem} (copy){ext}")
}

/// "512 bytes", "12.5 KB", "3.4 MB".
pub fn human_size(n: usize) -> String {
    if n < 1024 {
        return format!("{n} bytes");
    }
    let (v, unit) = if n < 1024 * 1024 {
        (n * 10 / 1024, "KB")
    } else {
        (n / 1024 * 10 / 1024, "MB")
    };
    format!("{}.{} {unit}", v / 10, v % 10)
}

/// The progress line: "cows.zip: 1.2 MB of 3.4 MB (35%)".
pub fn progress_line(name: &str, got: usize, total: Option<usize>) -> String {
    match total {
        Some(t) if t > 0 => format!(
            "{name}: {} of {} ({}%)",
            human_size(got),
            human_size(t),
            (got as u64 * 100 / t as u64).min(100)
        ),
        _ => format!("{name}: {}", human_size(got)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide() {
        assert!(!is_download("text/html; charset=utf-8", None));
        assert!(!is_download("", None));
        assert!(is_download("application/zip", None));
        assert!(is_download("image/png", None));
        assert!(is_download(
            "text/html",
            Some("attachment; filename=a.html")
        ));
        assert!(!is_download("text/plain", Some("inline")));
    }

    #[test]
    fn names() {
        let u = Url::parse("http://milk.test/files/My%20Cows.tar.gz?x=1").unwrap();
        assert_eq!(file_name(&u, None), "My Cows.tar.gz");
        assert_eq!(
            file_name(&u, Some("attachment; filename=\"report.pdf\"")),
            "report.pdf"
        );
        assert_eq!(
            file_name(
                &u,
                Some("attachment; filename*=UTF-8''caf%C3%A9.txt; filename=x")
            ),
            "café.txt"
        );
        assert_eq!(
            file_name(&u, Some("attachment; filename=\"../../etc/passwd\"")),
            "passwd"
        );
        assert_eq!(
            file_name(&u, Some("attachment; filename=\".hidden\"")),
            "hidden"
        );
        let root = Url::parse("http://milk.test/").unwrap();
        assert_eq!(file_name(&root, None), "download");
        assert_eq!(sanitize("a\u{0}b\nc:d"), "a_b_c_d");
        let long = "x".repeat(300) + ".zip";
        let s = sanitize(&long);
        assert_eq!(s.chars().count(), 100);
        assert!(s.ends_with(".zip"));
        assert_eq!(
            file_name(&u, Some("attachment; filename*=bad%ZZ")),
            "My Cows.tar.gz"
        );
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("a%4"), "a%4");
    }

    #[test]
    fn unique_names_and_sizes() {
        let taken = ["a.zip", "a (2).zip", "noext"];
        let ex = |n: &str| taken.contains(&n);
        assert_eq!(unique("b.zip", ex), "b.zip");
        assert_eq!(unique("a.zip", ex), "a (3).zip");
        assert_eq!(unique("noext", ex), "noext (2)");
        assert_eq!(human_size(10), "10 bytes");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024 + 400 * 1024), "3.3 MB");
        assert_eq!(
            progress_line("f", 512, Some(1024)),
            "f: 512 bytes of 1.0 KB (50%)"
        );
        assert_eq!(progress_line("f", 512, None), "f: 512 bytes");
    }
}
