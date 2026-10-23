
// ---- Headers ----------------------------------------------------------------------------------------

// The names a script may never set on a request (Fetch "forbidden request-header"), and the
// prefixes. The app checks again.
const FORBIDDEN_REQUEST = new Set([
  'accept-charset', 'accept-encoding', 'access-control-request-headers', 'access-control-request-method',
  'connection', 'content-length', 'cookie', 'cookie2', 'date', 'dnt', 'expect', 'host', 'keep-alive',
  'origin', 'referer', 'set-cookie', 'te', 'trailer', 'transfer-encoding', 'upgrade', 'via',
]);
const FORBIDDEN_RESPONSE = new Set(['set-cookie', 'set-cookie2']);

function forbiddenRequestHeader(lower) {
  return FORBIDDEN_REQUEST.has(lower) || lower.startsWith('proxy-') || lower.startsWith('sec-');
}

// Methods that can be tunnelled through an allowed one with these headers are refused when the
// method is forbidden (Fetch step "forbidden method override").
const FORBIDDEN_METHODS = new Set(['connect', 'trace', 'track']);

// A value is safe for the "CORS-safelisted" rules: short, no unsafe bytes.
function corsUnsafeByte(c) {
  return (c < 0x20 && c !== 0x09) || c === 0x22 || c === 0x28 || c === 0x29 || c === 0x3a || c === 0x3c ||
    c === 0x3e || c === 0x3f || c === 0x40 || c === 0x5b || c === 0x5c || c === 0x5d || c === 0x7b ||
    c === 0x7d || c === 0x7f;
}
function mimeEssence(v) {
  const i = v.indexOf(';');
  return (i < 0 ? v : v.slice(0, i)).trim().toLowerCase();
}
function noCorsSafelisted(lower, value) {
  if (value.length > 128) return false;
  switch (lower) {
    case 'accept':
      for (let i = 0; i < value.length; i++) if (corsUnsafeByte(value.charCodeAt(i))) return false;
      return true;
    case 'accept-language':
    case 'content-language':
      return /^[0-9A-Za-z *,\-.;=]*$/.test(value);
    case 'content-type': {
      for (let i = 0; i < value.length; i++) if (corsUnsafeByte(value.charCodeAt(i))) return false;
      const e = mimeEssence(value);
      return e === 'application/x-www-form-urlencoded' || e === 'multipart/form-data' || e === 'text/plain';
    }
    default:
      return false;
  }
}

function normalizeValue(v) {
  v = trimHttp(str(v));
  for (let i = 0; i < v.length; i++) {
    const c = v.charCodeAt(i);
    if (c > 255) throw new TypeError("Failed to execute 'append' on 'Headers': String contains non ISO-8859-1 code point.");
    if (c === 0 || c === 10 || c === 13) throw new TypeError("Failed to execute 'append' on 'Headers': Invalid value");
  }
  return v;
}
function checkName(n, what) {
  n = str(n);
  if (!isToken(n)) throw new TypeError("Failed to execute '" + what + "' on 'Headers': Invalid name");
  return n;
}

// The header list is an array of [lower-case name, value]; iteration sorts and combines.
function hdrGet(list, lower) {
  let out = null;
  for (const p of list) {
    if (p[0] === lower) out = out === null ? p[1] : out + ', ' + p[1];
  }
  return out;
}

class Headers {
  constructor(init) {
    const s = setSlots(this, { brand: 'Headers', list: [], guard: 'none' });
    if (init !== undefined && init !== null) fillHeaders(this, s, init);
    else if (init === null) throw new TypeError("Failed to construct 'Headers': The provided value is not of type '(record<ByteString, ByteString> or sequence<sequence<ByteString>>)'.");
  }
  append(name, value) {
    need(arguments, 2, 'append', 'Headers');
    appendHeader(slots(this, 'Headers'), checkName(name, 'append'), normalizeValue(value));
  }
  delete(name) {
    need(arguments, 1, 'delete', 'Headers');
    const s = slots(this, 'Headers');
    const lower = checkName(name, 'delete').toLowerCase();
    if (s.guard === 'immutable') throw new TypeError('Headers are immutable');
    if (s.guard === 'request' && forbiddenRequestHeader(lower)) return;
    if (s.guard === 'request-no-cors' && !noCorsSafelisted(lower, '')) return;
    if (s.guard === 'response' && FORBIDDEN_RESPONSE.has(lower)) return;
    s.list = s.list.filter((p) => p[0] !== lower);
  }
  get(name) {
    need(arguments, 1, 'get', 'Headers');
    return hdrGet(slots(this, 'Headers').list, checkName(name, 'get').toLowerCase());
  }
  getSetCookie() {
    const out = [];
    for (const p of slots(this, 'Headers').list) if (p[0] === 'set-cookie') out.push(p[1]);
    return out;
  }
  has(name) {
    need(arguments, 1, 'has', 'Headers');
    const lower = checkName(name, 'has').toLowerCase();
    for (const p of slots(this, 'Headers').list) if (p[0] === lower) return true;
    return false;
  }
  set(name, value) {
    need(arguments, 2, 'set', 'Headers');
    const s = slots(this, 'Headers');
    const n = checkName(name, 'set');
    const v = normalizeValue(value);
    const lower = n.toLowerCase();
    if (!guardAllows(s, lower, v)) return;
    let placed = false;
    const out = [];
    for (const p of s.list) {
      if (p[0] !== lower) out.push(p);
      else if (!placed) {
        placed = true;
        out.push([lower, v]);
      }
    }
    if (!placed) out.push([lower, v]);
    s.list = out;
  }
  forEach(callback, thisArg) {
    need(arguments, 1, 'forEach', 'Headers');
    if (typeof callback !== 'function') throw new TypeError("Failed to execute 'forEach' on 'Headers': The callback provided as parameter 1 is not a function.");
    for (const [k, v] of sortedList(slots(this, 'Headers'))) callback.call(thisArg, v, k, this);
  }
  *entries() {
    for (const p of sortedList(slots(this, 'Headers'))) yield [p[0], p[1]];
  }
  *keys() {
    for (const p of sortedList(slots(this, 'Headers'))) yield p[0];
  }
  *values() {
    for (const p of sortedList(slots(this, 'Headers'))) yield p[1];
  }
}
Headers.prototype[SymIter] = Headers.prototype.entries;
tag(Headers);
enumerable(Headers, ['append', 'delete', 'get', 'getSetCookie', 'has', 'set', 'forEach', 'entries', 'keys', 'values']);

// Does the guard let (name, value) be added?
function guardAllows(s, lower, value) {
  switch (s.guard) {
    case 'immutable':
      throw new TypeError('Headers are immutable');
    case 'request':
      return !forbiddenRequestHeader(lower);
    case 'request-no-cors': {
      const had = hdrGet(s.list, lower);
      const combined = had === null ? value : had + ', ' + value;
      return noCorsSafelisted(lower, combined);
    }
    case 'response':
      return !FORBIDDEN_RESPONSE.has(lower);
    default:
      return true;
  }
}

function appendHeader(s, name, value) {
  const lower = name.toLowerCase();
  if (!guardAllows(s, lower, value)) return;
  s.list.push([lower, value]);
}

function fillHeaders(h, s, init) {
  if (isObj(init)) {
    if (typeof init[SymIter] === 'function') {
      for (const pair of init) {
        if (!isObj(pair) || typeof pair[SymIter] !== 'function') {
          throw new TypeError("Failed to construct 'Headers': The provided value cannot be converted to a sequence.");
        }
        const items = Array.from(pair);
        if (items.length !== 2) throw new TypeError("Failed to construct 'Headers': Invalid value");
        appendHeader(s, checkName(items[0], 'append'), normalizeValue(items[1]));
      }
    } else {
      for (const k of Reflect.ownKeys(init)) {
        if (typeof k !== 'string') continue;
        const d = Object.getOwnPropertyDescriptor(init, k);
        if (!d || !d.enumerable) continue;
        appendHeader(s, checkName(k, 'append'), normalizeValue(init[k]));
      }
    }
  } else {
    throw new TypeError("Failed to construct 'Headers': The provided value is not of type '(record<ByteString, ByteString> or sequence<sequence<ByteString>>)'.");
  }
}

// Sorted by name, equal names combined (Set-Cookie stays separate), as iteration shows them.
function sortedList(s) {
  const names = [];
  for (const p of s.list) if (names.indexOf(p[0]) < 0) names.push(p[0]);
  names.sort();
  const out = [];
  for (const n of names) {
    if (n === 'set-cookie') {
      for (const p of s.list) if (p[0] === n) out.push([n, p[1]]);
    } else {
      out.push([n, hdrGet(s.list, n)]);
    }
  }
  return out;
}

// A Headers object with a guard, made inside the API.
function makeHeaders(guard, pairs) {
  const h = new Headers();
  const s = h[S];
  if (pairs) for (const p of pairs) appendHeader(s, p[0], p[1]);
  s.guard = guard;
  return h;
}
function headersList(h) {
  return slots(h, 'Headers').list;
}
