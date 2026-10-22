//! The search index's answers, for `search`: /System/Kernel/fs/search/query gives one hit a
//! line, `score<TAB>kind<TAB>size<TAB>mtime<TAB>matched<TAB>path` (`#` lines are comments).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub score: i64,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    pub matched: String,
    pub path: String,
}

impl Hit {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }
}

pub fn hits(text: &str) -> Vec<Hit> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(6, '\t').collect();
            (f.len() == 6).then(|| Hit {
                score: f[0].parse().unwrap_or(0),
                kind: String::from(f[1]),
                size: f[2].parse().unwrap_or(0),
                mtime: f[3].parse().unwrap_or(0),
                matched: String::from(f[4]),
                path: String::from(f[5]),
            })
        })
        .collect()
}

/// Unix seconds -> "YYYY-MM-DD HH:MM" (UTC).
pub fn date(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        secs / 60 % 60
    )
}

/// A `--long` line: kind, size, date, where it matched, path.
pub fn long_line(h: &Hit) -> String {
    format!(
        "{:<7} {:>8}  {}  {:<12} {}",
        h.kind,
        filekit::words::size(h.size.max(0) as u64),
        date(h.mtime),
        h.matched,
        h.path
    )
}

/// Is a status (`state: ready`, `index: N files`) ready, and is there an index to ask?
pub fn status_ready(status: &str) -> (bool, bool) {
    let ready = status
        .lines()
        .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["state:", "ready"]);
    let some = status.lines().any(|l| {
        l.strip_prefix("index:")
            .and_then(|r| r.split_whitespace().next())
            .is_some_and(|n| n != "0")
    });
    (ready, some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        let h = hits("# 2 hits\n90\ttext\t1536\t0\tname\t/Users/milk/notes.md\nbad line\n");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].name(), "notes.md");
        assert_eq!(h[0].score, 90);
        assert_eq!(date(0), "1970-01-01 00:00");
        assert_eq!(date(1_792_589_641), "2026-10-21 13:34");
        assert_eq!(
            long_line(&h[0]),
            "text      1.5 KB  1970-01-01 00:00  name         /Users/milk/notes.md"
        );
        assert_eq!(
            status_ready("state: ready\nindex: 10 files\n"),
            (true, true)
        );
        assert_eq!(
            status_ready("state: verifying\nindex: 10 files\n"),
            (false, true)
        );
        assert_eq!(
            status_ready("state: building\nindex: 0 files\n"),
            (false, false)
        );
    }
}
