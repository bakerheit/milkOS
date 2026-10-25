//! Kernel log ring: everything `kprintln!` prints is kept here (as well as going to the UART), so
//! the desktop's Terminal can show the boot log and follow new lines live.
//!
//! A fixed 32 KiB byte ring with a running byte count. Readers remember the count they have read
//! up to and ask for everything after it ([`read_since`]); if they fall more than a ring behind,
//! they just get the newest 32 KiB. Writers that find the lock taken (an IRQ handler printing while
//! the main loop is reading) drop their text from the ring; it still reached the UART.

use crate::sync::SpinLock;

const SIZE: usize = 32 * 1024; // power of two

struct Ring {
    buf: [u8; SIZE],
    /// Total bytes ever written. `buf[i % SIZE]` holds byte `i` for the last SIZE bytes.
    total: u64,
}

static RING: SpinLock<Ring> = SpinLock::new(Ring {
    buf: [0; SIZE],
    total: 0,
});

/// Append text to the ring.
pub fn write(s: &str) {
    let Some(mut r) = RING.try_lock() else {
        return;
    };
    for &b in s.as_bytes() {
        let i = (r.total as usize) & (SIZE - 1);
        r.buf[i] = b;
        r.total += 1;
    }
}

/// Call `f` with the bytes written after position `from` (at most the last 32 KiB, in at most two
/// slices, oldest first). Returns the new position.
pub fn read_since(from: u64, mut f: impl FnMut(&[u8])) -> u64 {
    let Some(r) = RING.try_lock() else {
        return from;
    };
    let start = from.max(r.total.saturating_sub(SIZE as u64));
    if start >= r.total {
        return r.total;
    }
    let (a, b) = (
        (start as usize) & (SIZE - 1),
        (r.total as usize) & (SIZE - 1),
    );
    if a < b {
        f(&r.buf[a..b]);
    } else {
        f(&r.buf[a..]);
        f(&r.buf[..b]);
    }
    r.total
}

/// Call `f` with the last `max` bytes of the log, oldest first, in at most two slices (for a
/// crash screen: it takes the lock only if it is free, so a crash while printing still shows what
/// the screen got). Returns false if the lock was taken.
pub fn tail(max: usize, mut f: impl FnMut(&[u8])) -> bool {
    let Some(r) = RING.try_lock() else {
        return false;
    };
    let n = (max.min(SIZE) as u64).min(r.total);
    let start = r.total - n;
    let (a, b) = (
        (start as usize) & (SIZE - 1),
        (r.total as usize) & (SIZE - 1),
    );
    if n == 0 {
        return true;
    }
    if a < b {
        f(&r.buf[a..b]);
    } else {
        f(&r.buf[a..]);
        f(&r.buf[..b]);
    }
    true
}

/// Where [`copy_since`] stopped.
#[cfg(feature = "raspi")]
pub struct Copied {
    /// Bytes copied into the caller's buffer.
    pub n: usize,
    /// The position to ask from next time (the count of log bytes consumed so far).
    pub next: u64,
    /// Bytes between `from` and the first copied byte that the ring had already overwritten.
    pub lost: u64,
}

#[cfg(feature = "raspi")]
/// Bytes logged so far (the position a reader that has everything would have), or `None` if the
/// ring is being written right now.
pub fn total() -> Option<u64> {
    RING.try_lock().map(|r| r.total)
}

#[cfg(feature = "raspi")]
/// Copy the log bytes after position `from` into `out` (as many as fit; oldest first), for the
/// boot log file. Never blocks: `None` if the ring is busy, unless `force` (a crash: the holder is
/// the context that died, or another CPU that is about to stop) reads it without the lock.
pub fn copy_since(from: u64, out: &mut [u8], force: bool) -> Option<Copied> {
    let guard = RING.try_lock();
    // SAFETY: only reads bytes of the array and the counter; see SpinLock::data_unchecked.
    let r: &Ring = match &guard {
        Some(g) => g,
        None if force => unsafe { &*RING.data_unchecked() },
        None => return None,
    };
    let total = r.total;
    let oldest = total.saturating_sub(SIZE as u64);
    let start = from.max(oldest);
    let lost = start - from.min(start);
    let n = ((total.saturating_sub(start)) as usize).min(out.len());
    for (i, o) in out[..n].iter_mut().enumerate() {
        *o = r.buf[((start + i as u64) as usize) & (SIZE - 1)];
    }
    Some(Copied {
        n,
        next: start + n as u64,
        lost,
    })
}
