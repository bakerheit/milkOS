//! epoll(7) and select (pselect6) (kernel/src/proc/epoll.rs).

use alloc::vec::Vec;

use crate::sys::{Errno, Result, syscall};

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLPRI: u32 = 0x002;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;
pub const EPOLL_CLOEXEC: usize = 0o2000000;
const ADD: usize = 1;
const DEL: usize = 2;
const MOD: usize = 3;

fn sc(n: usize, a: [usize; 6]) -> Result<usize> {
    // SAFETY: callers pass valid pointers.
    let r = unsafe { syscall(n, a) };
    if r < 0 {
        Err(Errno(-r as i32))
    } else {
        Ok(r as usize)
    }
}

/// One ready entry: its events and the `data` it was registered with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub events: u32,
    pub data: u64,
}

pub fn create() -> Result<i32> {
    sc(20, [EPOLL_CLOEXEC, 0, 0, 0, 0, 0]).map(|fd| fd as i32)
}

fn ctl(ep: i32, op: usize, fd: i32, events: u32, data: u64) -> Result<()> {
    let mut ev = [0u8; 16];
    ev[0..4].copy_from_slice(&events.to_le_bytes());
    ev[8..16].copy_from_slice(&data.to_le_bytes());
    sc(
        21,
        [ep as usize, op, fd as usize, ev.as_ptr() as usize, 0, 0],
    )
    .map(|_| ())
}

pub fn add(ep: i32, fd: i32, events: u32, data: u64) -> Result<()> {
    ctl(ep, ADD, fd, events, data)
}
pub fn modify(ep: i32, fd: i32, events: u32, data: u64) -> Result<()> {
    ctl(ep, MOD, fd, events, data)
}
pub fn del(ep: i32, fd: i32) -> Result<()> {
    ctl(ep, DEL, fd, 0, 0)
}

/// Wait up to `timeout_ms` (-1: forever, 0: just look) for up to `max` events.
pub fn wait(ep: i32, max: usize, timeout_ms: i32) -> Result<Vec<Event>> {
    let mut b = alloc::vec![0u8; max * 16];
    let n = sc(
        22,
        [
            ep as usize,
            b.as_mut_ptr() as usize,
            max,
            timeout_ms as isize as usize,
            0,
            8,
        ],
    )?;
    Ok((0..n)
        .map(|k| Event {
            events: u32::from_le_bytes(b[k * 16..k * 16 + 4].try_into().unwrap()),
            data: u64::from_le_bytes(b[k * 16 + 8..k * 16 + 16].try_into().unwrap()),
        })
        .collect())
}

/// select(2) via pselect6: fd bit sets (64 fds per word), updated in place.
/// `timeout_ns`: None = forever. Returns the number of bits set.
pub fn select(
    nfds: usize,
    rd: Option<&mut [u64]>,
    wr: Option<&mut [u64]>,
    ex: Option<&mut [u64]>,
    timeout_ns: Option<u64>,
) -> Result<usize> {
    let ts = timeout_ns.map(|t| [t / 1_000_000_000, t % 1_000_000_000]);
    let p = |s: Option<&mut [u64]>| s.map_or(0, |s| s.as_mut_ptr() as usize);
    sc(
        72,
        [
            nfds,
            p(rd),
            p(wr),
            p(ex),
            ts.as_ref().map_or(0, |t| t.as_ptr() as usize),
            0,
        ],
    )
}
