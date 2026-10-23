//! Bytes for the network API: a response body, a Blob's content, a request body. The script
//! side (`js/*.js`) holds them as opaque `Bytes` host objects and works on them through the
//! natives defined here (concatenate, slice, UTF-8 in and out), so a megabyte body never goes
//! through script as an array of numbers. Only where the page itself sees bytes (an
//! `ArrayBuffer`, a typed array, `Blob.arrayBuffer()`) are they copied in or out.
//!
//! The data lives in a store owned by the bridge, because the engine's heap can't hold
//! foreign memory; the `Bytes` objects are handles, and the store frees an entry when its
//! handle is collected ([`jsengine::HostClass::finalize`]). The store is bounded
//! ([`MAX_TOTAL`]), and since a handle is a few bytes of script heap the collector wouldn't
//! otherwise notice the memory behind it, [`Store::want_gc`] asks the runtime for a
//! collection at the end of the current task once enough has been made since the last one.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use jsengine::{ClassId, ErrorKind, HostClass, JsResult, Realm, Value};

use super::bridge::{Bridge, arg, arg_num, arg_str};

/// All the bytes alive at once.
pub const MAX_TOTAL: usize = 96 << 20;
/// One buffer.
pub const MAX_ONE: usize = 64 << 20;
/// Bytes made since the last collection that ask for another.
const GC_EVERY: usize = 8 << 20;

#[derive(Default)]
pub struct Store {
    items: BTreeMap<u64, Rc<Vec<u8>>>,
    total: usize,
    next: u64,
    since_gc: usize,
}

impl Store {
    /// A collection is worth running now (and the count starts again).
    pub fn want_gc(&mut self) -> bool {
        if self.since_gc >= GC_EVERY {
            self.since_gc = 0;
            true
        } else {
            false
        }
    }

    /// Bytes alive, and how many buffers (for the tests and the stats).
    pub fn live(&self) -> usize {
        self.total
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }
}

pub struct BytesClass {
    pub store: Rc<RefCell<Store>>,
}

impl HostClass for BytesClass {
    fn name(&self) -> &str {
        "Bytes"
    }
    fn finalize(&self, data: u64) {
        if let Ok(mut s) = self.store.try_borrow_mut()
            && let Some(v) = s.items.remove(&data)
        {
            s.total = s.total.saturating_sub(v.len());
        }
    }
}

/// The class and the store, made at install.
#[derive(Clone)]
pub struct Bytes {
    pub class: ClassId,
    pub store: Rc<RefCell<Store>>,
}

/// A new `Bytes` object holding `data`. Fails (a script error) over the limits.
pub fn make(b: &Rc<Bridge>, r: &mut Realm, data: Vec<u8>) -> JsResult<Value> {
    let Some(by) = b.bytes.get() else {
        return Err(r.throw_error(ErrorKind::Error, "bytes are not available"));
    };
    if data.len() > MAX_ONE {
        return Err(r.throw_error(
            ErrorKind::RangeError,
            "Array buffer allocation failed (too big)",
        ));
    }
    let id = {
        let mut s = by.store.borrow_mut();
        if s.total + data.len() > MAX_TOTAL {
            return Err(r.throw_error(
                ErrorKind::RangeError,
                "Array buffer allocation failed (out of memory)",
            ));
        }
        s.next += 1;
        let id = s.next;
        s.total += data.len();
        s.since_gc += data.len();
        s.items.insert(id, Rc::new(data));
        id
    };
    let proto = r.intrinsic(jsengine::Intrinsic::ObjectPrototype);
    Ok(r.new_host_object(by.class, id, proto))
}

/// The bytes behind a `Bytes` object (None if `v` is not one).
pub fn get(b: &Bridge, r: &Realm, v: Value) -> Option<Rc<Vec<u8>>> {
    let by = b.bytes.get()?;
    let id = r.host_data(v, by.class)?;
    by.store.borrow().items.get(&id).cloned()
}

/// A response body for script: a `Bytes` object.
pub fn bytes_value(b: &Rc<Bridge>, r: &mut Realm, body: &[u8]) -> JsResult<Value> {
    make(b, r, body.to_vec())
}

/// UTF-8 decode as the Encoding standard does it: a BOM is dropped, bad sequences become
/// U+FFFD.
pub fn decode_utf8(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(b).into_owned()
}

fn need(b: &Bridge, r: &mut Realm, v: Value) -> JsResult<Rc<Vec<u8>>> {
    match get(b, r, v) {
        Some(x) => Ok(x),
        None => Err(r.throw_error(ErrorKind::TypeError, "not a Bytes object")),
    }
}

/// Copy a page's `ArrayBuffer` / typed array / `DataView` in (None: not one, or detached).
pub fn from_buffer_source(r: &mut Realm, v: Value) -> JsResult<Option<Vec<u8>>> {
    if !v.is_object() || !(r.is_array_buffer(v) || r.is_array_buffer_view(v)) {
        return Ok(None);
    }
    Ok(r.copy_bytes(v))
}

/// Copy `data` out as a new `ArrayBuffer`.
pub fn to_array_buffer(r: &mut Realm, data: &[u8]) -> JsResult<Value> {
    r.new_array_buffer(data)
}

/// The natives on the `native` object.
pub fn define_natives(b: &Rc<Bridge>, r: &mut Realm, n: Value) -> JsResult<()> {
    let b2 = b.clone();
    r.define_method(n, "bytesFromString", 1, move |r, _t, a| {
        let s = arg_str(r, a, 0)?;
        make(&b2, r, s.into_bytes())
    })?;
    let b2 = b.clone();
    r.define_method(n, "bytesToString", 1, move |r, _t, a| {
        let x = need(&b2, r, arg(a, 0))?;
        let s = decode_utf8(&x);
        Ok(r.string(&s))
    })?;
    let b2 = b.clone();
    r.define_method(n, "bytesLength", 1, move |r, _t, a| {
        let x = need(&b2, r, arg(a, 0))?;
        Ok(Value::number(x.len() as f64))
    })?;
    // bytesConcat(array of Bytes)
    let b2 = b.clone();
    r.define_method(n, "bytesConcat", 1, move |r, _t, a| {
        let list = arg(a, 0);
        let len = r.get(list, "length")?;
        let len = r.to_number(len)?.clamp(0.0, 1_000_000.0) as u32;
        let mut out: Vec<u8> = Vec::new();
        for i in 0..len {
            let v = r.get(list, i)?;
            let x = need(&b2, r, v)?;
            if out.len() + x.len() > MAX_ONE {
                return Err(r.throw_error(ErrorKind::RangeError, "Array buffer allocation failed (too big)"));
            }
            out.extend_from_slice(&x);
        }
        make(&b2, r, out)
    })?;
    // bytesSlice(bytes, start, end): the range as a new Bytes (clamped).
    let b2 = b.clone();
    r.define_method(n, "bytesSlice", 3, move |r, _t, a| {
        let x = need(&b2, r, arg(a, 0))?;
        let len = x.len() as f64;
        let s = arg_num(r, a, 1)?;
        let e = if arg(a, 2).is_undefined() { len } else { arg_num(r, a, 2)? };
        let clamp = |v: f64| -> usize {
            let v = if v.is_nan() { 0.0 } else { v };
            let v = if v < 0.0 { (len + v).max(0.0) } else { v.min(len) };
            v as usize
        };
        let (s, e) = (clamp(s), clamp(e));
        let part = if s < e { x[s..e].to_vec() } else { Vec::new() };
        make(&b2, r, part)
    })?;
    // bytesFromBuffer(ArrayBuffer | view) -> Bytes or null
    let b2 = b.clone();
    r.define_method(n, "bytesFromBuffer", 1, move |r, _t, a| {
        match from_buffer_source(r, arg(a, 0))? {
            Some(v) => make(&b2, r, v),
            None => Ok(Value::NULL),
        }
    })?;
    // bytesToArrayBuffer(bytes) -> ArrayBuffer
    let b2 = b.clone();
    r.define_method(n, "bytesToArrayBuffer", 1, move |r, _t, a| {
        let x = need(&b2, r, arg(a, 0))?;
        to_array_buffer(r, &x)
    })?;
    // bytesIsBuffer(v): is it an ArrayBuffer / view the page made?
    r.define_method(n, "isBuffer", 1, move |r, _t, a| {
        Ok(Value::bool(from_buffer_source(r, arg(a, 0))?.is_some()))
    })?;
    // bytesText(bytes, encoding): decode with a label ("utf-8", "latin1" ...).
    let b2 = b.clone();
    r.define_method(n, "bytesDecode", 2, move |r, _t, a| {
        let x = need(&b2, r, arg(a, 0))?;
        let label = arg_str(r, a, 1)?.to_ascii_lowercase();
        let s: String = match label.trim() {
            "latin1" | "iso-8859-1" | "windows-1252" | "ascii" | "us-ascii" => {
                x.iter().map(|&c| crate::charset::windows1252(c)).collect()
            }
            "utf-16le" | "utf-16" => {
                let units: Vec<u16> = x.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&units)
            }
            _ => decode_utf8(&x),
        };
        Ok(r.string(&s))
    })?;
    // bytesFromBinary(string): one byte per UTF-16 unit (its low 8 bits); bytesToBinary(bytes).
    let b2 = b.clone();
    r.define_method(n, "bytesFromBinary", 1, move |r, _t, a| {
        let units = r.get_string_utf16(arg(a, 0)).unwrap_or_default();
        make(&b2, r, units.iter().map(|&u| u as u8).collect())
    })?;
    let b2 = b.clone();
    r.define_method(n, "bytesToBinary", 1, move |r, _t, a| {
        let x = need(&b2, r, arg(a, 0))?;
        let units: Vec<u16> = x.iter().map(|&c| c as u16).collect();
        Ok(r.string_utf16(&units))
    })?;
    // pctDecode(string): percent-decoding to a string (UTF-8, lossy).
    r.define_method(n, "pctDecode", 1, |r, _t, a| {
        let s = arg_str(r, a, 0)?;
        let d = crate::url::percent_decode(&s);
        Ok(r.string(&d))
    })?;
    // formEncode(string): application/x-www-form-urlencoded serializer.
    r.define_method(n, "formEncode", 1, |r, _t, a| {
        let s = arg_str(r, a, 0)?;
        let mut out = String::new();
        crate::url::form_urlencode(&s, &mut out);
        Ok(r.string(&out))
    })?;
    Ok(())
}
