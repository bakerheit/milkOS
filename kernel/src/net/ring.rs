//! Byte ring buffer over fixed memory (socket send/receive buffers).
//!
//! The memory comes from the socket pool (allocated once at boot) so nothing here
//! allocates: rings are filled and drained from IRQ context.

pub struct Ring {
    buf: *mut u8,
    cap: usize,
    /// Offset of the first byte.
    head: usize,
    len: usize,
}

// SAFETY: the memory is owned by the socket pool slot this ring belongs to; access is
// serialised by the stack lock.
unsafe impl Send for Ring {}

impl Ring {
    /// # Safety
    /// `buf..buf+cap` must be valid, writable and used by nothing else while the ring lives.
    pub const unsafe fn new(buf: *mut u8, cap: usize) -> Ring {
        Ring {
            buf,
            cap,
            head: 0,
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn cap(&self) -> usize {
        self.cap
    }
    pub fn free(&self) -> usize {
        self.cap - self.len
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn mem(&self) -> &[u8] {
        // SAFETY: see `new`.
        unsafe { core::slice::from_raw_parts(self.buf, self.cap) }
    }
    fn mem_mut(&mut self) -> &mut [u8] {
        // SAFETY: see `new`.
        unsafe { core::slice::from_raw_parts_mut(self.buf, self.cap) }
    }

    /// Append as much of `data` as fits; returns bytes taken.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free());
        let start = (self.head + self.len) % self.cap;
        let first = n.min(self.cap - start);
        let m = self.mem_mut();
        m[start..start + first].copy_from_slice(&data[..first]);
        m[..n - first].copy_from_slice(&data[first..n]);
        self.len += n;
        n
    }

    /// Write `data` into free space `off` bytes past the end, without making it readable
    /// (TCP out-of-order data). Returns how much fit. [`commit`](Self::commit) later.
    pub fn write_at(&mut self, off: usize, data: &[u8]) -> usize {
        if off >= self.free() {
            return 0;
        }
        let n = data.len().min(self.free() - off);
        let start = (self.head + self.len + off) % self.cap;
        let first = n.min(self.cap - start);
        let m = self.mem_mut();
        m[start..start + first].copy_from_slice(&data[..first]);
        m[..n - first].copy_from_slice(&data[first..n]);
        n
    }

    /// Make `n` bytes already written past the end (by `write_at`) readable.
    pub fn commit(&mut self, n: usize) {
        self.len += n.min(self.free());
    }

    /// Copy bytes `[off, off + out.len())` (relative to the head) without consuming them.
    /// Returns how many were available.
    pub fn peek_at(&self, off: usize, out: &mut [u8]) -> usize {
        if off >= self.len {
            return 0;
        }
        let n = out.len().min(self.len - off);
        let start = (self.head + off) % self.cap;
        let first = n.min(self.cap - start);
        let m = self.mem();
        out[..first].copy_from_slice(&m[start..start + first]);
        out[first..n].copy_from_slice(&m[..n - first]);
        n
    }

    /// Drop `n` bytes from the head.
    pub fn consume(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.cap;
        self.len -= n;
        // Don't re-home `head` to 0 when empty: out-of-order bytes may already sit past
        // the end (`write_at`), at positions relative to the current head.
    }

    /// Copy out and consume up to `out.len()` bytes.
    pub fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = self.peek_at(0, out);
        self.consume(n);
        n
    }
}
