//! Network requests from scripts: `fetch`, `XMLHttpRequest`, `navigator.sendBeacon`, and the
//! classes around them (Headers, Request, Response, Blob, FormData, URL, URLSearchParams,
//! AbortController, ProgressEvent, ...).
//!
//! The JavaScript half is `js/*.js`: the classes themselves, written in script because that is
//! what they are (promises, iterators, event targets). It is compiled and run the first time a
//! page touches one of the names in [`GLOBALS`] (each is an accessor on `window` until then,
//! so a page that never makes a request pays nothing), in the page's own realm, and gets one
//! argument, the `native` object made here: the few things script can't do itself (start and
//! cancel a request, resolve an address, report an error, set a timer, read a form).
//!
//! This half never does I/O and never decides what a request may do. [`start`] checks that
//! the request is well formed (an http or https address, a method token, header names and
//! values, a body within [`MAX_REQUEST_BODY`], not more than [`MAX_PENDING`] outstanding) and
//! leaves it in [`NetState::out`] for the app ([`crate::Document::take_net`]). The app's
//! answers come back through [`deliver`], one call from the app per task, and are handed to
//! the callback the script side gave `start`. The same-origin policy, CORS, cookies,
//! redirects and limits are the app's (they need the network and the cookie jar); the script
//! side only makes the API shape right and drops what a script may never set.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, JsResult, Pinned, Realm, Value};

use super::bridge::{Bridge, arg, arg_num, arg_str, type_error};
use super::netbytes;
use crate::api::*;

/// The script side, one function body taking `native`.
const PRELUDE: &str = concat!(
    include_str!("js/base.js"),
    include_str!("js/url.js"),
    include_str!("js/headers.js"),
    include_str!("js/body.js"),
    include_str!("js/fetch.js"),
    include_str!("js/xhr.js"),
    include_str!("js/ports.js"),
    include_str!("js/exports.js"),
);

/// The names this adds to `window`.
pub const GLOBALS: &[&str] = &[
    "fetch",
    "Headers",
    "Request",
    "Response",
    "Blob",
    "File",
    "FileReader",
    "FormData",
    "URL",
    "URLSearchParams",
    "AbortController",
    "AbortSignal",
    "XMLHttpRequest",
    "XMLHttpRequestEventTarget",
    "XMLHttpRequestUpload",
    "ProgressEvent",
    "DOMException",
    "ReadableStream",
    "MessageChannel",
    "MessagePort",
];

/// Requests a page may have outstanding at once (the app has its own, lower limits).
pub const MAX_PENDING: usize = 256;
/// The largest request body a script can hand over.
pub const MAX_REQUEST_BODY: usize = 32 << 20;
/// The longest address.
pub const MAX_URL: usize = 64 * 1024;
const MAX_HEADERS: usize = 256;
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct NetState {
    next_id: u32,
    /// Outstanding requests: the script callback of each.
    pending: BTreeMap<u32, Pinned>,
    /// What the app has not taken yet.
    pub out: Vec<NetOut>,
    /// Module fetches outstanding (script/modrt.rs), by request id.
    pub mod_fetches: BTreeMap<u32, super::modrt::Fetch>,
    /// The classes, once the script side has run.
    exports: Option<Pinned>,
    loading: bool,
}

impl NetState {
    pub fn pending(&self) -> usize {
        self.pending.len() + self.mod_fetches.len()
    }

    /// A fresh request id.
    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id.wrapping_add(1).max(1);
        self.next_id = id;
        id
    }

    /// The scripts end: forget every request (the app is told to cancel them). The pins to
    /// give back to the realm.
    pub fn clear(&mut self) -> Vec<Pinned> {
        let mut ids: Vec<u32> = self.pending.keys().copied().collect();
        ids.extend(self.mod_fetches.keys().copied());
        self.mod_fetches.clear();
        for id in ids {
            self.out.push(NetOut::Cancel(id));
        }
        let mut pins: Vec<Pinned> = core::mem::take(&mut self.pending).into_values().collect();
        if let Some(e) = self.exports.take() {
            pins.push(e);
        }
        pins
    }
}

// ---- installing ------------------------------------------------------------------------------

/// The lazy globals and `navigator.sendBeacon`.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let g = r.global();
    let store = Rc::new(core::cell::RefCell::new(netbytes::Store::default()));
    let class = r.register_class(netbytes::BytesClass { store: store.clone() });
    let _ = b.bytes.set(netbytes::Bytes { class, store });
    for &name in GLOBALS {
        let b2 = b.clone();
        let getter = r.new_function(&format!("get {name}"), 0, move |r, _t, _a| {
            let ex = exports(&b2, r)?;
            let v = r.get(ex, name)?;
            let g = r.global();
            // Now an ordinary property: `window.fetch = ...` and `delete` work as usual.
            r.define(g, name, v, Attrs::METHOD)?;
            Ok(v)
        });
        let setter = r.new_function(&format!("set {name}"), 1, move |r, _t, a| {
            let g = r.global();
            r.define(g, name, arg(a, 0), Attrs::METHOD)?;
            Ok(Value::UNDEFINED)
        });
        r.define_accessor(g, name, getter, setter, Attrs::CONFIGURABLE)?;
    }
    // navigator.sendBeacon(url, data): the script side, loaded on first use.
    let nav = r.get(g, "navigator")?;
    if nav.is_object() {
        let b2 = b.clone();
        r.define_method(nav, "sendBeacon", 1, move |r, _t, a| {
            let ex = exports(&b2, r)?;
            let f = r.get(ex, "sendBeacon")?;
            r.call(f, Value::UNDEFINED, a)
        })?;
    }
    Ok(())
}

/// The script side's exports object (running it the first time).
pub fn exports(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<Value> {
    {
        let st = b.st(r)?;
        if let Some(p) = &st.net.exports {
            return Ok(r.pinned(p));
        }
        if st.net.loading {
            return Err(type_error(r, "the network API is still loading"));
        }
    }
    b.st(r)?.net.loading = true;
    let made = load(b, r);
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.net.loading = false;
    }
    let v = made?;
    let p = r.pin(v);
    b.st(r)?.net.exports = Some(p);
    Ok(v)
}

fn load(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<Value> {
    let native = native_object(b, r)?;
    let f = r.compile_function(&["native"], PRELUDE, "fetch-api")?;
    r.call(f, Value::UNDEFINED, &[native])
}

// ---- the native object -----------------------------------------------------------------------

fn native_object(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<Value> {
    let n = r.new_object();
    let g = r.global();
    r.define(n, "global", g, Attrs::METHOD)?;
    // start(desc, callback) -> id (0: refused)
    let b2 = b.clone();
    r.define_method(n, "start", 2, move |r, _t, a| start(&b2, r, a))?;
    let b2 = b.clone();
    r.define_method(n, "cancel", 1, move |r, _t, a| {
        let id = arg_num(r, a, 0)? as u32;
        let gone = b2.st(r)?.net.pending.remove(&id);
        if let Some(p) = gone {
            r.unpin(p);
            b2.st(r)?.net.out.push(NetOut::Cancel(id));
        }
        Ok(Value::UNDEFINED)
    })?;
    // parseUrl(href, base?) -> the parts of the address, or null.
    r.define_method(n, "parseUrl", 2, |r, _t, a| {
        let href = arg_str(r, a, 0)?;
        let base = if arg(a, 1).is_undefined() { None } else { Some(arg_str(r, a, 1)?) };
        match parse_url(&href, base.as_deref()) {
            Some(u) => url_object(r, &u),
            None => Ok(Value::NULL),
        }
    })?;
    // location() -> the page's address.
    let b2 = b.clone();
    r.define_method(n, "location", 0, move |r, _t, _a| {
        let s = b2.page().map(|p| String::from(p.page_url())).unwrap_or_default();
        Ok(r.string(&s))
    })?;
    // log(level, text)
    let b2 = b.clone();
    r.define_method(n, "log", 2, move |r, _t, a| {
        let level = match arg_str(r, a, 0)?.as_str() {
            "error" => ConsoleLevel::Error,
            "warn" => ConsoleLevel::Warn,
            "info" => ConsoleLevel::Info,
            "debug" => ConsoleLevel::Debug,
            _ => ConsoleLevel::Log,
        };
        let text = arg_str(r, a, 1)?;
        b2.console(level, text);
        Ok(Value::UNDEFINED)
    })?;
    // report(error): an exception nobody catches (a listener's), to the console.
    let b2 = b.clone();
    r.define_method(n, "report", 1, move |r, _t, a| {
        let e = r.throw(arg(a, 0));
        super::report(&b2, r, &e, "");
        Ok(Value::UNDEFINED)
    })?;
    // setTimer(fn, ms) -> id; clearTimer(id): the page's own timers.
    let b2 = b.clone();
    r.define_method(n, "setTimer", 2, move |r, _t, a| {
        super::window::timer_add(&b2, r, &[arg(a, 0), arg(a, 1)], false)
    })?;
    let b2 = b.clone();
    r.define_method(n, "clearTimer", 1, move |r, _t, a| super::window::timer_clear(&b2, r, a))?;
    // now() -> ms (monotonic)
    let b2 = b.clone();
    r.define_method(n, "now", 0, move |_r, _t, _a| {
        Ok(Value::number(b2.host().map_or(0.0, |h| h.now())))
    })?;
    // time() -> ms since 1970
    let b2 = b.clone();
    r.define_method(n, "time", 0, move |_r, _t, _a| {
        Ok(Value::number(b2.host().map_or(0.0, |h| h.time())))
    })?;
    // formEntries(form) -> [[name, value], ...]
    netbytes::define_natives(b, r, n)?;
    super::ports::define_natives(b, r, n)?;
    r.define_method(n, "formEntries", 1, {
        let b2 = b.clone();
        move |r, _t, a| form_entries(&b2, r, a)
    })?;
    Ok(n)
}

/// The entries of a form element, `[[name, value], ...]` (what a submission would send; file
/// inputs and buttons that were not pressed add nothing).
fn form_entries(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    let node = super::nodes::node_of(b, r, arg(a, 0));
    let out = r.new_array(&[]);
    let Some(n) = node else { return Ok(out) };
    let sub = b.page().and_then(|p| {
        p.sync_forms();
        p.submit_form_node(n, None)
    });
    let Some(sub) = sub else { return Ok(out) };
    let query = if sub.body.is_empty() {
        sub.url.split_once('?').map(|x| x.1).unwrap_or("")
    } else {
        core::str::from_utf8(&sub.body).unwrap_or("")
    };
    let query = query.split('#').next().unwrap_or("");
    for pair in query.split('&').filter(|p| !p.is_empty()).take(10_000) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let dec = |s: &str| crate::url::percent_decode(&s.replace('+', " "));
        let k = r.string(&dec(k));
        let v = r.string(&dec(v));
        let p = r.new_array(&[k, v]);
        r.array_push(out, p)?;
    }
    Ok(out)
}

// ---- starting a request ----------------------------------------------------------------------

fn get_str(r: &mut Realm, o: Value, k: &str) -> JsResult<Option<String>> {
    let v = r.get(o, k)?;
    if v.is_undefined() || v.is_null() {
        Ok(None)
    } else {
        Ok(Some(r.to_string(v)?))
    }
}

/// An HTTP token (method names, header names).
pub fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^'
                        | b'_' | b'`' | b'|' | b'~'
                )
        })
}

/// A header value the wire can carry (no NUL, CR or LF).
pub fn is_header_value(s: &str) -> bool {
    !s.bytes().any(|c| c == 0 || c == b'\r' || c == b'\n')
}

fn start(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    let desc = arg(a, 0);
    let cb = arg(a, 1);
    if !desc.is_object() || !r.is_callable(cb) {
        return Err(type_error(r, "start: bad arguments"));
    }
    let refuse = |why: &str| -> JsResult<Value> {
        b.console(ConsoleLevel::Error, String::from(why));
        Ok(Value::int(0))
    };
    let url = get_str(r, desc, "url")?.unwrap_or_default();
    if url.len() > MAX_URL {
        return refuse("The request's address is too long");
    }
    let Some(parsed) = parse_url(&url, None) else {
        return refuse(&format!("Can't fetch {url}: not an address"));
    };
    if parsed.scheme != "http" && parsed.scheme != "https" {
        return refuse(&format!(
            "Can't fetch {}: only http and https addresses can be fetched",
            clip(&url)
        ));
    }
    let url = parsed.href_no_fragment();
    let method = get_str(r, desc, "method")?.unwrap_or_else(|| String::from("GET"));
    if !is_token(&method) {
        return refuse("The request's method isn't valid");
    }
    // Headers: [[name, value], ...].
    let mut headers: Vec<(String, String)> = Vec::new();
    let hs = r.get(desc, "headers")?;
    if hs.is_object() {
        let n = r.get(hs, "length")?;
        let n = r.to_number(n)?.clamp(0.0, MAX_HEADERS as f64) as u32;
        let mut bytes = 0usize;
        for i in 0..n {
            let pair = r.get(hs, i)?;
            let k = r.get(pair, 0u32)?;
            let v = r.get(pair, 1u32)?;
            let (k, v) = (r.to_string(k)?, r.to_string(v)?);
            if !is_token(&k) || !is_header_value(&v) {
                continue;
            }
            bytes += k.len() + v.len();
            if bytes > MAX_HEADER_BYTES {
                return refuse("The request's headers are too big");
            }
            headers.push((k, v));
        }
    }
    let body: Vec<u8> = {
        let v = r.get(desc, "body")?;
        if v.is_undefined() || v.is_null() {
            Vec::new()
        } else if let Some(s) = r.get_string(v) {
            s.into_bytes()
        } else if let Some(x) = netbytes::get(b, r, v) {
            (*x).clone()
        } else {
            netbytes::from_buffer_source(r, v)?.unwrap_or_default()
        }
    };
    if body.len() > MAX_REQUEST_BODY {
        return refuse(&format!(
            "The request's body is too big ({} MB; the limit is {} MB)",
            body.len() >> 20,
            MAX_REQUEST_BODY >> 20
        ));
    }
    let mode = match get_str(r, desc, "mode")?.as_deref() {
        Some("same-origin") => RequestMode::SameOrigin,
        Some("no-cors") => RequestMode::NoCors,
        Some("navigate") => RequestMode::Navigate,
        _ => RequestMode::Cors,
    };
    let credentials = match get_str(r, desc, "credentials")?.as_deref() {
        Some("omit") => CredentialsMode::Omit,
        Some("include") => CredentialsMode::Include,
        _ => CredentialsMode::SameOrigin,
    };
    let redirect = match get_str(r, desc, "redirect")?.as_deref() {
        Some("error") => RedirectMode::Error,
        Some("manual") => RedirectMode::Manual,
        _ => RedirectMode::Follow,
    };
    let kind = match get_str(r, desc, "kind")?.as_deref() {
        Some("xhr") => NetKind::Xhr,
        Some("beacon") => NetKind::Beacon,
        _ => NetKind::Fetch,
    };
    let referrer = get_str(r, desc, "referrer")?.unwrap_or_else(|| String::from("about:client"));
    let referrer_policy = get_str(r, desc, "referrerPolicy")?.unwrap_or_default();
    let cache = get_str(r, desc, "cache")?.unwrap_or_else(|| String::from("default"));
    let timeout = r.get(desc, "timeout")?;
    let timeout_ms = if timeout.is_undefined() {
        0
    } else {
        r.to_number(timeout)?.clamp(0.0, u32::MAX as f64) as u32
    };
    let force_preflight = {
        let v = r.get(desc, "forcePreflight")?;
        r.to_boolean(v)
    };
    {
        let st = b.st(r)?;
        if st.net.pending.len() >= MAX_PENDING {
            drop(st);
            return refuse("Too many requests are outstanding at once");
        }
    }
    let pin = r.pin(cb);
    let mut st = b.st(r)?;
    let id = st.net.alloc_id();
    st.net.pending.insert(id, pin);
    st.net.out.push(NetOut::Start(NetRequest {
        id,
        kind,
        url,
        method,
        headers,
        body,
        mode,
        credentials,
        redirect,
        referrer,
        referrer_policy,
        cache,
        timeout_ms,
        force_preflight,
    }));
    Ok(Value::int(id as i32))
}

fn clip(s: &str) -> String {
    let mut e = s.len().min(200);
    while !s.is_char_boundary(e) {
        e -= 1;
    }
    String::from(&s[..e])
}

// ---- answers from the app --------------------------------------------------------------------

/// One answer for request `id`: the script side's callback gets `(kind, data)`.
pub fn deliver(b: &Rc<Bridge>, r: &mut Realm, id: u32, ev: NetEvent) -> bool {
    // A module fetch (script/modrt.rs).
    if super::modrt::is_module_fetch(b, id) {
        return super::modrt::deliver(b, r, id, ev);
    }
    let cb = match b.st.try_borrow() {
        Ok(st) => st.net.pending.get(&id).map(|p| r.pinned(p)),
        Err(_) => None,
    };
    let Some(cb) = cb else { return false };
    let last = matches!(ev, NetEvent::Done(_) | NetEvent::Failed(_));
    let (ty, data) = match &ev {
        NetEvent::Head(h) => ("head", head_object(r, h)),
        NetEvent::Progress { loaded, total } => {
            let o = r.new_object();
            let _ = r.set(o, "loaded", Value::number(*loaded as f64));
            let _ = r.set(o, "total", Value::number(total.map_or(-1.0, |t| t as f64)));
            ("progress", Ok(o))
        }
        NetEvent::Done(body) => ("done", netbytes::bytes_value(b, r, body)),
        NetEvent::Failed(f) => {
            let o = r.new_object();
            let k = match f.kind {
                NetFailureKind::Network => "network",
                NetFailureKind::Blocked => "blocked",
                NetFailureKind::TooBig => "toobig",
                NetFailureKind::Timeout => "timeout",
            };
            let k = r.string(k);
            let m = r.string(&f.message);
            let _ = r.set(o, "kind", k);
            let _ = r.set(o, "message", m);
            // The console says why (the page can't see more than "Failed to fetch").
            b.console(ConsoleLevel::Error, f.message.clone());
            ("failed", Ok(o))
        }
    };
    let Ok(data) = data else { return false };
    let ty = r.string(ty);
    b.st.borrow_mut().stats.callbacks += 1;
    super::call_report(b, r, cb, Value::UNDEFINED, &[ty, data]);
    if last {
        let gone = b.st.borrow_mut().net.pending.remove(&id);
        if let Some(p) = gone {
            r.unpin(p);
        }
    }
    true
}

fn head_object(r: &mut Realm, h: &NetHead) -> JsResult<Value> {
    let o = r.new_object();
    r.set(o, "status", Value::int(h.status as i32))?;
    let t = r.string(&h.status_text);
    r.set(o, "statusText", t)?;
    let u = r.string(&h.url);
    r.set(o, "url", u)?;
    r.set(o, "redirected", Value::bool(h.redirected))?;
    let k = r.string(match h.kind {
        ResponseKind::Basic => "basic",
        ResponseKind::Cors => "cors",
        ResponseKind::Opaque => "opaque",
        ResponseKind::OpaqueRedirect => "opaqueredirect",
    });
    r.set(o, "type", k)?;
    let list = r.new_array(&[]);
    for (name, value) in h.headers.iter().take(MAX_HEADERS * 4) {
        let n = r.string(name);
        let v = r.string(value);
        let p = r.new_array(&[n, v]);
        r.array_push(list, p)?;
    }
    r.set(o, "headers", list)?;
    Ok(o)
}

// ---- addresses -------------------------------------------------------------------------------

/// An address taken apart, as the URL standard would (for the schemes that matter here).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Url {
    /// Lower case, no colon.
    pub scheme: String,
    pub username: String,
    pub password: String,
    /// Lower case; with the brackets for an IPv6 address; "" for a URL without an authority.
    pub host: String,
    /// None: the scheme's default port or none given.
    pub port: Option<u16>,
    pub has_authority: bool,
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

fn special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

/// Percent-encode what `keep` refuses (and every non-ASCII byte), leaving existing escapes.
fn encode_with(s: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c >= 0x80 || c <= 0x20 || c == 0x7f || !keep(c) {
            const H: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(H[(c >> 4) as usize] as char);
            out.push(H[(c & 15) as usize] as char);
        } else {
            out.push(c as char);
        }
    }
    out
}

fn path_ok(c: u8) -> bool {
    !matches!(c, b'"' | b'#' | b'<' | b'>' | b'?' | b'`' | b'{' | b'}')
}

fn query_ok(special: bool) -> impl Fn(u8) -> bool {
    move |c| !matches!(c, b'"' | b'#' | b'<' | b'>') && !(special && c == b'\'')
}

fn fragment_ok(c: u8) -> bool {
    !matches!(c, b'"' | b'<' | b'>' | b'`')
}

fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && !h.bytes().any(|c| {
            c <= 0x20
                || c == 0x7f
                || matches!(c, b':' | b'#' | b'/' | b'<' | b'>' | b'?' | b'@' | b'\\' | b'^' | b'|' | b'%' | b'"' | b'`' | b'{' | b'}')
        })
}

/// Parse `input` (against `base`, if given) into its parts; None if it is not an address.
pub fn parse_url(input: &str, base: Option<&str>) -> Option<Url> {
    let full = match base {
        Some(b) => {
            // The base must itself be an address.
            parse_url(b, None)?;
            crate::url::resolve(b, input)
        }
        None => {
            let t: String = input
                .trim_matches(|c: char| c <= ' ')
                .chars()
                .filter(|&c| !matches!(c, '\t' | '\n' | '\r'))
                .collect();
            t
        }
    };
    let (scheme, rest) = full.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let sp = special(&scheme);
    let rest = if sp {
        // Backslashes are slashes before the query.
        let q = rest.find(['?', '#']).unwrap_or(rest.len());
        let mut s = rest[..q].replace('\\', "/");
        s.push_str(&rest[q..]);
        s
    } else {
        String::from(rest)
    };
    let (rest, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest.as_str(), None),
    };
    let (rest, query) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let mut u = Url {
        scheme,
        fragment: fragment.map(|f| encode_with(f, fragment_ok)),
        query: query.map(|q| encode_with(q, query_ok(sp))),
        ..Default::default()
    };
    if sp && !rest.starts_with("//") && u.scheme != "file" {
        // "http:foo" with no base: the standard treats it like "http://foo".
        let r2 = rest.trim_start_matches('/');
        return finish_authority(u, r2);
    }
    if let Some(auth_path) = rest.strip_prefix("//") {
        return finish_authority(u, auth_path);
    }
    // No authority: an opaque path (mailto:, data:, javascript:, blob:) or a rooted path.
    u.path = if sp {
        encode_with(rest, path_ok)
    } else {
        // Opaque paths keep their characters except controls.
        encode_with(rest, |c| c >= 0x21 || c == b' ')
            .replace("%20", " ")
    };
    Some(u)
}

fn finish_authority(mut u: Url, auth_path: &str) -> Option<Url> {
    let sp = special(&u.scheme);
    let end = auth_path.find('/').unwrap_or(auth_path.len());
    let (auth, path) = auth_path.split_at(end);
    u.has_authority = true;
    let (userinfo, hostport) = match auth.rfind('@') {
        Some(i) => (&auth[..i], &auth[i + 1..]),
        None => ("", auth),
    };
    if !userinfo.is_empty() {
        let (name, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        let ui = |c: u8| !matches!(c, b'"' | b'#' | b'<' | b'>' | b'?' | b'`' | b'{' | b'}' | b'/' | b':' | b';' | b'=' | b'@' | b'[' | b'\\' | b']' | b'^' | b'|');
        u.username = encode_with(name, ui);
        u.password = encode_with(pass, ui);
    }
    let (host, port) = if hostport.starts_with('[') {
        let close = hostport.find(']')?;
        let after = &hostport[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return None,
        };
        (&hostport[..=close], port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    if let Some(p) = port
        && !p.is_empty()
    {
        if !p.bytes().all(|c| c.is_ascii_digit()) || p.len() > 5 {
            return None;
        }
        let n: u32 = p.parse().ok()?;
        if n > 65535 {
            return None;
        }
        u.port = if Some(n as u16) == default_port(&u.scheme) { None } else { Some(n as u16) };
    }
    let host = host.to_ascii_lowercase();
    if host.starts_with('[') {
        if !host[1..host.len() - 1].bytes().all(|c| c.is_ascii_hexdigit() || c == b':' || c == b'.') {
            return None;
        }
    } else if sp && u.scheme != "file" {
        let host = crate::url::percent_decode(&host);
        if !host_ok(&host) {
            return None;
        }
        u.host = host;
    } else if !host.is_empty() && !host_ok(&host) {
        return None;
    }
    if u.host.is_empty() {
        u.host = host;
    }
    if sp && u.scheme != "file" && u.host.is_empty() {
        return None;
    }
    // The path: dot segments are already removed by `resolve` when there was a base; do it for
    // the no-base case too.
    let p = if path.is_empty() && sp { "/" } else { path };
    let p = if sp { remove_dots(p) } else { String::from(p) };
    u.path = encode_with(&p, path_ok);
    Some(u)
}

fn remove_dots(path: &str) -> String {
    if !path.contains("/.") {
        return String::from(path);
    }
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    let n = segs.len();
    for (i, seg) in segs.iter().enumerate() {
        if i == 0 {
            continue;
        }
        let last = i + 1 == n;
        let low = seg.to_ascii_lowercase();
        match low.as_str() {
            "." | "%2e" => {
                if last {
                    out.push("");
                }
            }
            ".." | ".%2e" | "%2e." | "%2e%2e" => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            _ => out.push(seg),
        }
    }
    let mut s = String::from("/");
    s.push_str(&out.join("/"));
    s
}

impl Url {
    pub fn host_port(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn origin(&self) -> String {
        match self.scheme.as_str() {
            "http" | "https" | "ws" | "wss" | "ftp" => {
                format!("{}://{}", self.scheme, self.host_port())
            }
            "blob" => parse_url(&self.path, None)
                .filter(|u| u.scheme == "http" || u.scheme == "https")
                .map_or_else(|| String::from("null"), |u| u.origin()),
            _ => String::from("null"),
        }
    }

    pub fn search(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }
    }

    pub fn hash(&self) -> String {
        match &self.fragment {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        }
    }

    pub fn href_no_fragment(&self) -> String {
        let mut s = format!("{}:", self.scheme);
        if self.has_authority {
            s.push_str("//");
            if !self.username.is_empty() || !self.password.is_empty() {
                s.push_str(&self.username);
                if !self.password.is_empty() {
                    s.push(':');
                    s.push_str(&self.password);
                }
                s.push('@');
            }
            s.push_str(&self.host_port());
        }
        s.push_str(&self.path);
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }

    pub fn href(&self) -> String {
        let mut s = self.href_no_fragment();
        if let Some(f) = &self.fragment {
            s.push('#');
            s.push_str(f);
        }
        s
    }
}

fn url_object(r: &mut Realm, u: &Url) -> JsResult<Value> {
    let o = r.new_object();
    let put = |r: &mut Realm, k: &str, v: &str| -> JsResult<()> {
        let s = r.string(v);
        r.set(o, k, s)
    };
    put(r, "href", &u.href())?;
    put(r, "protocol", &format!("{}:", u.scheme))?;
    put(r, "username", &u.username)?;
    put(r, "password", &u.password)?;
    put(r, "host", &u.host_port())?;
    put(r, "hostname", &u.host)?;
    put(r, "port", &u.port.map(|p| format!("{p}")).unwrap_or_default())?;
    put(r, "pathname", &u.path)?;
    put(r, "search", &u.search())?;
    put(r, "hash", &u.hash())?;
    put(r, "origin", &u.origin())?;
    r.set(o, "authority", Value::bool(u.has_authority))?;
    Ok(o)
}
