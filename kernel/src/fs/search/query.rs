//! Queries over the search index, and `/System/Kernel/fs/search`. See docs/filesystem.md.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use super::{Entry, INDEX, REBUILD, ROOTS, is_text_kind, parent, roots, status_text, tokens};
use crate::fs::errno::{Errno, Result};
use crate::fs::synth::SynthDir;
use crate::sync::SpinLock;

// ---- queries --------------------------------------------------------------------------

/// One result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    /// "md", "txt", "folder", "png", "text", "data", ...
    pub kind: String,
    pub size: u64,
    /// Unix seconds.
    pub mtime: i64,
    pub score: u32,
    /// Where the words matched: "name", "content", "name+content" ("" for filter-only).
    pub matched: &'static str,
}

/// A parsed query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<String>,
    pub kinds: Vec<String>,
    pub within: Vec<String>,
    /// Only entries modified at or after this (unix s).
    pub since: Option<i64>,
}

const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "ppm"];
const AUDIO: &[&str] = &["wav", "mp3", "ogg", "flac"];

fn kind_matches(want: &str, k: &str) -> bool {
    match want {
        "text" => is_text_kind(k),
        "folder" | "dir" | "directory" => k == "folder",
        "image" => IMAGE.contains(&k),
        "audio" | "music" | "sound" => AUDIO.contains(&k),
        "markdown" => k == "md",
        _ => k == want,
    }
}

pub fn parse(q: &str, now: i64) -> Result<Query> {
    let mut out = Query::default();
    let today = now - now.rem_euclid(86_400);
    for w in q.split_whitespace() {
        match w.split_once(':') {
            Some(("kind", k)) if !k.is_empty() => out.kinds.push(k.to_lowercase()),
            Some(("in", d)) if !d.is_empty() => out.within.push(d.to_lowercase()),
            Some(("modified", m)) => {
                let since = match m {
                    "today" => today,
                    "yesterday" => today - 86_400,
                    "week" => now - 7 * 86_400,
                    "month" => now - 30 * 86_400,
                    _ => {
                        let (n, unit) = m.split_at(m.len().saturating_sub(1));
                        let n: i64 = n.parse().map_err(|_| Errno::EINVAL)?;
                        match unit {
                            "d" => now - n * 86_400,
                            "h" => now - n * 3600,
                            "m" => now - n * 60,
                            _ => return Err(Errno::EINVAL),
                        }
                    }
                };
                out.since = Some(out.since.map_or(since, |s: i64| s.max(since)));
            }
            _ => out.terms.extend(tokens(w, 1)),
        }
    }
    Ok(out)
}

fn prefix_ids(
    map: &BTreeMap<String, Vec<u32>>,
    t: &str,
    out: &mut BTreeMap<u32, (bool, bool)>,
    exact_flag: bool,
) {
    for (k, ids) in map.range(String::from(t)..) {
        if !k.starts_with(t) {
            break;
        }
        let exact = k == t;
        for &id in ids {
            let e = out.entry(id).or_insert((false, false));
            if exact_flag {
                e.0 |= exact;
            } else {
                e.1 |= exact;
            }
        }
    }
}

/// Search the index. Every word must match the name or the content (as a word or the
/// start of one); filters narrow it down. Ranked: name matches above content matches,
/// then newer first. `limit` 0 = 50.
pub fn query(q: &str, limit: usize) -> Result<Vec<Hit>> {
    let now = crate::fs::clock::now().sec;
    let pq = parse(q, now)?;
    let limit = if limit == 0 { 50 } else { limit.min(10_000) };
    let g = INDEX.lock();
    let ix = g.as_ref().ok_or(Errno::EAGAIN)?; // not built yet
    let full = q
        .split_whitespace()
        .filter(|w| !w.contains(':'))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    // Candidates and per-term scores.
    let mut scores: BTreeMap<u32, (u32, bool, bool)> = BTreeMap::new();
    for (n, t) in pq.terms.iter().enumerate() {
        let mut name_hits: BTreeMap<u32, (bool, bool)> = BTreeMap::new();
        prefix_ids(&ix.names, t, &mut name_hits, true);
        let mut content_hits: BTreeMap<u32, (bool, bool)> = BTreeMap::new();
        prefix_ids(&ix.words, t, &mut content_hits, false);
        let ids: BTreeSet<u32> = name_hits
            .keys()
            .chain(content_hits.keys())
            .copied()
            .collect();
        let mut next = BTreeMap::new();
        for id in ids {
            if n > 0 && !scores.contains_key(&id) {
                continue;
            }
            let (mut s, mut by_name, mut by_content) =
                scores.get(&id).copied().unwrap_or((0, false, false));
            if let Some(&(exact, _)) = name_hits.get(&id) {
                s += if exact { 100 } else { 60 };
                by_name = true;
            }
            if let Some(&(_, exact)) = content_hits.get(&id) {
                s += if exact { 20 } else { 10 };
                by_content = true;
            }
            next.insert(id, (s, by_name, by_content));
        }
        scores = next;
    }
    let filter = |e: &Entry| {
        (pq.kinds.is_empty() || pq.kinds.iter().any(|k| kind_matches(k, &e.kind)))
            && pq.since.is_none_or(|s| e.mtime >= s)
            && pq.within.iter().all(|w| {
                let p = e.path.to_lowercase();
                if w.starts_with('/') {
                    p.starts_with(w.as_str())
                } else {
                    // A folder of that name above the entry.
                    parent(&p).split('/').any(|c| c == w)
                }
            })
    };
    let mut hits: Vec<Hit> = Vec::new();
    let mut push = |e: &Entry, s: u32, how: &'static str| {
        let mut s = s;
        if !full.is_empty() && e.name == full {
            s += 50;
        }
        let age = now - e.mtime;
        s += if age < 86_400 {
            8
        } else if age < 7 * 86_400 {
            4
        } else if age < 30 * 86_400 {
            2
        } else {
            0
        };
        hits.push(Hit {
            path: e.path.clone(),
            kind: e.kind.clone(),
            size: e.size,
            mtime: e.mtime,
            score: s,
            matched: how,
        });
    };
    if pq.terms.is_empty() {
        if pq.kinds.is_empty() && pq.within.is_empty() && pq.since.is_none() {
            return Ok(Vec::new());
        }
        for e in ix.entries.iter().flatten().filter(|e| filter(e)) {
            push(e, 0, "");
        }
    } else {
        for (id, (s, n, c)) in scores {
            if let Some(e) = ix.entries[id as usize].as_ref().filter(|e| filter(e)) {
                let how = match (n, c) {
                    (true, true) => "name+content",
                    (true, false) => "name",
                    _ => "content",
                };
                push(e, s, how);
            }
        }
    }
    drop(g);
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.mtime.cmp(&a.mtime))
            .then(a.path.cmp(&b.path))
    });
    hits.truncate(limit);
    Ok(hits)
}

// ---- /System/Kernel/fs/search -------------------------------------------------------------------

static LAST: SpinLock<String> = SpinLock::new(String::new());

/// Results as text: a `#` header line, then `score<TAB>kind<TAB>size<TAB>mtime<TAB>
/// matched<TAB>path` per hit.
pub fn results_text(q: &str, limit: usize) -> Result<String> {
    let t0 = crate::timer::now_ns();
    let hits = query(q, limit)?;
    let us = (crate::timer::now_ns() - t0) / 1000;
    let mut s = format!("# {} result(s) for {q:?} in {us} us\n", hits.len());
    for h in hits {
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{}\t{}",
            h.score,
            h.kind,
            h.size,
            h.mtime,
            if h.matched.is_empty() { "-" } else { h.matched },
            h.path
        );
    }
    Ok(s)
}

/// `/System/Kernel/fs/search`.
pub fn sysfs(dir: &alloc::sync::Arc<SynthDir>) {
    dir.add_control(
        "query",
        || LAST.lock().clone(),
        |b| {
            // `[limit=N] words and filters`
            let s = core::str::from_utf8(b).map_err(|_| Errno::EINVAL)?.trim();
            let (limit, q) = match s.strip_prefix("limit=") {
                Some(rest) => {
                    let (n, q) = rest.split_once(' ').unwrap_or((rest, ""));
                    (n.parse().map_err(|_| Errno::EINVAL)?, q.trim())
                }
                None => (0, s),
            };
            *LAST.lock() = results_text(q, limit)?;
            Ok(())
        },
    );
    dir.add_file("status", status_text);
    dir.add_control(
        "roots",
        || roots().join("\n") + "\n",
        |b| {
            let s = core::str::from_utf8(b).map_err(|_| Errno::EINVAL)?;
            let r: Vec<String> = s
                .split_whitespace()
                .map(|p| String::from(p.trim_end_matches('/')))
                .filter(|p| p.starts_with('/'))
                .collect();
            if r.is_empty() {
                return Err(Errno::EINVAL);
            }
            *ROOTS.lock() = r;
            REBUILD.store(true, Ordering::Release);
            Ok(())
        },
    );
    dir.add_control("rebuild", String::new, |_| {
        REBUILD.store(true, Ordering::Release);
        Ok(())
    });
}
