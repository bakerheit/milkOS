//! `window.postMessage` and the `message` event.
//!
//! A message is copied with the structured clone algorithm's reach: strings, numbers (`NaN`,
//! the infinities and `-0` included), booleans, `null`, `undefined`, plain objects and arrays,
//! `Date`, `RegExp`, `Map`, `Set`, `Error`, `ArrayBuffer` and the typed arrays, with cycles and
//! shared references kept. A function, a symbol, a DOM node, a window or anything else throws
//! a `DataCloneError` in the sender. Not done: transferables (the third argument is
//! ignored; `ports` is always empty), `Blob`/`File`/`ImageData`, class instances keeping their
//! class (they arrive as plain objects, as in browsers).
//!
//! The copy travels as JSON text with a tag in front of everything JSON cannot say (the
//! engine's [`OutMessage::data`] is opaque text): `["d", ms]` a date, `["m", k1, v1, ...]` a map
//! and so on, written and read by two small script functions compiled once per realm.
//!
//! Delivery is asynchronous: `postMessage` only queues the message ([`OutMessage`]); the
//! Browser takes the queues ([`Document::take_all_messages`]), asks
//! [`Document::route_message`] whether the `targetOrigin` matches and what the receiver sees,
//! and calls [`Document::deliver_message`], which fires `message` at the receiving window as a
//! task of its event loop. A message to a frame that has gone, or whose origin is not the
//! `targetOrigin`, is dropped without a word (the sender cannot tell).

use alloc::rc::Rc;
use alloc::string::String;

use jsengine::{JsResult, Realm, Value};

use super::bridge::{Bridge, dom_error};

const ENCODER: &str = r#"
var ids = new Map(), n = 0, bytes = 0;
function fail(m) { throw { __clone: m }; }
var HEX = '0123456789abcdef';
function hex(u8) { var s = []; for (var i = 0; i < u8.length; i++) s.push(HEX[u8[i] >> 4] + HEX[u8[i] & 15]); return s.join(''); }
function tag(t, v) { var o = { $t: t }; if (v !== undefined) o.v = v; return o; }
function enc(v, depth) {
  if (depth > 100) fail('the message is nested too deeply');
  var t = typeof v;
  if (v === null) return null;
  if (t === 'undefined') return tag('u');
  if (t === 'boolean') return v;
  if (t === 'string') { bytes += v.length; if (bytes > 16000000) fail('the message is too large'); return v; }
  if (t === 'number') return (isFinite(v) && !(v === 0 && 1 / v < 0)) ? v : tag('n', (v === 0) ? '-0' : String(v));
  if (t === 'function') fail(String(v).slice(0, 40) + ' could not be cloned.');
  if (t === 'symbol') fail('A Symbol could not be cloned.');
  if (ids.has(v)) return tag('ref', ids.get(v));
  var cls = Object.prototype.toString.call(v).slice(8, -1);
  var id = n++;
  ids.set(v, id);
  var o, i;
  switch (cls) {
    case 'Array': o = []; for (i = 0; i < v.length; i++) o.push(enc(v[i], depth + 1)); return o;
    case 'Date': return tag('d', v.getTime());
    case 'RegExp': return tag('r', [v.source, v.flags]);
    case 'Map': o = []; v.forEach(function (val, k) { o.push(enc(k, depth + 1), enc(val, depth + 1)); }); return tag('m', o);
    case 'Set': o = []; v.forEach(function (k) { o.push(enc(k, depth + 1)); }); return tag('s', o);
    case 'ArrayBuffer': bytes += v.byteLength; return tag('b', hex(new Uint8Array(v)));
    case 'Error': return tag('e', [String(v.name), String(v.message)]);
    case 'Object':
      o = {};
      Object.keys(v).forEach(function (k) { o[k.charAt(0) === '$' ? '$' + k : k] = enc(v[k], depth + 1); });
      return o;
    default:
      if (ArrayBuffer.isView(v) && cls !== 'DataView') {
        bytes += v.byteLength;
        return tag('t', [cls, hex(new Uint8Array(v.buffer, v.byteOffset, v.byteLength))]);
      }
      fail('#<' + cls + '> could not be cloned.');
  }
}
return JSON.stringify(enc(value, 0));
"#;

const DECODER: &str = r#"
var seen = [];
function unhex(s) { var u = new Uint8Array(s.length >> 1); for (var i = 0; i < u.length; i++) u[i] = parseInt(s.substr(i * 2, 2), 16); return u; }
function dec(x) {
  if (x === null || typeof x !== 'object') return x;
  var i, o, v;
  if (Array.isArray(x)) { o = []; seen.push(o); for (i = 0; i < x.length; i++) o.push(dec(x[i])); return o; }
  if (Object.prototype.hasOwnProperty.call(x, '$t')) {
    v = x.v;
    switch (x.$t) {
      case 'u': return undefined;
      case 'n': return v === '-0' ? -0 : v === 'NaN' ? NaN : v === 'Infinity' ? Infinity : -Infinity;
      case 'ref': return seen[v];
      case 'd': o = new Date(v); seen.push(o); return o;
      case 'r': o = new RegExp(v[0], v[1]); seen.push(o); return o;
      case 'm': o = new Map(); seen.push(o); for (i = 0; i < v.length; i += 2) { var k = dec(v[i]); o.set(k, dec(v[i + 1])); } return o;
      case 's': o = new Set(); seen.push(o); for (i = 0; i < v.length; i++) o.add(dec(v[i])); return o;
      case 'b': o = unhex(v).buffer; seen.push(o); return o;
      case 'e': o = new Error(v[1]); o.name = v[0]; seen.push(o); return o;
      case 't': { var u = unhex(v[1]); var C = globalThis[v[0]]; o = (typeof C === 'function') ? new C(u.buffer) : u; seen.push(o); return o; }
    }
    return undefined;
  }
  o = {}; seen.push(o);
  Object.keys(x).forEach(function (k) { o[k.slice(0, 2) === '$$' ? k.slice(1) : k] = dec(x[k]); });
  return o;
}
return dec(JSON.parse(text));
"#;

fn cloners(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<(Value, Value)> {
    if let Some((e, d)) = b.xf.cloner.borrow().as_ref() {
        return Ok((r.pinned(e), r.pinned(d)));
    }
    let e = r.compile_function(&["value"], ENCODER, "postMessage")?;
    let d = r.compile_function(&["text"], DECODER, "message")?;
    let pe = r.pin(e);
    let pd = r.pin(d);
    *b.xf.cloner.borrow_mut() = Some((pe, pd));
    Ok((e, d))
}

/// The tagged JSON of `v` (the structured clone); a `DataCloneError` for what cannot be copied.
pub fn encode(b: &Rc<Bridge>, r: &mut Realm, v: Value) -> JsResult<String> {
    if v.is_undefined() {
        return Ok(String::new());
    }
    let (enc, _) = cloners(b, r)?;
    match r.call(enc, Value::UNDEFINED, &[v]) {
        Ok(v) => Ok(r.get_string(v).unwrap_or_default()),
        Err(e) => {
            // A thrown {__clone: reason} is the cloner saying no.
            if let Some(v) = e.value()
                && v.is_object()
                && let Ok(m) = r.get(v, "__clone")
                && m.is_string()
            {
                let why = r.get_string(m).unwrap_or_default();
                return Err(dom_error(r, "DataCloneError", &why));
            }
            Err(e)
        }
    }
}

/// The value a message's text stands for, in this realm.
pub fn decode(b: &Rc<Bridge>, r: &mut Realm, text: &str) -> JsResult<Value> {
    let (_, dec) = cloners(b, r)?;
    let t = r.string(text);
    r.call(dec, Value::UNDEFINED, &[t])
}
