//! Page cache for file reads (milkfs and FAT; not 9p, whose files the Mac can change).
//!
//! One global, bounded LRU of 4 KiB pages kept in PMM frames, keyed per inode by
//! `(dev, ino, page index)`. File systems route `read_at` through [`read`], giving a
//! `fill` callback that reads one page from the disk; the callback runs outside the cache
//! lock (it does I/O and sleeps). Writers invalidate: [`invalidate`] drops an inode's pages
//! (on write / truncate), [`invalidate_dev`] a whole file system view (rollback, promote).
//!
//! `/sys/fs/pagecache/`: `stats`, `enabled` (write 0/1), `drop` (write anything), `bench`
//! (write a path: read it twice, cold then warm, and report).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use super::errno::{Errno, Result};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::pmm;
use crate::sync::SpinLock;

pub const PAGE: usize = 4096;
const _: () = assert!(PAGE == PAGE_SIZE);
/// Pages kept at most (8 MiB).
pub const CAPACITY: usize = 2048;

type Key = (u64, u64, u64);

struct Inner {
    pages: BTreeMap<Key, (PhysAddr, u64)>,
    /// LRU order: tick -> key.
    lru: BTreeMap<u64, Key>,
    tick: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

static CACHE: SpinLock<Inner> = SpinLock::new(Inner {
    pages: BTreeMap::new(),
    lru: BTreeMap::new(),
    tick: 0,
    hits: 0,
    misses: 0,
    evictions: 0,
});
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Bumped by every invalidation: a page read from disk is only inserted if no
/// invalidation happened while it was being read (so a racing write can't leave it stale).
static EPOCH: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn page_ptr(pa: PhysAddr) -> *mut u8 {
    phys_to_virt(pa) as *mut u8
}

/// Copy `len` bytes at `at` of the cached page into `out`, refreshing its LRU slot.
fn lookup(key: Key, at: usize, out: &mut [u8]) -> bool {
    let mut c = CACHE.lock();
    let Some(&(pa, t)) = c.pages.get(&key) else {
        c.misses += 1;
        return false;
    };
    c.hits += 1;
    c.tick += 1;
    let now = c.tick;
    c.lru.remove(&t);
    c.lru.insert(now, key);
    c.pages.insert(key, (pa, now));
    // SAFETY: the frame is the cache's and PAGE long; at + len <= PAGE.
    unsafe { core::ptr::copy_nonoverlapping(page_ptr(pa).add(at), out.as_mut_ptr(), out.len()) };
    true
}

fn insert(key: Key, page: &[u8; PAGE], epoch: u64) {
    let Some(pa) = pmm::alloc_frame() else { return };
    // SAFETY: fresh frame.
    unsafe { core::ptr::copy_nonoverlapping(page.as_ptr(), page_ptr(pa), PAGE) };
    let mut freed = None;
    {
        let mut c = CACHE.lock();
        if c.pages.contains_key(&key) || EPOCH.load(Ordering::Acquire) != epoch {
            drop(c);
            pmm::free_frame(pa);
            return;
        }
        if c.pages.len() >= CAPACITY
            && let Some((&t, &old)) = c.lru.iter().next()
        {
            c.lru.remove(&t);
            freed = c.pages.remove(&old).map(|(p, _)| p);
            c.evictions += 1;
        }
        c.tick += 1;
        let now = c.tick;
        c.lru.insert(now, key);
        c.pages.insert(key, (pa, now));
    }
    if let Some(p) = freed {
        pmm::free_frame(p);
    }
}

/// Pages read ahead on a miss (64 KiB): one call into the file system fills them all.
pub const READAHEAD: u64 = 16;

/// Read `buf.len()` bytes at `off` of the file `(dev, ino)` whose size is `size`, through
/// the cache. `fill(first_page, out)` reads whole pages `first_page..` from the file system
/// into `out` (a multiple of PAGE bytes; bytes past EOF zero). On a miss up to
/// [`READAHEAD`] pages are filled and cached at once; with the cache off the whole request
/// goes to the file system in one call.
pub fn read(
    dev: u64,
    ino: u64,
    size: u64,
    off: u64,
    buf: &mut [u8],
    mut fill: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<usize> {
    if off >= size {
        return Ok(0);
    }
    let n = (buf.len() as u64).min(size - off) as usize;
    let p = PAGE as u64;
    let last_page = (size - 1) / p;
    if !ENABLED.load(Ordering::Relaxed) {
        let (first, end) = (off / p, (off + n as u64 - 1) / p + 1);
        let mut tmp = alloc::vec![0u8; ((end - first) * p) as usize];
        fill(first, &mut tmp)?;
        let at = (off - first * p) as usize;
        buf[..n].copy_from_slice(&tmp[at..at + n]);
        return Ok(n);
    }
    let mut tmp = alloc::vec::Vec::new();
    let mut done = 0;
    while done < n {
        let pos = off + done as u64;
        let (pg, at) = (pos / p, (pos % p) as usize);
        let chunk = (PAGE - at).min(n - done);
        if lookup((dev, ino, pg), at, &mut buf[done..done + chunk]) {
            done += chunk;
            continue;
        }
        let epoch = EPOCH.load(Ordering::Acquire);
        let ra = READAHEAD.min(last_page - pg + 1) as usize;
        tmp.clear();
        tmp.resize(ra * PAGE, 0);
        fill(pg, &mut tmp)?;
        for (i, page) in tmp.as_chunks::<PAGE>().0.iter().enumerate() {
            insert((dev, ino, pg + i as u64), page, epoch);
        }
        // Serve as much of the request as the filled pages cover.
        let have = (ra * PAGE - at).min(n - done);
        buf[done..done + have].copy_from_slice(&tmp[at..at + have]);
        done += have;
    }
    Ok(n)
}

fn drop_where(pred: impl Fn(&Key) -> bool) {
    EPOCH.fetch_add(1, Ordering::AcqRel);
    let mut gone = alloc::vec::Vec::new();
    {
        let mut c = CACHE.lock();
        let keys: alloc::vec::Vec<(Key, u64)> = c
            .pages
            .iter()
            .filter(|(k, _)| pred(k))
            .map(|(k, (_, t))| (*k, *t))
            .collect();
        for (k, t) in keys {
            c.lru.remove(&t);
            if let Some((pa, _)) = c.pages.remove(&k) {
                gone.push(pa);
            }
        }
    }
    for pa in gone {
        pmm::free_frame(pa);
    }
}

/// Forget the cached pages of one file.
pub fn invalidate(dev: u64, ino: u64) {
    drop_where(|k| k.0 == dev && k.1 == ino);
}

/// Forget everything cached for a file system view.
pub fn invalidate_dev(dev: u64) {
    drop_where(|k| k.0 == dev);
}

pub fn drop_all() {
    drop_where(|_| true);
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !on {
        drop_all();
    }
}

pub fn stats() -> String {
    let c = CACHE.lock();
    format!(
        "{} pages ({} KiB) of {}, {} hits, {} misses, {} evictions, {}\n",
        c.pages.len(),
        c.pages.len() * PAGE / 1024,
        CAPACITY,
        c.hits,
        c.misses,
        c.evictions,
        if ENABLED.load(Ordering::Relaxed) {
            "enabled"
        } else {
            "disabled"
        }
    )
}

static LAST_BENCH: SpinLock<String> = SpinLock::new(String::new());

/// Read `path` twice (after dropping its cached pages), timing each pass.
pub fn bench(path: &str) -> Result<String> {
    let f = super::ops::open(path, super::abi::O_RDONLY, 0)?;
    let st = f.stat();
    invalidate(st.dev, st.ino);
    let mut buf = alloc::vec![0u8; 64 * 1024];
    let mut pass = || -> Result<(u64, u64)> {
        let t0 = crate::timer::now_ns();
        let mut total = 0u64;
        loop {
            let n = f.pread(&mut buf, total)?;
            if n == 0 {
                break;
            }
            total += n as u64;
        }
        Ok((total, crate::timer::now_ns() - t0))
    };
    let (bytes, cold) = pass()?;
    let (_, warm) = pass()?;
    let mib_s = |ns: u64| (bytes * 1_000_000_000).checked_div(ns).unwrap_or(0) / (1 << 20);
    let ms = |ns: u64| format!("{}.{:03} ms", ns / 1_000_000, ns / 1000 % 1000);
    let r = format!(
        "{path}: {bytes} bytes, cache {}: first read {} ({} MiB/s), second read {} ({} MiB/s)\n",
        if ENABLED.load(Ordering::Relaxed) {
            "on"
        } else {
            "off"
        },
        ms(cold),
        mib_s(cold),
        ms(warm),
        mib_s(warm)
    );
    crate::kprintln!("pagecache bench: {}", r.trim_end());
    *LAST_BENCH.lock() = r.clone();
    Ok(r)
}

/// `/sys/fs/pagecache/`.
pub fn sysfs(dir: &alloc::sync::Arc<super::synth::SynthDir>) {
    dir.add_file("stats", stats);
    dir.add_control(
        "enabled",
        || {
            String::from(if ENABLED.load(Ordering::Relaxed) {
                "1\n"
            } else {
                "0\n"
            })
        },
        |b| match b.first() {
            Some(b'1') => {
                set_enabled(true);
                Ok(())
            }
            Some(b'0') => {
                set_enabled(false);
                Ok(())
            }
            _ => Err(Errno::EINVAL),
        },
    );
    dir.add_control("drop", String::new, |_| {
        drop_all();
        Ok(())
    });
    dir.add_control(
        "bench",
        || LAST_BENCH.lock().clone(),
        |b| {
            let p = core::str::from_utf8(b).map_err(|_| Errno::EINVAL)?.trim();
            bench(p).map(|_| ())
        },
    );
}
