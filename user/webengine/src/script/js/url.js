
// ---- URLSearchParams ----------------------------------------------------------------------------

const decodeComponent = (s) => native.pctDecode(s.replace(/\+/g, ' '));

function parseQuery(text) {
  if (text.charAt(0) === '?') text = text.slice(1);
  const out = [];
  if (text === '') return out;
  for (const part of text.split('&')) {
    if (part === '') continue;
    const i = part.indexOf('=');
    out.push(i < 0 ? [decodeComponent(part), ''] : [decodeComponent(part.slice(0, i)), decodeComponent(part.slice(i + 1))]);
  }
  return out;
}

function serializeQuery(list) {
  let out = '';
  for (let i = 0; i < list.length; i++) {
    if (i > 0) out += '&';
    out += native.formEncode(list[i][0]) + '=' + native.formEncode(list[i][1]);
  }
  return out;
}

class URLSearchParams {
  constructor(init) {
    const s = setSlots(this, { brand: 'URLSearchParams', list: [], url: null });
    if (init === undefined) return;
    if (isObj(init)) {
      if (hasBrand(init, 'URLSearchParams')) {
        s.list = init[S].list.map((p) => [p[0], p[1]]);
      } else if (typeof init[SymIter] === 'function') {
        for (const pair of init) {
          if (!isObj(pair) || typeof pair[SymIter] !== 'function') {
            throw new TypeError("Failed to construct 'URLSearchParams': The provided value cannot be converted to a sequence.");
          }
          const items = Array.from(pair);
          if (items.length !== 2) {
            throw new TypeError("Failed to construct 'URLSearchParams': Each query pair must be an iterable [name, value] tuple");
          }
          s.list.push([str(items[0]), str(items[1])]);
        }
      } else {
        for (const k of Object.keys(init)) s.list.push([k, str(init[k])]);
      }
    } else {
      s.list = parseQuery(str(init));
    }
  }
  get size() {
    return slots(this, 'URLSearchParams').list.length;
  }
  append(name, value) {
    need(arguments, 2, 'append', 'URLSearchParams');
    const s = slots(this, 'URLSearchParams');
    s.list.push([str(name), str(value)]);
    updateUrl(s);
  }
  delete(name, value) {
    need(arguments, 1, 'delete', 'URLSearchParams');
    const s = slots(this, 'URLSearchParams');
    name = str(name);
    if (value !== undefined) {
      value = str(value);
      s.list = s.list.filter((p) => !(p[0] === name && p[1] === value));
    } else {
      s.list = s.list.filter((p) => p[0] !== name);
    }
    updateUrl(s);
  }
  get(name) {
    need(arguments, 1, 'get', 'URLSearchParams');
    name = str(name);
    for (const p of slots(this, 'URLSearchParams').list) if (p[0] === name) return p[1];
    return null;
  }
  getAll(name) {
    need(arguments, 1, 'getAll', 'URLSearchParams');
    name = str(name);
    const out = [];
    for (const p of slots(this, 'URLSearchParams').list) if (p[0] === name) out.push(p[1]);
    return out;
  }
  has(name, value) {
    need(arguments, 1, 'has', 'URLSearchParams');
    name = str(name);
    const v = value === undefined ? undefined : str(value);
    for (const p of slots(this, 'URLSearchParams').list) if (p[0] === name && (v === undefined || p[1] === v)) return true;
    return false;
  }
  set(name, value) {
    need(arguments, 2, 'set', 'URLSearchParams');
    const s = slots(this, 'URLSearchParams');
    name = str(name);
    value = str(value);
    let found = false;
    const out = [];
    for (const p of s.list) {
      if (p[0] !== name) out.push(p);
      else if (!found) {
        found = true;
        out.push([name, value]);
      }
    }
    if (!found) out.push([name, value]);
    s.list = out;
    updateUrl(s);
  }
  sort() {
    const s = slots(this, 'URLSearchParams');
    // Stable, by UTF-16 code units of the name.
    const idx = s.list.map((p, i) => [p, i]);
    idx.sort((a, b) => (a[0][0] < b[0][0] ? -1 : a[0][0] > b[0][0] ? 1 : a[1] - b[1]));
    s.list = idx.map((x) => x[0]);
    updateUrl(s);
  }
  toString() {
    return serializeQuery(slots(this, 'URLSearchParams').list);
  }
  forEach(callback, thisArg) {
    need(arguments, 1, 'forEach', 'URLSearchParams');
    if (typeof callback !== 'function') throw new TypeError("Failed to execute 'forEach' on 'URLSearchParams': parameter 1 is not of type 'Function'.");
    const s = slots(this, 'URLSearchParams');
    for (let i = 0; i < s.list.length; i++) callback.call(thisArg, s.list[i][1], s.list[i][0], this);
  }
  *entries() {
    const s = slots(this, 'URLSearchParams');
    for (let i = 0; i < s.list.length; i++) yield [s.list[i][0], s.list[i][1]];
  }
  *keys() {
    const s = slots(this, 'URLSearchParams');
    for (let i = 0; i < s.list.length; i++) yield s.list[i][0];
  }
  *values() {
    const s = slots(this, 'URLSearchParams');
    for (let i = 0; i < s.list.length; i++) yield s.list[i][1];
  }
}
URLSearchParams.prototype[SymIter] = URLSearchParams.prototype.entries;
tag(URLSearchParams);
enumerable(URLSearchParams, ['append', 'delete', 'get', 'getAll', 'has', 'set', 'sort', 'toString', 'forEach', 'entries', 'keys', 'values', 'size']);

function updateUrl(s) {
  if (!s.url) return;
  const u = s.url[S];
  const q = serializeQuery(s.list);
  u.rec = reparse(u.rec, { search: q === '' ? '' : '?' + q }) || u.rec;
}

// ---- URL ------------------------------------------------------------------------------------------

function parseURL(text, base) {
  return native.parseUrl(text, base);
}

// The address with some parts replaced, parsed again (which escapes and normalizes); null if the
// result is not an address.
function reparse(rec, patch) {
  const r = Object.assign({}, rec, patch);
  let s = r.protocol;
  if (r.authority) {
    s += '//';
    if (r.username !== '' || r.password !== '') {
      s += r.username;
      if (r.password !== '') s += ':' + r.password;
      s += '@';
    }
    s += r.host;
  }
  s += r.pathname + r.search + r.hash;
  return parseURL(s);
}

class URL {
  constructor(url, base) {
    if (arguments.length === 0) throw new TypeError("Failed to construct 'URL': 1 argument required, but only 0 present.");
    const rec = parseURL(str(url), base === undefined ? undefined : str(base));
    if (!rec) throw new TypeError("Failed to construct 'URL': Invalid URL");
    setSlots(this, { brand: 'URL', rec, params: null });
  }
  static canParse(url, base) {
    need(arguments, 1, 'canParse', 'URL');
    return !!parseURL(str(url), base === undefined ? undefined : str(base));
  }
  static parse(url, base) {
    need(arguments, 1, 'parse', 'URL');
    const rec = parseURL(str(url), base === undefined ? undefined : str(base));
    if (!rec) return null;
    const u = Object.create(URL.prototype);
    setSlots(u, { brand: 'URL', rec, params: null });
    return u;
  }
  static createObjectURL(obj) {
    need(arguments, 1, 'createObjectURL', 'URL');
    if (!hasBrand(obj, 'Blob') && !hasBrand(obj, 'File')) {
      throw new TypeError("Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.");
    }
    let id = '';
    for (let i = 0; i < 32; i++) {
      id += '0123456789abcdef'.charAt(Math.floor(Math.random() * 16));
      if (i === 7 || i === 11 || i === 15 || i === 19) id += '-';
    }
    const origin = parseURL(native.location()).origin;
    const u = 'blob:' + (origin === 'null' ? '' : origin) + '/' + id;
    blobUrls.set(u, obj);
    return u;
  }
  static revokeObjectURL(url) {
    need(arguments, 1, 'revokeObjectURL', 'URL');
    blobUrls.delete(str(url));
  }
  get href() {
    return slots(this, 'URL').rec.href;
  }
  set href(v) {
    const s = slots(this, 'URL');
    const rec = parseURL(str(v));
    if (!rec) throw new TypeError("Failed to set the 'href' property on 'URL': Invalid URL");
    s.rec = rec;
    if (s.params) s.params[S].list = parseQuery(rec.search);
  }
  get origin() {
    return slots(this, 'URL').rec.origin;
  }
  get protocol() {
    return slots(this, 'URL').rec.protocol;
  }
  set protocol(v) {
    const s = slots(this, 'URL');
    const scheme = str(v).split(':')[0].toLowerCase();
    if (!/^[a-z][a-z0-9+.\-]*$/.test(scheme)) return;
    const special = (x) => /^(https?|wss?|ftp|file)$/.test(x);
    if (special(scheme) !== special(s.rec.protocol.slice(0, -1))) return;
    const rec = reparse(s.rec, { protocol: scheme + ':' });
    if (rec) s.rec = rec;
  }
  get username() {
    return slots(this, 'URL').rec.username;
  }
  set username(v) {
    const s = slots(this, 'URL');
    if (!s.rec.authority || s.rec.hostname === '') return;
    const rec = reparse(s.rec, { username: str(v) });
    if (rec) s.rec = rec;
  }
  get password() {
    return slots(this, 'URL').rec.password;
  }
  set password(v) {
    const s = slots(this, 'URL');
    if (!s.rec.authority || s.rec.hostname === '') return;
    const rec = reparse(s.rec, { password: str(v) });
    if (rec) s.rec = rec;
  }
  get host() {
    return slots(this, 'URL').rec.host;
  }
  set host(v) {
    const s = slots(this, 'URL');
    if (!s.rec.authority) return;
    const rec = reparse(s.rec, { host: str(v).split(/[/?#\\]/)[0] });
    if (rec) s.rec = rec;
  }
  get hostname() {
    return slots(this, 'URL').rec.hostname;
  }
  set hostname(v) {
    const s = slots(this, 'URL');
    if (!s.rec.authority) return;
    const h = str(v).split(/[/?#\\:]/)[0];
    const rec = reparse(s.rec, { host: h + (s.rec.port !== '' ? ':' + s.rec.port : '') });
    if (rec) s.rec = rec;
  }
  get port() {
    return slots(this, 'URL').rec.port;
  }
  set port(v) {
    const s = slots(this, 'URL');
    if (!s.rec.authority || s.rec.hostname === '' || s.rec.protocol === 'file:') return;
    v = str(v);
    let digits = /^[0-9]*/.exec(v)[0];
    if (digits === '' && v !== '') return;
    const rec = reparse(s.rec, { host: s.rec.hostname + (digits === '' ? '' : ':' + digits) });
    if (rec) s.rec = rec;
  }
  get pathname() {
    return slots(this, 'URL').rec.pathname;
  }
  set pathname(v) {
    const s = slots(this, 'URL');
    if (!s.rec.authority && !s.rec.pathname.startsWith('/')) return;
    v = str(v);
    if (!v.startsWith('/')) v = '/' + v;
    // A "?" or "#" in the new path is part of the path, not a delimiter.
    v = v.replace(/\?/g, '%3F').replace(/#/g, '%23');
    const rec = reparse(s.rec, { pathname: v });
    if (rec) s.rec = rec;
  }
  get search() {
    return slots(this, 'URL').rec.search;
  }
  set search(v) {
    const s = slots(this, 'URL');
    v = str(v);
    if (v !== '' && v.charAt(0) !== '?') v = '?' + v;
    if (v === '?') v = '';
    const rec = reparse(s.rec, { search: v.replace(/#/g, '%23') });
    if (rec) {
      s.rec = rec;
      if (s.params) s.params[S].list = parseQuery(rec.search);
    }
  }
  get searchParams() {
    const s = slots(this, 'URL');
    if (!s.params) {
      s.params = new URLSearchParams(s.rec.search);
      s.params[S].url = this;
    }
    return s.params;
  }
  get hash() {
    return slots(this, 'URL').rec.hash;
  }
  set hash(v) {
    const s = slots(this, 'URL');
    v = str(v);
    if (v !== '' && v.charAt(0) !== '#') v = '#' + v;
    if (v === '#') v = '';
    const rec = reparse(s.rec, { hash: v });
    if (rec) s.rec = rec;
  }
  toString() {
    return slots(this, 'URL').rec.href;
  }
  toJSON() {
    return slots(this, 'URL').rec.href;
  }
}
tag(URL);
enumerable(URL, ['href', 'origin', 'protocol', 'username', 'password', 'host', 'hostname', 'port', 'pathname', 'search', 'searchParams', 'hash', 'toString', 'toJSON']);

// blob: addresses made by URL.createObjectURL, until revoked.
const blobUrls = new Map();

// An absolute address for `text` against the page, as a record (null if it is not one).
const pageBase = () => native.location();
function resolveUrl(text) {
  return parseURL(text, pageBase());
}
