//! Spotlight-style search: an index of the files under a few roots (default `/Users/milk`
//! and `/Apps`): names, paths, kinds, sizes, modification times, and the words of
//! text files, kept up to date from VFS change notifications (`fs::notify`) by a
//! low-priority thread, stored at `~/.cache/milk/index` so it survives reboots.
//!
//! - [`query()`]`(q, limit)` (`query.rs`): words and prefixes, ranked (name > content, newer first),
//!   with filters `kind:md`, `in:Documents`, `modified:today`. See docs/filesystem.md.
//! - `/System/Kernel/fs/search`: `query` (write a query, read the results), `status`, `roots`,
//!   `rebuild`. Tool: `spot`.
//!
//! The indexer starts from the saved index (if its format and roots match), then walks the
//! roots once comparing size and mtime, re-reading only what changed (a "verify" scan);
//! without a saved index it builds one from scratch the same way. After that it follows
//! events: a create, rename or delete is reflected within ~0.2 s, a written file once it
//! is closed. Hidden names (starting with '.') are skipped, so the index file itself and
//! `~/.cache` never show up. Text files are read up to [`TEXT_CAP`]; reading is rate
//! limited ([`RATE_MB`]) and the thread is low priority, like the scrub.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::fs::errno::Result;

pub mod query;
use crate::fs::node::InodeType;
use crate::fs::notify::{self, Event, Id};
use crate::sched::Mutex;
use crate::sync::SpinLock;
#[allow(unused_imports)]
pub use query::{Hit, Query, parse, query, results_text, sysfs};

/// Where the index is kept.
pub const INDEX_PATH: &str = "/Users/milk/.cache/milk/index";
/// Default roots.
pub const DEFAULT_ROOTS: &[&str] = &["/Users/milk", "/Apps"];
/// Text files are read (for their words) up to this size.
pub const TEXT_CAP: u64 = 512 << 10;
/// Most distinct words kept per file.
const WORDS_PER_FILE: usize = 4000;
/// Content reading rate limit, MB/s.
pub const RATE_MB: u64 = 16;
const FORMAT: u32 = 1;
const MAGIC: &[u8; 4] = b"MKSX";

/// Extensions read for their words, and the kind they get.
const TEXT_EXT: &[&str] = &[
    "md", "txt", "rs", "toml", "html", "htm", "css", "js", "json", "py", "sh", "c", "h", "cpp",
    "hpp", "yaml", "yml", "csv", "log", "conf", "ini", "xml", "svg", "tex", "rst", "lua", "go",
    "java", "swift", "kt", "rb", "pl", "ts", "list", "cfg",
];
/// Other extensions we recognise (not read).
const OTHER_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ppm", "wav", "mp3", "ogg", "flac", "pdf", "zip", "gz",
    "tar", "img", "iso", "bin", "milkpkg", "stream", "app",
];

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub(super) path: String,
    /// Lowercased last component.
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) size: u64,
    pub(super) mtime: i64,
    pub(super) obj: Id,
    pub(super) dir: bool,
    pub(super) words: Vec<String>,
}

#[derive(Default)]
pub(super) struct Index {
    pub(super) entries: Vec<Option<Entry>>,
    pub(super) free: Vec<u32>,
    pub(super) by_path: BTreeMap<String, u32>,
    pub(super) by_obj: BTreeMap<Id, Vec<u32>>,
    pub(super) names: BTreeMap<String, Vec<u32>>,
    pub(super) words: BTreeMap<String, Vec<u32>>,
    /// Root path -> its directory's id.
    pub(super) roots: Vec<(String, Id)>,
}

fn add_posting(map: &mut BTreeMap<String, Vec<u32>>, k: &str, id: u32) {
    let v = map.entry(String::from(k)).or_default();
    if let Err(i) = v.binary_search(&id) {
        v.insert(i, id);
    }
}

fn del_posting(map: &mut BTreeMap<String, Vec<u32>>, k: &str, id: u32) {
    if let Some(v) = map.get_mut(k) {
        if let Ok(i) = v.binary_search(&id) {
            v.remove(i);
        }
        if v.is_empty() {
            map.remove(k);
        }
    }
}

/// Lowercase alphanumeric runs (the unit of both name and content matching).
pub(super) fn tokens(s: &str, min: usize) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(move |t| t.chars().count() >= min && t.len() <= 64)
        .map(|t| t.to_lowercase())
}

fn name_tokens(name: &str) -> Vec<String> {
    let mut v: Vec<String> = tokens(name, 1).collect();
    v.push(name.to_lowercase());
    v.sort();
    v.dedup();
    v
}

fn last(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "",
    }
}

fn hidden(path: &str) -> bool {
    path.split('/').any(|c| c.starts_with('.'))
}

impl Index {
    fn insert(&mut self, e: Entry) -> u32 {
        if let Some(&old) = self.by_path.get(&e.path) {
            self.remove_id(old);
        }
        let id = match self.free.pop() {
            Some(i) => i,
            None => {
                self.entries.push(None);
                (self.entries.len() - 1) as u32
            }
        };
        for t in name_tokens(last(&e.path)) {
            add_posting(&mut self.names, &t, id);
        }
        for w in &e.words {
            add_posting(&mut self.words, w, id);
        }
        self.by_path.insert(e.path.clone(), id);
        let v = self.by_obj.entry(e.obj).or_default();
        if !v.contains(&id) {
            v.push(id);
        }
        self.entries[id as usize] = Some(e);
        id
    }

    fn remove_id(&mut self, id: u32) -> Option<Entry> {
        let e = self.entries.get_mut(id as usize)?.take()?;
        for t in name_tokens(last(&e.path)) {
            del_posting(&mut self.names, &t, id);
        }
        for w in &e.words {
            del_posting(&mut self.words, w, id);
        }
        self.by_path.remove(&e.path);
        if let Some(v) = self.by_obj.get_mut(&e.obj) {
            v.retain(|&x| x != id);
            if v.is_empty() {
                self.by_obj.remove(&e.obj);
            }
        }
        self.free.push(id);
        Some(e)
    }

    /// Ids of `path` and everything under it.
    fn under(&self, path: &str) -> Vec<u32> {
        let pre = format!("{path}/");
        self.by_path
            .range(String::from(path)..)
            .take_while(|(p, _)| *p == path || p.starts_with(&pre))
            .map(|(_, &id)| id)
            .collect()
    }

    fn remove_tree(&mut self, path: &str) -> usize {
        let ids = self.under(path);
        for &id in &ids {
            self.remove_id(id);
        }
        ids.len()
    }

    /// `old` (and its subtree) is now `new`.
    fn rename_tree(&mut self, old: &str, new: &str) {
        self.remove_tree(new);
        for id in self.under(old) {
            if let Some(mut e) = self.remove_id(id) {
                e.path = format!("{new}{}", &e.path[old.len()..]);
                e.name = last(&e.path).to_lowercase();
                self.insert(e);
            }
        }
    }

    /// Path of a directory we know (a root or an indexed folder).
    fn dir_path(&self, d: Id) -> Option<String> {
        if let Some((p, _)) = self.roots.iter().find(|(_, id)| *id == d) {
            return Some(p.clone());
        }
        self.by_obj.get(&d)?.iter().find_map(|&i| {
            self.entries[i as usize]
                .as_ref()
                .filter(|e| e.dir)
                .map(|e| e.path.clone())
        })
    }

    fn in_roots(&self, path: &str) -> bool {
        self.roots.iter().any(|(r, _)| {
            path.len() > r.len() && path.starts_with(r.as_str()) && path.as_bytes()[r.len()] == b'/'
        })
    }

    fn counts(&self) -> (usize, usize) {
        let (mut f, mut d) = (0, 0);
        for e in self.entries.iter().flatten() {
            if e.dir {
                d += 1;
            } else {
                f += 1;
            }
        }
        (f, d)
    }
}

// ---- reading files ------------------------------------------------------------------

/// Token bucket over content bytes read (shared by the indexer).
struct Limiter {
    start: u64,
    bytes: u64,
}

impl Limiter {
    fn account(&mut self, n: u64) {
        if !THREAD_UP.load(Ordering::Relaxed) {
            return; // before the scheduler (self-test): no sleeping
        }
        self.bytes += n;
        let due = self.start + self.bytes * 1_000_000_000 / (RATE_MB << 20);
        let now = crate::timer::now_ns();
        if due > now {
            crate::sched::sleep_ns(due - now);
        } else if now - due > 1_000_000_000 {
            // Don't bank more than a second of budget.
            self.start = now;
            self.bytes = 0;
        }
    }
}

fn ext_of(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 8).then(|| ext.to_lowercase())
}

pub(super) fn is_text_kind(k: &str) -> bool {
    k == "text" || TEXT_EXT.contains(&k)
}

/// UTF-8 without control characters (other than line breaks and tabs) in the first 512
/// bytes; a character cut off at the end of the sample is fine.
fn looks_text(b: &[u8]) -> bool {
    let n = b.len().min(512);
    let s = match core::str::from_utf8(&b[..n]) {
        Ok(s) => s,
        Err(e) if e.error_len().is_none() => match core::str::from_utf8(&b[..e.valid_up_to()]) {
            Ok(s) => s,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    s.chars()
        .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t' | '\x0c'))
}

/// Kind from the first bytes (files without a known extension).
fn sniff(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG") {
        "png"
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        "wav"
    } else if b.starts_with(b"\x7fELF") {
        "app"
    } else if b.starts_with(b"%PDF") {
        "pdf"
    } else if b.starts_with(b"MILKSTRM") {
        "stream"
    } else if looks_text(b) {
        "text"
    } else {
        "data"
    }
}

/// Look at `path` (lstat) and build its entry, reading text for words.
fn make_entry(path: &str, lim: &mut Limiter) -> Result<Entry> {
    let st = crate::fs::ops::lstat(path)?;
    let name = last(path);
    let mut e = Entry {
        path: String::from(path),
        name: name.to_lowercase(),
        kind: String::new(),
        size: st.size,
        mtime: st.mtime.sec,
        obj: (st.dev, st.ino),
        dir: st.kind == InodeType::Dir,
        words: Vec::new(),
    };
    e.kind = match st.kind {
        InodeType::Dir => String::from("folder"),
        InodeType::Symlink => String::from("link"),
        InodeType::File => match ext_of(name) {
            Some(x) if TEXT_EXT.contains(&x.as_str()) || OTHER_EXT.contains(&x.as_str()) => x,
            _ => String::new(),
        },
        _ => String::from("device"),
    };
    if st.kind != InodeType::File {
        return Ok(e);
    }
    let needs_sniff = e.kind.is_empty();
    let read_all = st.size <= TEXT_CAP && (needs_sniff || is_text_kind(&e.kind));
    if !needs_sniff && !read_all {
        return Ok(e);
    }
    let f = crate::fs::ops::open(path, crate::fs::abi::O_RDONLY, 0)?;
    let n = if read_all {
        st.size as usize
    } else {
        512.min(st.size as usize)
    };
    let mut buf = alloc::vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.pread(&mut buf[got..], got as u64) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    buf.truncate(got);
    lim.account(got as u64);
    if needs_sniff {
        e.kind = String::from(sniff(&buf));
    }
    if read_all && is_text_kind(&e.kind) {
        let text = String::from_utf8_lossy(&buf);
        let mut seen = BTreeSet::new();
        for w in tokens(&text, 2) {
            if seen.len() >= WORDS_PER_FILE {
                break;
            }
            seen.insert(w);
        }
        e.words = seen.into_iter().collect();
    }
    Ok(e)
}

// ---- the index and its thread ---------------------------------------------------------

pub(super) static INDEX: Mutex<Option<Index>> = Mutex::new(None);
static THREAD_UP: AtomicBool = AtomicBool::new(false);
pub(super) static REBUILD: AtomicBool = AtomicBool::new(false);
/// An event couldn't be applied as is (the tree moved on before we got to it): verify
/// once the queue is empty.
static STALE: AtomicBool = AtomicBool::new(false);
/// The index changed since it was last saved.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// One save at a time (the indexer's own, and `flush` from a sync).
static SAVING: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// "starting", "building", "verifying", "ready", "off".
    pub state: &'static str,
    pub files: usize,
    pub folders: usize,
    pub words: usize,
    pub roots: Vec<String>,
    /// How the index came to be this boot, e.g. "loaded 5003 entries, verified in 210 ms".
    pub origin: String,
    pub events: u64,
    pub saves: u64,
    pub last_save_ns: u64,
}

static STATUS: SpinLock<Status> = SpinLock::new(Status {
    state: "starting",
    files: 0,
    folders: 0,
    words: 0,
    roots: Vec::new(),
    origin: String::new(),
    events: 0,
    saves: 0,
    last_save_ns: 0,
});
pub(super) static ROOTS: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

pub fn status() -> Status {
    let mut s = STATUS.lock().clone();
    if let Some(ix) = INDEX.lock().as_ref() {
        let (f, d) = ix.counts();
        s.files = f;
        s.folders = d;
        s.words = ix.words.len();
        s.roots = ix.roots.iter().map(|(p, _)| p.clone()).collect();
    }
    s
}

fn set_state(s: &'static str) {
    STATUS.lock().state = s;
}

pub(super) fn roots() -> Vec<String> {
    let r = ROOTS.lock().clone();
    if r.is_empty() {
        DEFAULT_ROOTS.iter().map(|s| String::from(*s)).collect()
    } else {
        r
    }
}

/// Walk `dir` (an indexed path), adding or refreshing everything under it. `keep`:
/// entries from a saved index (by path) whose size, mtime and kind still match are
/// reused without reading the file again. Returns the paths seen.
fn walk(
    dir: &str,
    keep: &BTreeMap<String, Entry>,
    lim: &mut Limiter,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<Entry>,
) {
    let Ok(list) = crate::fs::ops::readdir(dir) else {
        return;
    };
    for d in list {
        if d.name.starts_with('.') {
            continue;
        }
        let p = if dir == "/" {
            format!("/{}", d.name)
        } else {
            format!("{dir}/{}", d.name)
        };
        let Ok(st) = crate::fs::ops::lstat(&p) else {
            continue;
        };
        let reuse = keep.get(&p).filter(|e| {
            e.size == st.size && e.mtime == st.mtime.sec && e.dir == (st.kind == InodeType::Dir)
        });
        let e = match reuse {
            Some(e) => Entry {
                obj: (st.dev, st.ino),
                ..e.clone()
            },
            None => match make_entry(&p, lim) {
                Ok(e) => e,
                Err(_) => continue,
            },
        };
        let is_dir = e.dir;
        seen.insert(p.clone());
        out.push(e);
        if is_dir {
            walk(&p, keep, lim, seen, out);
        }
    }
}

/// (Re)build the whole index from the roots, reusing `keep` where nothing changed.
fn scan(keep: BTreeMap<String, Entry>, lim: &mut Limiter) -> (Index, usize) {
    let mut ix = Index::default();
    let mut reused = 0;
    for r in roots() {
        let Ok(st) = crate::fs::ops::stat(&r) else {
            continue;
        };
        ix.roots.push((r.clone(), (st.dev, st.ino)));
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        walk(&r, &keep, lim, &mut seen, &mut out);
        for e in out {
            if keep
                .get(&e.path)
                .is_some_and(|k| k.mtime == e.mtime && k.size == e.size)
            {
                reused += 1;
            }
            ix.insert(e);
        }
    }
    (ix, reused)
}

/// Index (or re-index) one path and, for a folder, everything under it. `expect`: the
/// object the event was about; if `path` doesn't name it (any more), the index is marked
/// stale instead of guessing.
fn refresh_path(path: &str, expect: Option<Id>, lim: &mut Limiter) {
    let in_roots = INDEX.lock().as_ref().is_some_and(|ix| ix.in_roots(path));
    if !in_roots || hidden(path) {
        return;
    }
    let mut new = Vec::new();
    match make_entry(path, lim) {
        Ok(e) if expect.is_some_and(|x| x != e.obj) => {
            STALE.store(true, Ordering::Release);
        }
        Ok(e) => {
            let d = e.dir;
            new.push(e);
            if d {
                walk(path, &BTreeMap::new(), lim, &mut BTreeSet::new(), &mut new);
            }
        }
        Err(_) => {
            if let Some(ix) = INDEX.lock().as_mut() {
                ix.remove_tree(path);
            }
            if expect.is_some() {
                STALE.store(true, Ordering::Release);
            }
            return;
        }
    }
    if let Some(ix) = INDEX.lock().as_mut() {
        for e in new {
            ix.insert(e);
        }
    }
}

fn handle(e: Event, lim: &mut Limiter) -> bool {
    STATUS.lock().events += 1;
    let dir_path = |d: Id| INDEX.lock().as_ref().and_then(|ix| ix.dir_path(d));
    let join = |d: &str, n: &str| {
        if d == "/" {
            format!("/{n}")
        } else {
            format!("{d}/{n}")
        }
    };
    match e {
        Event::Created { dir, name, obj, .. } => {
            if name.starts_with('.') {
                return false;
            }
            let Some(d) = dir_path(dir) else { return false };
            refresh_path(&join(&d, &name), Some(obj), lim);
        }
        Event::Removed { dir, name, .. } => {
            let Some(d) = dir_path(dir) else { return false };
            if let Some(ix) = INDEX.lock().as_mut() {
                ix.remove_tree(&join(&d, &name));
            }
        }
        Event::Renamed {
            from_dir,
            from,
            to_dir,
            to,
            obj,
        } => {
            let old = dir_path(from_dir).map(|d| join(&d, &from));
            let new = dir_path(to_dir).map(|d| join(&d, &to));
            // Moved within the index: rename (no file is read again). Otherwise it left
            // the index, or came in from outside (read it), or both.
            let moved = {
                let mut g = INDEX.lock();
                let Some(ix) = g.as_mut() else { return false };
                match (&old, &new) {
                    (Some(o), Some(n)) if !hidden(n) && ix.by_path.contains_key(o) => {
                        let file = ix
                            .by_path
                            .get(o)
                            .and_then(|&i| ix.entries[i as usize].as_ref())
                            .is_some_and(|e| !e.dir);
                        ix.rename_tree(o, n);
                        // A folder's contents just move; a file may have a new extension
                        // (kind): look at it again.
                        !file
                    }
                    _ => {
                        if let Some(o) = &old {
                            ix.remove_tree(o);
                        }
                        if let Some(n) = &new {
                            ix.remove_tree(n);
                        }
                        false
                    }
                }
            };
            if !moved && let Some(n) = new {
                refresh_path(&n, Some(obj), lim);
            }
        }
        Event::Modified { obj } | Event::Attr { obj } => {
            let paths: Vec<String> = {
                let g = INDEX.lock();
                let Some(ix) = g.as_ref() else { return false };
                ix.by_obj
                    .get(&obj)
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|&i| ix.entries[i as usize].as_ref())
                            .filter(|e| !e.dir)
                            .map(|e| e.path.clone())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            if paths.is_empty() {
                return false;
            }
            for p in paths {
                refresh_path(&p, Some(obj), lim);
            }
        }
        Event::Rescan { .. } | Event::Overflow => {
            verify(lim, "rescan");
        }
    }
    true
}

/// Walk the roots again against the current index (cheap: only changed files are read).
fn verify(lim: &mut Limiter, why: &str) {
    set_state("verifying");
    let t0 = crate::timer::now_ns();
    let keep: BTreeMap<String, Entry> = INDEX
        .lock()
        .take()
        .map(|ix| {
            ix.entries
                .into_iter()
                .flatten()
                .map(|e| (e.path.clone(), e))
                .collect()
        })
        .unwrap_or_default();
    let n = keep.len();
    let (ix, reused) = scan(keep, lim);
    let total = ix.by_path.len();
    *INDEX.lock() = Some(ix);
    let ms = (crate::timer::now_ns() - t0) / 1_000_000;
    STATUS.lock().origin =
        format!("{why}: {total} entries ({reused} unchanged of {n} known) in {ms} ms");
    set_state("ready");
}

// ---- persistence ----------------------------------------------------------------------

fn put_str(b: &mut Vec<u8>, s: &str) {
    let s = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s);
}

fn encode(ix: &Index) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&FORMAT.to_le_bytes());
    b.extend_from_slice(&(ix.roots.len() as u32).to_le_bytes());
    for (r, _) in &ix.roots {
        put_str(&mut b, r);
    }
    let es: Vec<&Entry> = ix.entries.iter().flatten().collect();
    b.extend_from_slice(&(es.len() as u32).to_le_bytes());
    for e in es {
        put_str(&mut b, &e.path);
        put_str(&mut b, &e.kind);
        b.extend_from_slice(&e.size.to_le_bytes());
        b.extend_from_slice(&e.mtime.to_le_bytes());
        b.push(e.dir as u8);
        b.extend_from_slice(&(e.words.len() as u32).to_le_bytes());
        for w in &e.words {
            put_str(&mut b, w);
        }
    }
    let c = crate::fs::milkfs::format::xxh64(&b);
    b.extend_from_slice(&c.to_le_bytes());
    b
}

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// Saved entries by path, and the roots they were made for. None: missing or damaged.
fn decode(b: &[u8]) -> Option<(Vec<String>, BTreeMap<String, Entry>)> {
    if b.len() < 16 || &b[..4] != MAGIC {
        return None;
    }
    let (body, c) = b.split_at(b.len() - 8);
    if crate::fs::milkfs::format::xxh64(body) != u64::from_le_bytes(c.try_into().ok()?) {
        return None;
    }
    let mut r = Rd(&body[4..]);
    if r.u32()? != FORMAT {
        return None;
    }
    let roots = (0..r.u32()?).map(|_| r.str()).collect::<Option<Vec<_>>>()?;
    let n = r.u32()?;
    let mut out = BTreeMap::new();
    for _ in 0..n {
        let path = r.str()?;
        let kind = r.str()?;
        let size = r.u64()?;
        let mtime = r.u64()? as i64;
        let dir = r.take(1)?[0] != 0;
        let nw = r.u32()? as usize;
        let words = (0..nw).map(|_| r.str()).collect::<Option<Vec<_>>>()?;
        out.insert(
            path.clone(),
            Entry {
                name: last(&path).to_lowercase(),
                path,
                kind,
                size,
                mtime,
                obj: (0, 0),
                dir,
                words,
            },
        );
    }
    Some((roots, out))
}

/// Save the index now if it changed since the last save (`sync(2)`, the milkfs `sync`
/// control, shutting down): so a machine turned off right after a change doesn't come
/// back to an older saved index. Nothing to do while it is being built or verified (the
/// saved one is still the newest complete one).
pub fn flush() -> Result<()> {
    if DIRTY.load(Ordering::Acquire) && status().state == "ready" {
        save()?;
    }
    Ok(())
}

fn save() -> Result<()> {
    let _one = SAVING.lock();
    // Cleared before encoding: a change made meanwhile sets it again.
    DIRTY.store(false, Ordering::Release);
    let data = match INDEX.lock().as_ref() {
        Some(ix) => encode(ix),
        None => return Ok(()),
    };
    let dir = parent(INDEX_PATH);
    crate::fs::ops::mkdir_all(dir, 0o700)?;
    let tmp = format!("{INDEX_PATH}.tmp");
    let r = crate::fs::ops::write_file(&tmp, &data)
        .and_then(|_| crate::fs::ops::rename(&tmp, INDEX_PATH));
    if r.is_err() {
        DIRTY.store(true, Ordering::Release);
    }
    r?;
    let mut s = STATUS.lock();
    s.saves += 1;
    s.last_save_ns = crate::timer::now_ns();
    Ok(())
}

fn load() -> Option<BTreeMap<String, Entry>> {
    let data = crate::fs::ops::read_file(INDEX_PATH).ok()?;
    let (r, entries) = decode(&data)?;
    (r == roots()).then_some(entries)
}

// ---- thread -------------------------------------------------------------------------

/// Start the indexer (after the scheduler runs and home is mounted).
pub fn start_thread() {
    drop(crate::sched::spawn_prio(
        "search-index",
        crate::sched::Priority::Low,
        thread,
    ));
}

fn thread() {
    THREAD_UP.store(true, Ordering::Relaxed);
    // Watch first, so nothing that happens during the first scan is missed.
    let w = notify::watch(8192);
    crate::sched::sleep_ms(500);
    let mut lim = Limiter {
        start: crate::timer::now_ns(),
        bytes: 0,
    };
    let t0 = crate::timer::now_ns();
    match load() {
        Some(saved) => {
            let n = saved.len();
            // Queries work on the saved index at once, while it is checked against the
            // disk (only files whose size or time changed are read again).
            let mut quick = Index::default();
            for r in roots() {
                if let Ok(st) = crate::fs::ops::stat(&r) {
                    quick.roots.push((r, (st.dev, st.ino)));
                }
            }
            for e in saved.values() {
                quick.insert(e.clone());
            }
            *INDEX.lock() = Some(quick);
            STATUS.lock().origin = format!("loaded {n} entries from {INDEX_PATH}, verifying");
            set_state("verifying");
            let t1 = crate::timer::now_ns();
            let (ix, reused) = scan(saved, &mut lim);
            let total = ix.by_path.len();
            *INDEX.lock() = Some(ix);
            let t2 = crate::timer::now_ns();
            STATUS.lock().origin = format!(
                "loaded {n} entries from {INDEX_PATH} in {} ms, verified in {} ms ({reused} unchanged, {total} now)",
                (t1 - t0) / 1_000_000,
                (t2 - t1) / 1_000_000
            );
        }
        None => {
            set_state("building");
            let (ix, _) = scan(BTreeMap::new(), &mut lim);
            let total = ix.by_path.len();
            *INDEX.lock() = Some(ix);
            STATUS.lock().origin = format!(
                "built from scratch: {total} entries in {} ms",
                (crate::timer::now_ns() - t0) / 1_000_000
            );
        }
    }
    set_state("ready");
    let _ = save();
    let mut dirty_since: Option<u64> = None;
    loop {
        if REBUILD.swap(false, Ordering::AcqRel) {
            *INDEX.lock() = None;
            let t = crate::timer::now_ns();
            set_state("building");
            let (ix, _) = scan(BTreeMap::new(), &mut lim);
            let total = ix.by_path.len();
            *INDEX.lock() = Some(ix);
            STATUS.lock().origin = format!(
                "rebuilt: {total} entries in {} ms",
                (crate::timer::now_ns() - t) / 1_000_000
            );
            set_state("ready");
            dirty_since.get_or_insert(crate::timer::now_ns());
            DIRTY.store(true, Ordering::Release);
        }
        if let Some(e) = w.next(200_000_000)
            && handle(e, &mut lim)
        {
            dirty_since.get_or_insert(crate::timer::now_ns());
            DIRTY.store(true, Ordering::Release);
        }
        if w.pending() == 0 && STALE.swap(false, Ordering::AcqRel) {
            verify(&mut lim, "caught up with a moved tree");
            dirty_since.get_or_insert(crate::timer::now_ns());
            DIRTY.store(true, Ordering::Release);
        }
        // Save once things have been quiet for 2 s (or every 30 s while busy).
        if let Some(t) = dirty_since {
            let now = crate::timer::now_ns();
            let quiet = w.pending() == 0;
            if (quiet && now - t >= 2_000_000_000) || now - t >= 30_000_000_000 {
                if DIRTY.load(Ordering::Acquire) {
                    let _ = save(); // (a sync may have saved it already)
                }
                dirty_since = None;
            }
        }
    }
}

pub fn status_text() -> String {
    let s = status();
    let mut out = String::new();
    let _ = writeln!(out, "state:   {}", s.state);
    let _ = writeln!(
        out,
        "index:   {} files, {} folders, {} distinct words",
        s.files, s.folders, s.words
    );
    let _ = writeln!(out, "roots:   {}", s.roots.join(" "));
    let _ = writeln!(out, "origin:  {}", s.origin);
    let _ = writeln!(
        out,
        "events:  {} handled, {} saves to {INDEX_PATH}",
        s.events, s.saves
    );
    out
}

// ---- self-test helpers ------------------------------------------------------------------

/// Build the index synchronously from `roots` (self-test, before the scheduler runs).
pub fn build_now(roots_list: &[&str]) {
    *ROOTS.lock() = roots_list.iter().map(|s| String::from(*s)).collect();
    let mut lim = Limiter { start: 0, bytes: 0 };
    let (ix, _) = scan(BTreeMap::new(), &mut lim);
    *INDEX.lock() = Some(ix);
    set_state("ready");
}

/// Apply pending events of watcher `w` synchronously (self-test).
pub fn apply_now(w: &notify::Watcher) {
    let mut lim = Limiter { start: 0, bytes: 0 };
    while let Some(e) = w.next(0) {
        handle(e, &mut lim);
    }
    if STALE.swap(false, Ordering::AcqRel) {
        verify(&mut lim, "caught up with a moved tree");
    }
}

/// Round-trip the index through its saved form (self-test): true if equal.
pub fn roundtrip_ok() -> bool {
    let g = INDEX.lock();
    let Some(ix) = g.as_ref() else { return false };
    let Some((r, back)) = decode(&encode(ix)) else {
        return false;
    };
    let mine: BTreeMap<&String, (&String, u64, i64, &Vec<String>)> = ix
        .entries
        .iter()
        .flatten()
        .map(|e| (&e.path, (&e.kind, e.size, e.mtime, &e.words)))
        .collect();
    r == ix.roots.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>()
        && back.len() == mine.len()
        && back
            .iter()
            .all(|(p, e)| mine.get(p) == Some(&(&e.kind, e.size, e.mtime, &e.words)))
}

/// Forget the index and roots (self-test cleanup).
pub fn reset() {
    *INDEX.lock() = None;
    ROOTS.lock().clear();
    STALE.store(false, Ordering::Release);
    DIRTY.store(false, Ordering::Release);
    let mut s = STATUS.lock();
    s.origin.clear();
    s.events = 0;
    s.saves = 0;
    s.last_save_ns = 0;
    s.state = "starting";
}
