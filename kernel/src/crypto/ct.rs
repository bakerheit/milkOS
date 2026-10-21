//! Constant-time comparison and zeroing.

/// `a == b` without an early exit (same length assumed public).
pub fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    // Keep the compiler from turning the loop back into an early exit.
    core::hint::black_box(d) == 0
}

/// Overwrite secrets (volatile, so it isn't optimised away).
pub fn zeroize(b: &mut [u8]) {
    for x in b.iter_mut() {
        // SAFETY: `x` is a valid, exclusively borrowed byte.
        unsafe { core::ptr::write_volatile(x, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
