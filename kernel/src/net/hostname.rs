//! The machine's hostname: uname's nodename, set with sethostname(2) (root only). The
//! default is `milk`; at boot `net::init` applies `/System/Settings/milk/net/hostname` if it exists
//! (persistent on a machine with a Milk volume, like `/System/Settings/milk/drop`). `netctl hostname`
//! writes that file and sets the running name.

use alloc::string::String;

use super::errno::*;
use crate::fs::errno::Result;
use crate::sync::SpinLock;

pub const MAX: usize = 63;
pub const PERSIST: &str = "/System/Settings/milk/net/hostname";

const fn initial() -> ([u8; MAX], usize) {
    let mut b = [0u8; MAX];
    b[0] = b'm';
    b[1] = b'i';
    b[2] = b'l';
    b[3] = b'k';
    (b, 4)
}

static NAME: SpinLock<([u8; MAX], usize)> = SpinLock::new(initial());

/// A hostname we accept: one DNS label (RFC 1123): 1-63 letters, digits and '-', not
/// starting or ending with '-'.
pub fn valid(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= MAX
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

pub fn get() -> String {
    let g = NAME.lock();
    String::from_utf8_lossy(&g.0[..g.1]).into_owned()
}

pub fn set(name: &str) -> Result<()> {
    if !valid(name) {
        return Err(EINVAL);
    }
    let mut g = NAME.lock();
    g.0 = [0; MAX];
    g.0[..name.len()].copy_from_slice(name.as_bytes());
    g.1 = name.len();
    Ok(())
}

/// Apply the persisted hostname, if there is a valid one.
pub fn load() {
    if let Ok(t) = crate::fs::read_to_string(PERSIST) {
        let n = t.trim();
        match set(n) {
            Ok(()) => crate::kprintln!("[net] hostname {n} (from {PERSIST})"),
            Err(_) => crate::kprintln!("[net] {PERSIST}: {n:?} is not a hostname; keeping milk"),
        }
    }
}
