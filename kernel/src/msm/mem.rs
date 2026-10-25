//! The RAM list for the Pixel 2: what the device tree's `/memory` says, minus the regions it
//! reserves. The secure world (TrustZone, the hypervisor), the modem, the DSPs and a few more own
//! parts of RAM; touching them, even reading, can end in a bus error or a reset. Linux leaves the
//! `no-map` ones out of its linear map for the same reason. Every child of `/reserved-memory`
//! with an address, and every `/memreserve/`, comes out here, except the framebuffer (the
//! `splash_region`): that one is RAM we map and use, only not for the allocator
//! ([`super::fb::reserve_frames`]).

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::dtb::{Dtb, MAX_BANKS, MemRange};
use rpikit::msm::carve;

/// Numbers for the log, kept because the RAM list is made before the screen exists.
pub static LISTED: AtomicUsize = AtomicUsize::new(0);
pub static RESERVED: AtomicUsize = AtomicUsize::new(0);
pub static RESERVED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static BANKS_LEFT: AtomicUsize = AtomicUsize::new(0);

const MAX_RESERVED: usize = 64;

/// The reserved regions of the tree that are not the framebuffer.
pub fn reserved(d: &Dtb, out: &mut [MemRange; MAX_RESERVED]) -> usize {
    let mut n = d.mem_reserve(&mut out[..]);
    d.each_reserved_child(|name, base, size| {
        if name.starts_with(b"splash") || size == 0 || n >= MAX_RESERVED {
            return;
        }
        out[n] = MemRange {
            base: base as usize,
            size: size as usize,
        };
        n += 1;
    });
    n
}

/// Replace `tree[..n]` (the `/memory` banks) with the same minus the reserved regions; returns the
/// new count.
pub fn carve_tree(d: &Dtb, tree: &mut [MemRange; MAX_BANKS], n: usize) -> usize {
    let mut res = [MemRange { base: 0, size: 0 }; MAX_RESERVED];
    let nres = reserved(d, &mut res);
    let mut out = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    let left = carve(&tree[..n], &res[..nres], 4096, &mut out);
    tree[..left].copy_from_slice(&out[..left]);
    LISTED.store(n, Ordering::Relaxed);
    RESERVED.store(nres, Ordering::Relaxed);
    RESERVED_BYTES.store(res[..nres].iter().map(|r| r.size).sum(), Ordering::Relaxed);
    BANKS_LEFT.store(left, Ordering::Relaxed);
    left
}
