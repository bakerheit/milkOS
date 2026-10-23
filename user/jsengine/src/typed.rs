//! ArrayBuffer, typed arrays and DataView: the storage, the element codecs and the realm's
//! element access. The JavaScript-visible constructors and methods are in
//! `builtins/typedarray.rs`.
//!
//! An ArrayBuffer owns a `Vec<u8>`. A typed array or DataView is a view: the buffer's handle
//! plus a byte offset and a length fixed at creation (there are no resizable buffers), so the
//! only way a view's range stops being valid is the buffer being detached (its bytes taken
//! away), which reads as length 0 and makes every access fail the way the spec says.
//! Elements are little-endian in memory (DataView takes an explicit endianness).
//!
//! Bytes count against the heap budget (`Obj::size_estimate`), and one buffer may not be
//! larger than the realm's cap ([`Realm::set_max_buffer_bytes`]).

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::conv::f64_to_int32;
use crate::object::ObjKind;
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

/// The default cap on one ArrayBuffer.
pub(crate) const DEFAULT_MAX_BUFFER: usize = 32 << 20;

pub(crate) struct BufData {
    pub bytes: Vec<u8>,
    pub detached: bool,
}

/// The element types (no BigInt in this engine, so no BigInt64Array).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum TaKind {
    Int8,
    Uint8,
    Uint8C,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
}

pub(crate) const TA_KINDS: [TaKind; 9] = [
    TaKind::Int8,
    TaKind::Uint8,
    TaKind::Uint8C,
    TaKind::Int16,
    TaKind::Uint16,
    TaKind::Int32,
    TaKind::Uint32,
    TaKind::Float32,
    TaKind::Float64,
];

impl TaKind {
    #[inline]
    pub fn size(self) -> usize {
        match self {
            TaKind::Int8 | TaKind::Uint8 | TaKind::Uint8C => 1,
            TaKind::Int16 | TaKind::Uint16 => 2,
            TaKind::Int32 | TaKind::Uint32 | TaKind::Float32 => 4,
            TaKind::Float64 => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            TaKind::Int8 => "Int8Array",
            TaKind::Uint8 => "Uint8Array",
            TaKind::Uint8C => "Uint8ClampedArray",
            TaKind::Int16 => "Int16Array",
            TaKind::Uint16 => "Uint16Array",
            TaKind::Int32 => "Int32Array",
            TaKind::Uint32 => "Uint32Array",
            TaKind::Float32 => "Float32Array",
            TaKind::Float64 => "Float64Array",
        }
    }
    pub fn from_index(i: i32) -> TaKind {
        TA_KINDS[(i.clamp(0, 8)) as usize]
    }

    /// Decodes one element (`b` is exactly `size()` bytes, little-endian).
    #[inline]
    pub fn read(self, b: &[u8]) -> f64 {
        match self {
            TaKind::Int8 => b[0] as i8 as f64,
            TaKind::Uint8 | TaKind::Uint8C => b[0] as f64,
            TaKind::Int16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            TaKind::Uint16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            TaKind::Int32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            TaKind::Uint32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            TaKind::Float32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            TaKind::Float64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        }
    }

    /// Encodes a number: integer types wrap modulo 2^n (ToInt32 and truncation), the clamped
    /// type clamps and rounds half to even, Float32 rounds to nearest.
    #[inline]
    pub fn write(self, n: f64, dst: &mut [u8]) {
        match self {
            TaKind::Int8 | TaKind::Uint8 => dst[0] = f64_to_int32(n) as u8,
            TaKind::Uint8C => dst[0] = clamp_u8(n),
            TaKind::Int16 | TaKind::Uint16 => {
                dst[..2].copy_from_slice(&(f64_to_int32(n) as u16).to_le_bytes())
            }
            TaKind::Int32 | TaKind::Uint32 => {
                dst[..4].copy_from_slice(&f64_to_int32(n).to_le_bytes())
            }
            TaKind::Float32 => dst[..4].copy_from_slice(&(n as f32).to_le_bytes()),
            TaKind::Float64 => dst[..8].copy_from_slice(&n.to_le_bytes()),
        }
    }
}

/// ToUint8Clamp.
pub(crate) fn clamp_u8(n: f64) -> u8 {
    if n.is_nan() || n <= 0.0 {
        return 0;
    }
    if n >= 255.0 {
        return 255;
    }
    let t = n as u8;
    let frac = n - t as f64;
    if frac < 0.5 {
        t
    } else if frac > 0.5 {
        t + 1
    } else if t & 1 == 0 {
        t
    } else {
        t + 1
    }
}

pub(crate) struct TypedData {
    pub kind: TaKind,
    pub buf: ObjId,
    /// Byte offset into the buffer, and the length in elements.
    pub offset: usize,
    pub len: usize,
}

pub(crate) struct ViewData {
    pub buf: ObjId,
    pub offset: usize,
    /// Length in bytes.
    pub len: usize,
}

impl Realm {
    // ---- creating ----

    /// Checks a buffer of `n` bytes may be made: under the cap and able to fit the heap.
    pub(crate) fn check_buffer_size(&mut self, n: usize) -> Res<()> {
        if n > self.max_buffer || !self.heap.could_fit(n) {
            return Err(self.range_error("Array buffer allocation failed"));
        }
        Ok(())
    }

    /// An ArrayBuffer over bytes already made (the size was checked by the caller).
    pub(crate) fn buffer_from_vec(&mut self, bytes: Vec<u8>) -> ObjId {
        let p = self.intr.array_buffer_proto;
        self.heap.work += (bytes.len() >> 6) as u64;
        self.alloc(
            Some(p),
            ObjKind::ArrayBuffer(Box::new(BufData {
                bytes,
                detached: false,
            })),
        )
    }

    /// A zeroed ArrayBuffer.
    pub(crate) fn alloc_buffer(&mut self, len: usize, proto: Option<ObjId>) -> Res<ObjId> {
        self.check_buffer_size(len)?;
        let id = self.buffer_from_vec(alloc::vec![0u8; len]);
        if let Some(p) = proto {
            self.om(id).proto = Some(p);
        }
        Ok(id)
    }

    pub(crate) fn typed_new(
        &mut self,
        kind: TaKind,
        buf: ObjId,
        offset: usize,
        len: usize,
        proto: Option<ObjId>,
    ) -> ObjId {
        let p = proto.unwrap_or_else(|| self.intr.ta_proto(kind));
        self.alloc(
            Some(p),
            ObjKind::Typed(Box::new(TypedData {
                kind,
                buf,
                offset,
                len,
            })),
        )
    }

    /// A new zeroed typed array of `len` elements on a new buffer.
    pub(crate) fn typed_alloc(
        &mut self,
        kind: TaKind,
        len: usize,
        proto: Option<ObjId>,
    ) -> Res<ObjId> {
        let bytes = len.saturating_mul(kind.size());
        let buf = self.alloc_buffer(bytes, None)?;
        // Keep the buffer alive until the view holds it (allocation never collects, but be
        // explicit for readers).
        Ok(self.typed_new(kind, buf, 0, len, proto))
    }

    // ---- element access ----

    /// The kind, buffer, byte offset and length of a typed array.
    #[inline]
    pub(crate) fn typed_info(&self, id: ObjId) -> Option<(TaKind, ObjId, usize, usize)> {
        match &self.o(id).kind {
            ObjKind::Typed(t) => Some((t.kind, t.buf, t.offset, t.len)),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn buf_data(&self, buf: ObjId) -> Option<&BufData> {
        match &self.o(buf).kind {
            ObjKind::ArrayBuffer(b) => Some(b),
            _ => None,
        }
    }

    pub(crate) fn buf_detached(&self, buf: ObjId) -> bool {
        self.buf_data(buf).is_none_or(|b| b.detached)
    }

    /// The length in elements, 0 once the buffer is detached.
    #[inline]
    pub(crate) fn typed_len(&self, id: ObjId) -> usize {
        match &self.o(id).kind {
            ObjKind::Typed(t) => {
                if self.buf_detached(t.buf) {
                    0
                } else {
                    t.len
                }
            }
            _ => 0,
        }
    }

    /// The element at `i`, or None when out of range or detached.
    #[inline]
    pub(crate) fn typed_get(&self, id: ObjId, i: usize) -> Option<Val> {
        let ObjKind::Typed(t) = &self.o(id).kind else {
            return None;
        };
        if i >= t.len {
            return None;
        }
        let ObjKind::ArrayBuffer(b) = &self.o(t.buf).kind else {
            return None;
        };
        let sz = t.kind.size();
        let at = t.offset + i * sz;
        let s = b.bytes.get(at..at + sz)?;
        Some(Val::num(t.kind.read(s)))
    }

    /// Stores a number at `i` (a no-op when out of range or detached). True if stored.
    #[inline]
    pub(crate) fn typed_put(&mut self, id: ObjId, i: usize, n: f64) -> bool {
        let (kind, buf, offset, len) = match &self.o(id).kind {
            ObjKind::Typed(t) => (t.kind, t.buf, t.offset, t.len),
            _ => return false,
        };
        if i >= len {
            return false;
        }
        let ObjKind::ArrayBuffer(b) = &mut self.heap.om(buf).kind else {
            return false;
        };
        let sz = kind.size();
        let at = offset + i * sz;
        match b.bytes.get_mut(at..at + sz) {
            Some(d) => {
                kind.write(n, d);
                true
            }
            None => false,
        }
    }

    /// The bytes a typed array views (empty if detached).
    pub(crate) fn typed_bytes(&self, id: ObjId) -> &[u8] {
        let ObjKind::Typed(t) = &self.o(id).kind else {
            return &[];
        };
        match self.buf_data(t.buf) {
            Some(b) if !b.detached => b
                .bytes
                .get(t.offset..t.offset + t.len * t.kind.size())
                .unwrap_or(&[]),
            _ => &[],
        }
    }

    pub(crate) fn typed_bytes_mut(&mut self, id: ObjId) -> &mut [u8] {
        let (buf, off, n) = match &self.o(id).kind {
            ObjKind::Typed(t) => (t.buf, t.offset, t.len * t.kind.size()),
            _ => return &mut [],
        };
        match &mut self.heap.om(buf).kind {
            ObjKind::ArrayBuffer(b) if !b.detached => {
                b.bytes.get_mut(off..off + n).unwrap_or(&mut [])
            }
            _ => &mut [],
        }
    }

    /// Takes a buffer's bytes away: it becomes detached (length 0) and every view of it
    /// stops working. Returns the bytes.
    pub(crate) fn detach_buffer(&mut self, buf: ObjId) -> Vec<u8> {
        match &mut self.heap.om(buf).kind {
            ObjKind::ArrayBuffer(b) => {
                b.detached = true;
                core::mem::take(&mut b.bytes)
            }
            _ => Vec::new(),
        }
    }

    /// Whether `s` (a property name) is a CanonicalNumericIndexString: for a typed array
    /// such a key is never an ordinary property ("-1", "1.5", "NaN", "-0", "Infinity").
    pub(crate) fn canonical_numeric(&self, k: crate::object::PropKey) -> bool {
        let crate::object::PropKey::Str(s) = k else {
            return matches!(k, crate::object::PropKey::Idx(_));
        };
        let u = self.heap.flat(s);
        let Some(&c0) = u.first() else { return false };
        // Cheap reject: only digits, '-', 'I' (Infinity) and 'N' (NaN) can start one.
        if !(c0 == b'-' as u16
            || c0 == b'I' as u16
            || c0 == b'N' as u16
            || crate::string::is_ascii_digit(c0))
        {
            return false;
        }
        if u.len() == 2 && u[0] == b'-' as u16 && u[1] == b'0' as u16 {
            return true;
        }
        let n = crate::numconv::string_to_number(u);
        if n.is_nan() && u != [b'N' as u16, b'a' as u16, b'N' as u16] {
            return false;
        }
        let back = crate::numconv::number_to_string(n);
        back.encode_utf16().eq(u.iter().copied())
    }
}
