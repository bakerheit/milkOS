
// ---- AbortController, AbortSignal -------------------------------------------------------------------

class AbortSignal extends NetEventTarget {
  constructor(token) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    super();
    setSlots(this, { brand: 'AbortSignal', aborted: false, reason: undefined, dependents: [] });
  }
  get aborted() {
    return slots(this, 'AbortSignal').aborted;
  }
  get reason() {
    return slots(this, 'AbortSignal').reason;
  }
  throwIfAborted() {
    const s = slots(this, 'AbortSignal');
    if (s.aborted) throw s.reason;
  }
  static abort(reason) {
    const sig = new AbortSignal(TOKEN);
    signalAbort(sig, reason);
    return sig;
  }
  static timeout(ms) {
    need(arguments, 1, 'timeout', 'AbortSignal');
    ms = Number(ms);
    if (!(ms >= 0) || ms > 4294967295) throw new TypeError("Failed to execute 'timeout' on 'AbortSignal': Value is outside the 'unsigned long long' value range.");
    const sig = new AbortSignal(TOKEN);
    native.setTimer(() => signalAbort(sig, new DOMException('signal timed out', 'TimeoutError')), ms);
    return sig;
  }
  static any(signals) {
    need(arguments, 1, 'any', 'AbortSignal');
    const list = Array.from(signals);
    const sig = new AbortSignal(TOKEN);
    for (const x of list) {
      if (!hasBrand(x, 'AbortSignal')) throw new TypeError("Failed to execute 'any' on 'AbortSignal': Failed to convert value to 'AbortSignal'.");
    }
    for (const x of list) {
      if (x[S].aborted) {
        signalAbort(sig, x[S].reason);
        return sig;
      }
    }
    for (const x of list) x[S].dependents.push(sig);
    return sig;
  }
}
handlerAttrs(AbortSignal, ['abort']);
tag(AbortSignal);
enumerable(AbortSignal, ['aborted', 'reason', 'throwIfAborted']);

function signalAbort(sig, reason) {
  const s = sig[S];
  if (s.aborted) return;
  s.aborted = true;
  s.reason = reason === undefined ? new DOMException('signal is aborted without reason', 'AbortError') : reason;
  const deps = s.dependents;
  s.dependents = [];
  emitPlain(sig, 'abort');
  for (const d of deps) signalAbort(d, s.reason);
}

class AbortController {
  constructor() {
    setSlots(this, { brand: 'AbortController', signal: new AbortSignal(TOKEN) });
  }
  get signal() {
    return slots(this, 'AbortController').signal;
  }
  abort(reason) {
    signalAbort(slots(this, 'AbortController').signal, reason);
  }
}
tag(AbortController);
enumerable(AbortController, ['signal', 'abort']);

// A signal that follows `source` (null: never aborts on its own).
function followSignal(source) {
  const sig = new AbortSignal(TOKEN);
  if (source) {
    if (source[S].aborted) signalAbort(sig, source[S].reason);
    else source[S].dependents.push(sig);
  }
  return sig;
}

// ---- Request --------------------------------------------------------------------------------------------

const MODES = ['same-origin', 'no-cors', 'cors', 'navigate'];
const CREDENTIALS = ['omit', 'same-origin', 'include'];
const CACHES = ['default', 'no-store', 'reload', 'no-cache', 'force-cache', 'only-if-cached'];
const REDIRECTS = ['follow', 'error', 'manual'];
const REFERRER_POLICIES = [
  '', 'no-referrer', 'no-referrer-when-downgrade', 'same-origin', 'origin', 'strict-origin',
  'origin-when-cross-origin', 'strict-origin-when-cross-origin', 'unsafe-url',
];
const NORMAL_METHODS = ['DELETE', 'GET', 'HEAD', 'OPTIONS', 'POST', 'PUT'];

function enumMember(value, list, what, member) {
  value = str(value);
  if (list.indexOf(value) < 0) {
    throw new TypeError("Failed to construct '" + what + "': The provided value '" + value + "' is not a valid enum value of type " + member + '.');
  }
  return value;
}

function normalizeMethod(m, what) {
  m = str(m);
  if (!isToken(m)) throw new TypeError("Failed to " + what + ": '" + m + "' is not a valid HTTP method.");
  const up = m.toUpperCase();
  if (FORBIDDEN_METHODS.has(m.toLowerCase())) throw new TypeError("Failed to " + what + ": '" + m + "' HTTP method is unsupported.");
  return NORMAL_METHODS.indexOf(up) >= 0 ? up : m;
}

function pageOrigin() {
  return parseURL(native.location()).origin;
}

class Request {
  constructor(input, init) {
    need(arguments, 1, 'Request', 'Request');
    init = init === undefined || init === null ? {} : init;
    if (!isObj(init)) throw new TypeError("Failed to construct 'Request': The provided value is not of type 'RequestInit'.");
    const ctx = 'construct \'Request\'';
    let r;
    let inputBody = null;
    let inputSignal = null;
    if (hasBrand(input, 'Request')) {
      const o = input[S];
      r = {
        url: o.url, method: o.method, mode: o.mode, credentials: o.credentials, cache: o.cache, redirect: o.redirect,
        referrer: o.referrer, referrerPolicy: o.referrerPolicy, integrity: o.integrity, keepalive: o.keepalive,
        headers: o.headers,
      };
      inputBody = o.body;
      inputSignal = o.signal;
    } else {
      const rec = resolveUrl(str(input));
      if (!rec) throw new TypeError("Failed to construct 'Request': Failed to parse URL from " + str(input));
      if (rec.username !== '' || rec.password !== '') {
        throw new TypeError("Failed to construct 'Request': Request cannot be constructed from a URL that includes credentials: " + str(input));
      }
      r = {
        url: rec.href, method: 'GET', mode: 'cors', credentials: 'same-origin', cache: 'default', redirect: 'follow',
        referrer: 'about:client', referrerPolicy: '', integrity: '', keepalive: false, headers: null,
      };
    }
    if (init.referrer !== undefined) {
      const ref = str(init.referrer);
      if (ref === '') r.referrer = '';
      else {
        const rec = resolveUrl(ref);
        if (!rec) throw new TypeError("Failed to construct 'Request': Referrer '" + ref + "' is not a valid URL.");
        r.referrer = rec.protocol === 'about:' && rec.pathname === 'client' || rec.origin !== pageOrigin() ? 'about:client' : rec.href;
      }
    }
    if (init.referrerPolicy !== undefined) r.referrerPolicy = enumMember(init.referrerPolicy, REFERRER_POLICIES, 'Request', 'ReferrerPolicy');
    if (init.mode !== undefined) {
      const m = enumMember(init.mode, MODES, 'Request', 'RequestMode');
      if (m === 'navigate') throw new TypeError("Failed to construct 'Request': Cannot construct a Request with a RequestInit whose mode member is set as 'navigate'.");
      r.mode = m;
    }
    if (init.credentials !== undefined) r.credentials = enumMember(init.credentials, CREDENTIALS, 'Request', 'RequestCredentials');
    if (init.cache !== undefined) r.cache = enumMember(init.cache, CACHES, 'Request', 'RequestCache');
    if (r.cache === 'only-if-cached' && r.mode !== 'same-origin') {
      throw new TypeError("Failed to construct 'Request': 'only-if-cached' can be set only with 'same-origin' mode.");
    }
    if (init.redirect !== undefined) r.redirect = enumMember(init.redirect, REDIRECTS, 'Request', 'RequestRedirect');
    if (init.integrity !== undefined) r.integrity = str(init.integrity);
    if (init.keepalive !== undefined) r.keepalive = !!init.keepalive;
    if (init.method !== undefined) r.method = normalizeMethod(init.method, ctx);
    if (r.mode === 'no-cors' && r.method !== 'GET' && r.method !== 'HEAD' && r.method !== 'POST') {
      throw new TypeError("Failed to construct 'Request': '" + r.method + "' is unsupported in no-cors mode.");
    }
    let signal = inputSignal;
    if (init.signal !== undefined && init.signal !== null) {
      if (!hasBrand(init.signal, 'AbortSignal')) throw new TypeError("Failed to construct 'Request': Failed to convert value to 'AbortSignal'.");
      signal = init.signal;
    } else if (init.signal === null) {
      signal = null;
    }
    // Headers: a fresh list under the guard this mode needs.
    const h = makeHeaders(r.mode === 'no-cors' ? 'request-no-cors' : 'request');
    const hs = h[S];
    let source = init.headers !== undefined ? init.headers : r.headers;
    if (source !== null && source !== undefined) {
      if (hasBrand(source, 'Headers')) {
        for (const p of source[S].list) appendHeader(hs, p[0], p[1]);
      } else {
        fillHeaders(h, hs, source);
      }
    }
    // The body.
    let body = newBody(null);
    const initBody = init.body;
    if (initBody !== undefined && initBody !== null) {
      if (r.method === 'GET' || r.method === 'HEAD') throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      const ex = extractBody(initBody);
      if (ex.stream) throw new TypeError("Failed to construct 'Request': ReadableStream request bodies aren't supported.");
      body = newBody(ex);
      if (ex.type !== null && hdrGet(hs.list, 'content-type') === null) appendHeader(hs, 'content-type', ex.type);
    } else if (inputBody !== null && (inputBody.src || inputBody.stream)) {
      if (r.method === 'GET' || r.method === 'HEAD') throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      if (inputBody.used) throw new TypeError("Failed to construct 'Request': Cannot construct a Request with a Request object that has already been used.");
      inputBody.used = true;
      body = { src: inputBody.src, stream: null, used: false, view: null };
    }
    r.headers = h;
    r.body = body;
    r.signal = followSignal(signal);
    r.brand = 'Request';
    setSlots(this, r);
  }
  get method() {
    return slots(this, 'Request').method;
  }
  get url() {
    return slots(this, 'Request').url;
  }
  get headers() {
    return slots(this, 'Request').headers;
  }
  get destination() {
    return '';
  }
  get referrer() {
    return slots(this, 'Request').referrer;
  }
  get referrerPolicy() {
    return slots(this, 'Request').referrerPolicy;
  }
  get mode() {
    return slots(this, 'Request').mode;
  }
  get credentials() {
    return slots(this, 'Request').credentials;
  }
  get cache() {
    return slots(this, 'Request').cache;
  }
  get redirect() {
    return slots(this, 'Request').redirect;
  }
  get integrity() {
    return slots(this, 'Request').integrity;
  }
  get keepalive() {
    return slots(this, 'Request').keepalive;
  }
  get isReloadNavigation() {
    return false;
  }
  get isHistoryNavigation() {
    return false;
  }
  get signal() {
    return slots(this, 'Request').signal;
  }
  get duplex() {
    return 'half';
  }
  clone() {
    const s = slots(this, 'Request');
    if (s.body.used || (s.body.stream && s.body.stream[S].locked)) throw new TypeError("Failed to execute 'clone' on 'Request': Request object is already used");
    const c = Object.create(Request.prototype);
    const h = makeHeaders(s.headers[S].guard);
    h[S].list = s.headers[S].list.map((p) => [p[0], p[1]]);
    setSlots(c, Object.assign({}, s, {
      headers: h,
      body: { src: s.body.src, stream: null, used: false, view: null },
      signal: followSignal(s.signal),
    }));
    return c;
  }
}
tag(Request);
bodyMixin(Request, 'Request');
enumerable(Request, ['method', 'url', 'headers', 'destination', 'referrer', 'referrerPolicy', 'mode', 'credentials', 'cache', 'redirect', 'integrity', 'keepalive', 'isReloadNavigation', 'isHistoryNavigation', 'signal', 'duplex', 'clone']);

// ---- Response -------------------------------------------------------------------------------------------

const NULL_BODY_STATUS = [101, 204, 205, 304];
const REDIRECT_STATUS = [301, 302, 303, 307, 308];
const REASON_RE = /^[\t\x20-\x7e\x80-\xff]*$/;

function responseSlots(over) {
  return Object.assign({ brand: 'Response', type: 'default', url: '', redirected: false, status: 200, statusText: '', headers: null, body: newBody(null) }, over);
}

class Response {
  constructor(body, init) {
    init = init === undefined || init === null ? {} : init;
    if (!isObj(init)) throw new TypeError("Failed to construct 'Response': The provided value is not of type 'ResponseInit'.");
    let status = 200;
    if (init.status !== undefined) {
      status = Math.trunc(Number(init.status));
      if (!(status >= 200 && status <= 599)) {
        throw new RangeError("Failed to construct 'Response': The status provided (" + init.status + ') is outside the range [200, 599].');
      }
    }
    let statusText = '';
    if (init.statusText !== undefined) {
      statusText = str(init.statusText);
      if (!REASON_RE.test(statusText)) throw new TypeError("Failed to construct 'Response': Invalid statusText");
    }
    const h = makeHeaders('response');
    if (init.headers !== undefined && init.headers !== null) {
      if (hasBrand(init.headers, 'Headers')) for (const p of init.headers[S].list) appendHeader(h[S], p[0], p[1]);
      else fillHeaders(h, h[S], init.headers);
    }
    let b = newBody(null);
    if (body !== undefined && body !== null) {
      if (NULL_BODY_STATUS.indexOf(status) >= 0) throw new TypeError("Failed to construct 'Response': Response with null body status cannot have body");
      const ex = extractBody(body);
      b = newBody(ex);
      if (ex.type !== null && hdrGet(h[S].list, 'content-type') === null) appendHeader(h[S], 'content-type', ex.type);
    }
    setSlots(this, responseSlots({ status, statusText, headers: h, body: b }));
  }
  static error() {
    const r = Object.create(Response.prototype);
    const h = makeHeaders('immutable');
    setSlots(r, responseSlots({ type: 'error', status: 0, headers: h }));
    return r;
  }
  static json(data, init) {
    need(arguments, 1, 'json', 'Response');
    const text = JSON.stringify(data);
    if (text === undefined) throw new TypeError("Failed to execute 'json' on 'Response': The data is not JSON serializable");
    const r = new Response(text, init);
    if (!r.headers.has('content-type')) r.headers.set('content-type', 'application/json');
    else if (init === undefined || init === null || init.headers === undefined || !new Headers(init.headers).has('content-type')) {
      r.headers.set('content-type', 'application/json');
    }
    return r;
  }
  static redirect(url, status) {
    need(arguments, 1, 'redirect', 'Response');
    const rec = resolveUrl(str(url));
    if (!rec) throw new TypeError("Failed to execute 'redirect' on 'Response': Invalid URL");
    status = status === undefined ? 302 : Math.trunc(Number(status));
    if (REDIRECT_STATUS.indexOf(status) < 0) throw new RangeError("Failed to execute 'redirect' on 'Response': Invalid status code");
    const r = Object.create(Response.prototype);
    const h = makeHeaders('response', [['location', rec.href]]);
    h[S].guard = 'immutable';
    setSlots(r, responseSlots({ status, headers: h }));
    return r;
  }
  get type() {
    return slots(this, 'Response').type;
  }
  get url() {
    return slots(this, 'Response').url;
  }
  get redirected() {
    return slots(this, 'Response').redirected;
  }
  get status() {
    return slots(this, 'Response').status;
  }
  get ok() {
    const st = slots(this, 'Response').status;
    return st >= 200 && st <= 299;
  }
  get statusText() {
    return slots(this, 'Response').statusText;
  }
  get headers() {
    return slots(this, 'Response').headers;
  }
  clone() {
    const s = slots(this, 'Response');
    if (s.body.used || (s.body.stream && s.body.stream[S].locked)) throw new TypeError("Failed to execute 'clone' on 'Response': Response body is already used");
    const c = Object.create(Response.prototype);
    const h = makeHeaders(s.headers[S].guard);
    h[S].list = s.headers[S].list.map((p) => [p[0], p[1]]);
    setSlots(c, Object.assign({}, s, { headers: h, body: { src: s.body.src, stream: null, used: false, view: null } }));
    return c;
  }
}
tag(Response);
bodyMixin(Response, 'Response');
enumerable(Response, ['type', 'url', 'redirected', 'status', 'ok', 'statusText', 'headers', 'clone']);

// ---- fetch ------------------------------------------------------------------------------------------------

function networkError(info) {
  const e = new TypeError('Failed to fetch');
  if (info && info.message) ObjDefine(e, 'cause', { value: new Error(info.message), writable: true, configurable: true });
  return e;
}

// The response for a head the app sent.
function responseFromHead(head, src) {
  const h = makeHeaders('response');
  if (head.type !== 'opaque' && head.type !== 'opaqueredirect') {
    for (const p of head.headers) {
      const lower = String(p[0]).toLowerCase();
      if (isToken(lower) && !FORBIDDEN_RESPONSE.has(lower)) h[S].list.push([lower, String(p[1])]);
    }
  }
  h[S].guard = 'immutable';
  const r = Object.create(Response.prototype);
  const opaque = head.type === 'opaque' || head.type === 'opaqueredirect';
  setSlots(r, responseSlots({
    type: head.type,
    url: opaque ? '' : head.url,
    redirected: !!head.redirected,
    status: opaque ? 0 : head.status,
    statusText: opaque ? '' : head.statusText,
    headers: h,
    body: opaque || NULL_BODY_STATUS.indexOf(head.status) >= 0 ? newBody(null) : { src, stream: null, used: false, view: null },
  }));
  return r;
}

// data: and blob: addresses are answered here, with no request.
function localResponse(req) {
  const rec = parseURL(req.url);
  if (!rec) return null;
  if (rec.protocol === 'data:') {
    const href = rec.href.replace(/#.*$/, '');
    const rest = href.slice(5);
    const comma = rest.indexOf(',');
    if (comma < 0) return Promise.reject(networkError({ message: 'Fetch of a data: address failed: no comma' }));
    let meta = rest.slice(0, comma);
    const payload = rest.slice(comma + 1);
    let bytes;
    const b64 = /;\s*base64\s*$/i.test(meta);
    if (b64) meta = meta.replace(/;\s*base64\s*$/i, '');
    try {
      if (b64) bytes = native.bytesFromBinary(G.atob(native.pctDecode(payload).replace(/[\t\n\f\r ]/g, '')));
      else bytes = dataBytes(payload);
    } catch (e) {
      return Promise.reject(networkError({ message: 'Fetch of a data: address failed: it is not valid' }));
    }
    const type = meta.trim() === '' ? 'text/plain;charset=US-ASCII' : meta.charAt(0) === ';' ? 'text/plain' + meta : meta;
    return Promise.resolve(syntheticResponse(req, bytes, type, href));
  }
  if (rec.protocol === 'blob:') {
    const blob = blobUrls.get(rec.href.replace(/#.*$/, ''));
    if (!blob) return Promise.reject(networkError({ message: 'Fetch of a blob: address failed: it was revoked or never made' }));
    return Promise.resolve(syntheticResponse(req, blob[S].bytes, blob[S].type, rec.href));
  }
  return null;
}

function dataBytes(rest) {
  const parts = [];
  const re = /((?:%[0-9a-fA-F]{2})+)/g;
  let last = 0;
  let m;
  while ((m = re.exec(rest))) {
    if (m.index > last) parts.push(native.bytesFromString(rest.slice(last, m.index)));
    parts.push(native.bytesFromBinary(m[1].replace(/%([0-9a-fA-F]{2})/g, (x, h) => String.fromCharCode(parseInt(h, 16)))));
    last = re.lastIndex;
  }
  if (last < rest.length) parts.push(native.bytesFromString(rest.slice(last)));
  return native.bytesConcat(parts);
}

function syntheticResponse(req, bytes, type, url) {
  const h = makeHeaders('response');
  if (type) h[S].list.push(['content-type', type]);
  h[S].list.push(['content-length', String(native.bytesLength(bytes))]);
  h[S].guard = 'immutable';
  const r = Object.create(Response.prototype);
  setSlots(r, responseSlots({
    type: 'basic', url, status: 200, statusText: 'OK', headers: h,
    body: req.method === 'HEAD' ? newBody(null) : { src: readySource(bytes), stream: null, used: false, view: null },
  }));
  return r;
}

function fetch(input, init) {
  return new Promise((resolve, reject) => {
    need(arguments, 1, 'fetch', 'Window');
    const request = new Request(input, init);
    const s = request[S];
    const signal = s.signal;
    if (signal[S].aborted) {
      reject(signal[S].reason);
      return;
    }
    const local = localResponse(s);
    if (local !== null) {
      local.then(resolve, reject);
      return;
    }
    const body = s.body.src ? s.body.src.bytes : null;
    s.body.used = s.body.used || body !== null;
    let response = null;
    let src = null;
    let finished = false;
    const onAbort = () => {
      if (finished) return;
      finished = true;
      native.cancel(id);
      const reason = signal[S].reason;
      if (response) srcFail(src, reason);
      else reject(reason);
    };
    const finish = () => {
      finished = true;
      signal.removeEventListener('abort', onAbort);
    };
    const id = native.start({
      kind: 'fetch',
      url: s.url,
      method: s.method,
      headers: headersList(s.headers),
      body,
      mode: s.mode,
      credentials: s.credentials,
      redirect: s.redirect,
      referrer: s.referrer,
      referrerPolicy: s.referrerPolicy,
      cache: s.cache,
    }, (kind, data) => {
      if (finished) return;
      switch (kind) {
        case 'head':
          src = pendingSource();
          response = responseFromHead(data, src);
          resolve(response);
          break;
        case 'done':
          finish();
          if (src) srcFinish(src, data);
          break;
        case 'failed':
          finish();
          if (response) srcFail(src, new TypeError('network error'));
          else reject(networkError(data));
          break;
        default:
          break;
      }
    });
    if (!id) {
      reject(new TypeError('Failed to fetch'));
      return;
    }
    signal.addEventListener('abort', onAbort);
  });
}

// ---- navigator.sendBeacon ----------------------------------------------------------------------------------

function sendBeacon(url, data) {
  need(arguments, 1, 'sendBeacon', 'Navigator');
  const rec = resolveUrl(str(url));
  if (!rec) throw new TypeError("Failed to execute 'sendBeacon' on 'Navigator': Invalid URL '" + str(url) + "'.");
  if (rec.protocol !== 'http:' && rec.protocol !== 'https:') {
    throw new TypeError("Failed to execute 'sendBeacon' on 'Navigator': Beacons are only supported over HTTP(S).");
  }
  const headers = [];
  let bytes = null;
  if (data !== undefined && data !== null) {
    const ex = extractBody(data);
    if (ex.stream) throw new TypeError("Failed to execute 'sendBeacon' on 'Navigator': ReadableStream isn't supported.");
    bytes = ex.src.bytes;
    if (ex.type !== null) headers.push(['content-type', ex.type]);
  }
  if (bytes !== null && native.bytesLength(bytes) > 65536) return false;
  const id = native.start({
    kind: 'beacon', url: rec.href, method: 'POST', headers, body: bytes, mode: 'cors', credentials: 'include',
    redirect: 'follow', referrer: 'about:client', referrerPolicy: '', cache: 'default',
  }, () => {});
  return id !== 0;
}
