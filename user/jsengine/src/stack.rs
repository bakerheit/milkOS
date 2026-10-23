//! A native stack guard: recursion in the parser, the compiler, JSON and native calls checks
//! how far the stack has grown since the host's entry and stops with an error well before
//! the thread's stack runs out.

/// Bytes of native stack an entry from the host may use (default; see
/// `Realm::set_stack_limit`).
pub(crate) const DEFAULT_STACK_LIMIT: usize = 768 << 10;

#[derive(Clone, Copy, Debug)]
pub(crate) struct StackGuard {
    pub base: usize,
    pub limit: usize,
}

#[inline(never)]
pub(crate) fn sp() -> usize {
    let x = 0u8;
    core::hint::black_box(&x) as *const u8 as usize
}

impl StackGuard {
    pub fn new(limit: usize) -> StackGuard {
        StackGuard { base: sp(), limit }
    }
    /// True when the stack has grown more than the limit below the base.
    #[inline]
    pub fn exceeded(&self) -> bool {
        self.base.saturating_sub(sp()) > self.limit
    }
}
