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

use jsengine::{Attrs, JsResult, Realm, Value};

use super::bridge::{Bridge, arg, dom_error};
use super::events::WINDOW;
use super::events_js;
use super::remote;
use super::xorigin::{self, Target};
use crate::api::*;

/// Messages one window may have queued (the Browser takes them every tick).
const MAX_QUEUE: usize = 2000;
/// The largest message, as text.
const MAX_MESSAGE: usize = 16 << 20;

const ENCODER: &str = r#"
var ids = new Map(), n = 0, bytes = 0;
function fail(m) { throw { __clone: m }; }
var HEX = '0123456789abcdef';
function hex(u8) { var s = []; for (var i = 0; i < u8.length; i++) s.push(HEX[u8[i] >> 4] + HEX[u8[i] & 15]); return s.join(''); }
function enc(v, depth) {
  if (depth > 100) fail('the message is nested too deeply');
  var t = typeof v;
  if (v === null) return null;
  if (t === 'undefined') return ['u'];
  if (t === 'boolean') return v;
  if (t === 'string') { bytes += v.length; if (bytes > 16000000) fail('the message is too large'); return v; }
  if (t === 'number') return (isFinite(v) && !(v === 0 && 1 / v < 0)) ? v : ['n', (v === 0) ? '-0' : String(v)];
  if (t === 'function') fail(String(v).slice(0, 40) + ' could not be cloned.');
  if (t === 'symbol') fail('A Symbol could not be cloned.');
  if (ids.has(v)) return ['ref', ids.get(v)];
  var tag = Object.prototype.toString.call(v).slice(8, -1);
  var id = n++;
  ids.set(v, id);
  var o, i;
  switch (tag) {
    case 'Array': o = ['a']; for (i = 0; i < v.length; i++) o.push(enc(v[i], depth + 1)); return o;
    case 'Date': return ['d', v.getTime()];
    case 'RegExp': return ['r', v.source, v.flags];
    case 'Map': o = ['m']; v.forEach(function (val, k) { o.push(enc(k, depth + 1), enc(val, depth + 1)); }); return o;
    case 'Set': o = ['s']; v.forEach(function (k) { o.push(enc(k, depth + 1)); }); return o;
    case 'ArrayBuffer': bytes += v.byteLength; return ['b', hex(new Uint8Array(v))];
    case 'Error': return ['e', String(v.name), String(v.message)];
    case 'Object':
      o = ['o'];
      Object.keys(v).forEach(function (k) { o.push(k, enc(v[k], depth + 1)); });
      return o;
    default:
      if (ArrayBuffer.isView(v) && tag !== 'DataView') {
        bytes += v.byteLength;
        return ['t', tag, hex(new Uint8Array(v.buffer, v.byteOffset, v.byteLength))];
      }
      fail('#<' + tag + '> could not be cloned.');
  }
}
return JSON.stringify(enc(value, 0));
"#;

const DECODER: &str = r#"
var seen = [];
function unhex(s) { var u = new Uint8Array(s.length >> 1); for (var i = 0; i < u.length; i++) u[i] = parseInt(s.substr(i * 2, 2), 16); return u; }
function dec(x) {
  if (x === null || typeof x !== 'object') return x;
  var i, o;
  switch (x[0]) {
    case 'u': return undefined;
    case 'n': return x[1] === '-0' ? -0 : x[1] === 'NaN' ? NaN : x[1] === 'Infinity' ? Infinity : -Infinity;
    case 'ref': return seen[x[1]];
    case 'a': o = []; seen.push(o); for (i = 1; i < x.length; i++) o.push(dec(x[i])); return o;
    case 'o': o = {}; seen.push(o); for (i = 1; i < x.length; i += 2) o[x[i]] = dec(x[i + 1]); return o;
    case 'd': o = new Date(x[1]); seen.push(o); return o;
    case 'r': o = new RegExp(x[1], x[2]); seen.push(o); return o;
    case 'm': o = new Map(); seen.push(o); for (i = 1; i < x.length; i += 2) { var k = dec(x[i]); o.set(k, dec(x[i + 1])); } return o;
    case 's': o = new Set(); seen.push(o); for (i = 1; i < x.length; i++) o.add(dec(x[i])); return o;
    case 'b': o = unhex(x[1]).buffer; seen.push(o); return o;
    case 'e': o = new Error(x[2]); o.name = x[1]; seen.push(o); return o;
    case 't': { var u = unhex(x[2]); var C = globalThis[x[1]]; o = (typeof C === 'function') ? new C(u.buffer) : u; seen.push(o); return o; }
  }
  return undefined;
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

/// `window.postMessage` called on the window of frame `target`.
pub fn post_message(b: &Rc<Bridge>, r: &mut Realm, target: u32, a: &[Value]) -> JsResult<Value> {
    // postMessage(message, targetOrigin) or postMessage(message, { targetOrigin }).
    let second = arg(a, 1);
    let origin_text: String = if second.is_object() {
        let t = r.get(second, "targetOrigin")?;
        if t.is_undefined() {
            String::from("/")
        } else {
            r.to_string(t)?
        }
    } else if second.is_undefined() {
        String::from("/")
    } else {
        r.to_string(second)?
    };
    let Some(t) = xorigin::parse_target(&origin_text) else {
        return Err(dom_error(
            r,
            "SyntaxError",
            &alloc::format!("Failed to execute 'postMessage' on 'Window': Invalid target origin '{origin_text}' in a call to 'postMessage'."),
        ));
    };
    let (enc, _) = cloners(b, r)?;
    let text = match r.call(enc, Value::UNDEFINED, &[arg(a, 0)]) {
        Ok(v) => r.get_string(v).unwrap_or_default(),
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
            return Err(e);
        }
    };
    if text.len() > MAX_MESSAGE {
        return Err(dom_error(r, "DataCloneError", "the message is too large"));
    }
    let target_origin = match t {
        Target::Any => String::from("*"),
        Target::SameAsSender => String::from("/"),
        Target::Origin(o) => o,
    };
    if let Ok(mut st) = b.st.try_borrow_mut()
        && st.outbox.q.len() < MAX_QUEUE
    {
        st.outbox.q.push(OutMessage {
            target: FrameId(target),
            target_origin,
            data: text,
        });
    }
    Ok(Value::UNDEFINED)
}

/// Make sure the realm has MessageEvent's prototype (kept pinned in the frame state).
fn ensure_message_proto(b: &Rc<Bridge>, r: &mut Realm) {
    if b.xf.message_proto.borrow().is_some() {
        return;
    }
    let ev = r.pinned(&b.protos().event);
    let proto = r.new_object_with_proto(ev);
    let cls = r.new_class("MessageEvent", 2, proto, None, None);
    let g = r.global();
    let _ = r.define(g, "MessageEvent", cls, Attrs::METHOD);
    *b.xf.message_proto.borrow_mut() = Some(r.pin(proto));
}

/// Fire the `message` event for `msg` at the window of `d`.
pub fn deliver(d: &mut Document, host: &mut dyn ScriptHost, msg: &Message) -> bool {
    let Some(js) = d.js.as_mut() else {
        return false;
    };
    let page = &mut d.page;
    js.entry(page, host, |b, r| {
        let data = if msg.data.is_empty() {
            Value::UNDEFINED
        } else {
            let Ok((_, dec)) = cloners(b, r) else {
                return false;
            };
            let t = r.string(&msg.data);
            match r.call(dec, Value::UNDEFINED, &[t]) {
                Ok(v) => v,
                Err(_) => return false,
            }
        };
        ensure_message_proto(b, r);
        let e = {
            let p = b.xf.message_proto.borrow();
            let Some(proto) = p.as_ref() else { return false };
            events_js::new_event(b, r, proto, "message", events_js::TRUSTED)
        };
        let ro = Attrs::ENUMERABLE;
        let _ = r.define(e, "data", data, ro);
        let o = r.string(&msg.origin);
        let _ = r.define(e, "origin", o, ro);
        let lid = r.string("");
        let _ = r.define(e, "lastEventId", lid, ro);
        let src = remote::window_of(b, r, msg.source.0).unwrap_or(Value::NULL);
        let _ = r.define(e, "source", src, ro);
        let ports = r.new_array(&[]);
        let _ = r.define(e, "ports", ports, ro);
        events_js::dispatch(b, r, WINDOW, e);
        true
    })
    .unwrap_or(false)
}
