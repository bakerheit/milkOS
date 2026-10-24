
// ---- small platform pieces ----------------------------------------------------------------------------
// crypto, idle callbacks, the performance timeline, matchMedia, CSS, DOMRect and friends,
// navigator's objects, animations, reportError.

// ---- crypto ----

const INT_ARRAYS = ['Int8Array', 'Uint8Array', 'Uint8ClampedArray', 'Int16Array', 'Uint16Array', 'Int32Array', 'Uint32Array'];

function randomFill(bytes) {
  if (!native.random(bytes)) throw domError('NotSupportedError', 'There is no source of random numbers.');
}

class Crypto {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  // (A secure context has `subtle`; a page here is not one.)
  get subtle() {
    return undefined;
  }
  getRandomValues(array) {
    need(arguments, 1, 'getRandomValues', 'Crypto');
    const cls = isObj(array) ? Object.prototype.toString.call(array).slice(8, -1) : '';
    if (INT_ARRAYS.indexOf(cls) < 0) {
      throw domError('TypeMismatchError', "Failed to execute 'getRandomValues' on 'Crypto': The provided value is not of type '(Int8Array or Int16Array or Int32Array or Uint8Array or Uint16Array or Uint32Array or Uint8ClampedArray)'");
    }
    if (array.byteLength > 65536) {
      throw domError('QuotaExceededError', "Failed to execute 'getRandomValues' on 'Crypto': The ArrayBufferView's byte length (" + array.byteLength + ") exceeds the number of bytes of entropy available via this API (65536).");
    }
    randomFill(new Uint8Array(array.buffer, array.byteOffset, array.byteLength));
    return array;
  }
  randomUUID() {
    const b = new Uint8Array(16);
    randomFill(b);
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    const h = [];
    for (let i = 0; i < 16; i++) h.push((b[i] < 16 ? '0' : '') + b[i].toString(16));
    return h.slice(0, 4).join('') + '-' + h.slice(4, 6).join('') + '-' + h.slice(6, 8).join('') + '-' + h.slice(8, 10).join('') + '-' + h.slice(10).join('');
  }
}
tag(Crypto);
enumerable(Crypto, ['subtle', 'getRandomValues', 'randomUUID']);
const cryptoObject = Object.create(Crypto.prototype);

// ---- requestIdleCallback ----

class IdleDeadline {
  constructor(token, start, timeout) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    setSlots(this, { brand: 'IdleDeadline', start, timeout });
  }
  get didTimeout() {
    return slots(this, 'IdleDeadline').timeout;
  }
  timeRemaining() {
    return Math.max(0, 50 - (native.now() - slots(this, 'IdleDeadline').start));
  }
}
tag(IdleDeadline);
enumerable(IdleDeadline, ['didTimeout', 'timeRemaining']);

function requestIdleCallback(callback, options) {
  need(arguments, 1, 'requestIdleCallback', 'Window');
  if (typeof callback !== 'function') throw new TypeError("Failed to execute 'requestIdleCallback' on 'Window': The callback provided as parameter 1 is not a function.");
  const timeout = isObj(options) && options.timeout > 0 ? Number(options.timeout) : 0;
  const queued = native.now();
  return native.setTimer(() => {
    const start = native.now();
    try {
      callback(new IdleDeadline(TOKEN, start, timeout > 0 && start - queued >= timeout));
    } catch (e) {
      native.report(e);
    }
  }, 1);
}
function cancelIdleCallback(id) {
  native.clearTimer(id);
}

// ---- reportError ----

function reportError(e) {
  need(arguments, 1, 'reportError', 'Window');
  native.report(e);
}

// ---- the performance timeline ----

const perfEntries = [];
const perfObservers = [];

class PerformanceEntry {
  constructor(token, name, entryType, startTime, duration, detail) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    setSlots(this, { brand: 'PerformanceEntry', name, entryType, startTime, duration, detail });
  }
  get name() {
    return slots(this, 'PerformanceEntry').name;
  }
  get entryType() {
    return slots(this, 'PerformanceEntry').entryType;
  }
  get startTime() {
    return slots(this, 'PerformanceEntry').startTime;
  }
  get duration() {
    return slots(this, 'PerformanceEntry').duration;
  }
  toJSON() {
    const s = slots(this, 'PerformanceEntry');
    return { name: s.name, entryType: s.entryType, startTime: s.startTime, duration: s.duration };
  }
}
tag(PerformanceEntry);
enumerable(PerformanceEntry, ['name', 'entryType', 'startTime', 'duration', 'toJSON']);
class PerformanceMark extends PerformanceEntry {
  get detail() {
    return slots(this, 'PerformanceEntry').detail;
  }
}
tag(PerformanceMark);
class PerformanceMeasure extends PerformanceEntry {
  get detail() {
    return slots(this, 'PerformanceEntry').detail;
  }
}
tag(PerformanceMeasure);

function perfNow() {
  return native.now() - perfOrigin;
}
const perfOrigin = native.perfOrigin();

function perfAdd(entry) {
  perfEntries.push(entry);
  if (perfEntries.length > 10000) perfEntries.shift();
  for (const o of perfObservers) o.offer(entry);
}

function markTime(name) {
  for (let i = perfEntries.length - 1; i >= 0; i--) {
    if (perfEntries[i].entryType === 'mark' && perfEntries[i].name === name) return perfEntries[i].startTime;
  }
  throw domError('SyntaxError', "Failed to execute 'measure' on 'Performance': The mark '" + name + "' does not exist.");
}

function performanceMark(name, options) {
  need(arguments, 1, 'mark', 'Performance');
  name = str(name);
  let start = perfNow();
  let detail = null;
  if (isObj(options)) {
    if (options.startTime !== undefined) {
      start = Number(options.startTime);
      if (!isFinite(start) || start < 0) throw new TypeError("Failed to execute 'mark' on 'Performance': '" + options.startTime + "' is an invalid timestamp.");
    }
    if (options.detail !== undefined) detail = options.detail;
  }
  const e = new PerformanceMark(TOKEN, name, 'mark', start, 0, detail);
  perfAdd(e);
  return e;
}

function performanceMeasure(name, startOrOptions, endMark) {
  need(arguments, 1, 'measure', 'Performance');
  name = str(name);
  let start = 0;
  let end = perfNow();
  let detail = null;
  const point = (v) => (typeof v === 'number' ? v : markTime(str(v)));
  if (isObj(startOrOptions)) {
    if (startOrOptions.start !== undefined) start = point(startOrOptions.start);
    if (startOrOptions.end !== undefined) end = point(startOrOptions.end);
    if (startOrOptions.duration !== undefined) {
      const d = Number(startOrOptions.duration);
      if (startOrOptions.end !== undefined && startOrOptions.start === undefined) start = end - d;
      else end = start + d;
    }
    if (startOrOptions.detail !== undefined) detail = startOrOptions.detail;
  } else {
    if (startOrOptions !== undefined) start = point(startOrOptions);
    if (endMark !== undefined) end = point(endMark);
  }
  const e = new PerformanceMeasure(TOKEN, name, 'measure', start, end - start, detail);
  perfAdd(e);
  return e;
}

function clearEntries(type, name) {
  for (let i = perfEntries.length - 1; i >= 0; i--) {
    if (perfEntries[i].entryType === type && (name === undefined || perfEntries[i].name === name)) perfEntries.splice(i, 1);
  }
}

function navigationEntry() {
  const t = perfNow();
  return new PerformanceEntry(TOKEN, native.location(), 'navigation', 0, t, null);
}

function getEntries() {
  return perfEntries.slice().sort((a, b) => a.startTime - b.startTime);
}
function getEntriesByType(type) {
  type = str(type);
  if (type === 'navigation') return [navigationEntry()];
  return getEntries().filter((e) => e.entryType === type);
}
function getEntriesByName(name, type) {
  name = str(name);
  return getEntries().filter((e) => e.name === name && (type === undefined || e.entryType === type));
}

class PerformanceObserver {
  constructor(callback) {
    need(arguments, 1, 'PerformanceObserver', 'PerformanceObserver');
    if (typeof callback !== 'function') throw new TypeError("Failed to construct 'PerformanceObserver': The callback provided as parameter 1 is not a function.");
    setSlots(this, { brand: 'PerformanceObserver', cb: callback, types: [], queue: [], scheduled: false, registered: false });
  }
  observe(options) {
    const s = slots(this, 'PerformanceObserver');
    let types = [];
    if (isObj(options)) {
      if (options.entryTypes !== undefined) types = Array.from(options.entryTypes, str);
      else if (options.type !== undefined) types = [str(options.type)];
    }
    if (types.length === 0) throw new TypeError("Failed to execute 'observe' on 'PerformanceObserver': either an 'entryTypes' or 'type' member must be present.");
    s.types = types.filter((t) => t === 'mark' || t === 'measure');
    if (isObj(options) && options.buffered && options.type !== undefined) {
      for (const e of perfEntries) if (s.types.indexOf(e.entryType) >= 0) s.queue.push(e);
      if (s.queue.length) this[S].flush();
    }
    if (!s.registered) {
      s.registered = true;
      const self = this;
      s.offer = (e) => {
        if (s.types.indexOf(e.entryType) < 0) return;
        s.queue.push(e);
        s.flush();
      };
      s.flush = () => {
        if (s.scheduled) return;
        s.scheduled = true;
        nextTick(() => {
          s.scheduled = false;
          const list = s.queue;
          s.queue = [];
          if (!list.length) return;
          try {
            s.cb.call(self, makeEntryList(list), self);
          } catch (err) {
            native.report(err);
          }
        });
      };
      perfObservers.push(s);
    }
  }
  disconnect() {
    const s = slots(this, 'PerformanceObserver');
    s.types = [];
    s.queue = [];
    s.registered = false;
    const i = perfObservers.indexOf(s);
    if (i >= 0) perfObservers.splice(i, 1);
  }
  takeRecords() {
    const s = slots(this, 'PerformanceObserver');
    const q = s.queue;
    s.queue = [];
    return q;
  }
  static get supportedEntryTypes() {
    return ['mark', 'measure'];
  }
}
tag(PerformanceObserver);
enumerable(PerformanceObserver, ['observe', 'disconnect', 'takeRecords']);
function makeEntryList(list) {
  return {
    getEntries: () => list.slice(),
    getEntriesByType: (t) => list.filter((e) => e.entryType === t),
    getEntriesByName: (n, t) => list.filter((e) => e.name === n && (t === undefined || e.entryType === t)),
  };
}

// The legacy performance.timing and performance.navigation.
const perfTiming = (() => {
  const o = perfOrigin ? native.timeOrigin() : 0;
  const t = {};
  for (const k of ['navigationStart', 'fetchStart', 'requestStart', 'responseStart', 'responseEnd', 'domLoading', 'domInteractive', 'domContentLoadedEventStart', 'domContentLoadedEventEnd', 'domComplete', 'loadEventStart', 'loadEventEnd']) t[k] = Math.round(o);
  for (const k of ['unloadEventStart', 'unloadEventEnd', 'redirectStart', 'redirectEnd', 'domainLookupStart', 'domainLookupEnd', 'connectStart', 'connectEnd', 'secureConnectionStart']) t[k] = 0;
  t.toJSON = function () { return Object.assign({}, this); };
  return t;
})();
const perfNavigation = { type: 0, redirectCount: 0, TYPE_NAVIGATE: 0, TYPE_RELOAD: 1, TYPE_BACK_FORWARD: 2, TYPE_RESERVED: 255 };

// ---- matchMedia ----

class MediaQueryListEvent extends G.Event {
  constructor(type, init) {
    super(type, init);
    eventMembers(this, init, { media: '', matches: false });
  }
}
tag(MediaQueryListEvent);

class MediaQueryList extends NetEventTarget {
  constructor(token, query) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    super();
    setSlots(this, { brand: 'MediaQueryList', query });
  }
  get matches() {
    return native.mediaMatches(slots(this, 'MediaQueryList').query);
  }
  get media() {
    return slots(this, 'MediaQueryList').query;
  }
  addListener(cb) {
    if (cb !== null && cb !== undefined) this.addEventListener('change', cb);
  }
  removeListener(cb) {
    this.removeEventListener('change', cb);
  }
}
handlerAttrs(MediaQueryList, ['change']);
tag(MediaQueryList);
enumerable(MediaQueryList, ['matches', 'media', 'addListener', 'removeListener']);

function matchMedia(query) {
  need(arguments, 1, 'matchMedia', 'Window');
  return new MediaQueryList(TOKEN, str(query));
}

// ---- CSS ----

const CSS = {
  supports(a, b) {
    need(arguments, 1, 'supports', 'CSS');
    if (arguments.length >= 2) return native.cssSupportsDecl(str(a), str(b));
    return native.cssSupports(str(a));
  },
  escape(value) {
    need(arguments, 1, 'escape', 'CSS');
    const s = str(value);
    let out = '';
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === 0) out += '�';
      else if ((c >= 1 && c <= 31) || c === 127 || (i === 0 && c >= 48 && c <= 57) || (i === 1 && c >= 48 && c <= 57 && s.charCodeAt(0) === 45)) out += '\\' + c.toString(16) + ' ';
      else if (i === 0 && c === 45 && s.length === 1) out += '\\' + s.charAt(i);
      else if (c >= 128 || c === 45 || c === 95 || (c >= 48 && c <= 57) || (c >= 65 && c <= 90) || (c >= 97 && c <= 122)) out += s.charAt(i);
      else out += '\\' + s.charAt(i);
    }
    return out;
  },
  registerProperty() {},
  px: (v) => ({ value: v, unit: 'px', toString: () => v + 'px' }),
  em: (v) => ({ value: v, unit: 'em', toString: () => v + 'em' }),
  percent: (v) => ({ value: v, unit: 'percent', toString: () => v + '%' }),
};
ObjDefine(CSS, TAG, { value: 'CSS', configurable: true });
for (const k of ['supports', 'escape', 'registerProperty']) ObjDefine(CSS, k, { value: CSS[k], enumerable: true, writable: true, configurable: true });

// ---- DOMRect, DOMPoint, DOMMatrix ----

const rectProto = native.rectProto();
function rectFields(o, x, y, w, h) {
  ObjDefine(o, 'x', { value: x, writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'y', { value: y, writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'width', { value: w, writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'height', { value: h, writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'top', { value: Math.min(y, y + h), writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'left', { value: Math.min(x, x + w), writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'right', { value: Math.max(x, x + w), writable: true, enumerable: true, configurable: true });
  ObjDefine(o, 'bottom', { value: Math.max(y, y + h), writable: true, enumerable: true, configurable: true });
}
const num = (v) => {
  v = Number(v);
  return v;
};
function DOMRectReadOnly(x = 0, y = 0, width = 0, height = 0) {
  if (new.target === undefined) throw new TypeError("Failed to construct 'DOMRectReadOnly': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
  const o = Object.create(new.target.prototype);
  rectFields(o, num(x), num(y), num(width), num(height));
  return o;
}
function DOMRect(x = 0, y = 0, width = 0, height = 0) {
  if (new.target === undefined) throw new TypeError("Failed to construct 'DOMRect': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
  const o = Object.create(new.target.prototype);
  rectFields(o, num(x), num(y), num(width), num(height));
  return o;
}
DOMRectReadOnly.prototype = Object.create(Object.prototype, { constructor: { value: DOMRectReadOnly, writable: true, configurable: true } });
Object.setPrototypeOf(rectProto, DOMRectReadOnly.prototype);
DOMRect.prototype = rectProto;
ObjDefine(rectProto, 'constructor', { value: DOMRect, writable: true, configurable: true });
ObjDefine(DOMRectReadOnly.prototype, TAG, { value: 'DOMRectReadOnly', configurable: true });
ObjDefine(rectProto, TAG, { value: 'DOMRect', configurable: true });
ObjDefine(DOMRectReadOnly.prototype, 'toJSON', {
  value: function toJSON() {
    return { x: this.x, y: this.y, width: this.width, height: this.height, top: this.top, right: this.right, bottom: this.bottom, left: this.left };
  },
  writable: true, configurable: true,
});
ObjDefine(DOMRectReadOnly, 'fromRect', { value: (r = {}) => new DOMRectReadOnly(r.x, r.y, r.width, r.height), writable: true, configurable: true });
ObjDefine(DOMRect, 'fromRect', { value: (r = {}) => new DOMRect(r.x, r.y, r.width, r.height), writable: true, configurable: true });
Object.setPrototypeOf(DOMRect, DOMRectReadOnly);

class DOMPointReadOnly {
  constructor(x = 0, y = 0, z = 0, w = 1) {
    setSlots(this, { brand: 'DOMPoint', x: num(x), y: num(y), z: num(z), w: num(w) });
  }
  get x() { return slots(this, 'DOMPoint').x; }
  get y() { return slots(this, 'DOMPoint').y; }
  get z() { return slots(this, 'DOMPoint').z; }
  get w() { return slots(this, 'DOMPoint').w; }
  matrixTransform(m) {
    return new DOMPoint(...new DOMMatrixReadOnly(m).transformXYZW(this.x, this.y, this.z, this.w));
  }
  toJSON() { return { x: this.x, y: this.y, z: this.z, w: this.w }; }
  static fromPoint(p = {}) { return new this(p.x, p.y, p.z, p.w === undefined ? 1 : p.w); }
}
tag(DOMPointReadOnly);
class DOMPoint extends DOMPointReadOnly {
  get x() { return slots(this, 'DOMPoint').x; }
  set x(v) { slots(this, 'DOMPoint').x = num(v); }
  get y() { return slots(this, 'DOMPoint').y; }
  set y(v) { slots(this, 'DOMPoint').y = num(v); }
  get z() { return slots(this, 'DOMPoint').z; }
  set z(v) { slots(this, 'DOMPoint').z = num(v); }
  get w() { return slots(this, 'DOMPoint').w; }
  set w(v) { slots(this, 'DOMPoint').w = num(v); }
}
tag(DOMPoint);

// A 4x4 matrix kept as the sixteen numbers m11..m44 (column-major, as DOMMatrix does).
const MKEYS = ['m11', 'm12', 'm13', 'm14', 'm21', 'm22', 'm23', 'm24', 'm31', 'm32', 'm33', 'm34', 'm41', 'm42', 'm43', 'm44'];
const ALIAS = { a: 'm11', b: 'm12', c: 'm21', d: 'm22', e: 'm41', f: 'm42' };
function matrixFromInit(init) {
  const m = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  if (init === undefined) return m;
  if (typeof init === 'string') {
    init = init.trim();
    if (init === '' || init === 'none') return m;
    const parsed = parseTransform(init);
    if (!parsed) throw new SyntaxError("Failed to construct 'DOMMatrix': Failed to parse '" + init + "'.");
    return parsed;
  }
  if (isObj(init) && typeof init[SymIter] === 'function') {
    const a = Array.from(init, Number);
    if (a.length === 6) return [a[0], a[1], 0, 0, a[2], a[3], 0, 0, 0, 0, 1, 0, a[4], a[5], 0, 1];
    if (a.length === 16) return a;
    throw new TypeError("Failed to construct 'DOMMatrix': The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix.");
  }
  if (isObj(init)) {
    const r = m.slice();
    MKEYS.forEach((k, i) => {
      if (init[k] !== undefined) r[i] = Number(init[k]);
    });
    for (const k of Object.keys(ALIAS)) if (init[k] !== undefined) r[MKEYS.indexOf(ALIAS[k])] = Number(init[k]);
    return r;
  }
  return m;
}
function mul(a, b) {
  // a * b, both column-major 4x4
  const r = new Array(16).fill(0);
  for (let c = 0; c < 4; c++) for (let row = 0; row < 4; row++) {
    let s = 0;
    for (let k = 0; k < 4; k++) s += a[k * 4 + row] * b[c * 4 + k];
    r[c * 4 + row] = s;
  }
  return r;
}
function translateM(x, y, z) { const m = matrixFromInit(); m[12] = x; m[13] = y; m[14] = z; return m; }
function scaleM(x, y, z) { const m = matrixFromInit(); m[0] = x; m[5] = y; m[10] = z; return m; }
function rotateM(deg) {
  const r = (deg * Math.PI) / 180, c = Math.cos(r), s = Math.sin(r);
  const m = matrixFromInit(); m[0] = c; m[1] = s; m[4] = -s; m[5] = c; return m;
}
function parseTransform(text) {
  let m = matrixFromInit();
  const re = /([a-zA-Z0-9]+)\(([^)]*)\)/g;
  let found = false, x;
  while ((x = re.exec(text))) {
    found = true;
    const args = x[2].split(',').map((s) => parseFloat(s));
    const f = x[1].toLowerCase();
    let t;
    if (f === 'matrix' && args.length === 6) t = [args[0], args[1], 0, 0, args[2], args[3], 0, 0, 0, 0, 1, 0, args[4], args[5], 0, 1];
    else if (f === 'matrix3d' && args.length === 16) t = args;
    else if (f === 'translate') t = translateM(args[0] || 0, args[1] || 0, 0);
    else if (f === 'translatex') t = translateM(args[0] || 0, 0, 0);
    else if (f === 'translatey') t = translateM(0, args[0] || 0, 0);
    else if (f === 'scale') t = scaleM(args[0], args.length > 1 ? args[1] : args[0], 1);
    else if (f === 'scalex') t = scaleM(args[0], 1, 1);
    else if (f === 'scaley') t = scaleM(1, args[0], 1);
    else if (f === 'rotate') t = rotateM(args[0] || 0);
    else return null;
    if (t.some((v) => v !== v)) return null;
    m = mul(m, t);
  }
  return found ? m : null;
}
class DOMMatrixReadOnly {
  constructor(init) {
    setSlots(this, { brand: 'DOMMatrix', m: matrixFromInit(init) });
  }
  get is2D() { const m = slots(this, 'DOMMatrix').m; return m[2] === 0 && m[3] === 0 && m[6] === 0 && m[7] === 0 && m[8] === 0 && m[9] === 0 && m[10] === 1 && m[11] === 0 && m[14] === 0 && m[15] === 1; }
  get isIdentity() { const m = slots(this, 'DOMMatrix').m; return m.every((v, i) => v === (i % 5 === 0 ? 1 : 0)); }
  multiply(o) { return new DOMMatrix(mul(slots(this, 'DOMMatrix').m, new DOMMatrixReadOnly(o)[S].m)); }
  translate(x = 0, y = 0, z = 0) { return new DOMMatrix(mul(slots(this, 'DOMMatrix').m, translateM(num(x), num(y), num(z)))); }
  scale(x = 1, y, z = 1) { return new DOMMatrix(mul(slots(this, 'DOMMatrix').m, scaleM(num(x), y === undefined ? num(x) : num(y), num(z)))); }
  rotate(deg = 0) { return new DOMMatrix(mul(slots(this, 'DOMMatrix').m, rotateM(num(deg)))); }
  inverse() {
    const m = slots(this, 'DOMMatrix').m;
    const det = m[0] * m[5] - m[1] * m[4];
    if (!this.is2D || det === 0) return new DOMMatrix([NaN, NaN, NaN, NaN, NaN, NaN]);
    const a = m[5] / det, b = -m[1] / det, c = -m[4] / det, d = m[0] / det;
    const e = -(a * m[12] + c * m[13]), f = -(b * m[12] + d * m[13]);
    return new DOMMatrix([a, b, c, d, e, f]);
  }
  transformXYZW(x, y, z, w) {
    const m = slots(this, 'DOMMatrix').m;
    return [0, 1, 2, 3].map((r) => m[r] * x + m[4 + r] * y + m[8 + r] * z + m[12 + r] * w);
  }
  transformPoint(p = {}) {
    const [x, y, z, w] = this.transformXYZW(p.x || 0, p.y || 0, p.z || 0, p.w === undefined ? 1 : p.w);
    return new DOMPoint(x, y, z, w);
  }
  toFloat64Array() { return new Float64Array(slots(this, 'DOMMatrix').m); }
  toFloat32Array() { return new Float32Array(slots(this, 'DOMMatrix').m); }
  toJSON() { const o = {}; MKEYS.forEach((k, i) => (o[k] = slots(this, 'DOMMatrix').m[i])); for (const k of Object.keys(ALIAS)) o[k] = this[k]; o.is2D = this.is2D; o.isIdentity = this.isIdentity; return o; }
  toString() {
    const m = slots(this, 'DOMMatrix').m;
    if (this.is2D) return 'matrix(' + [m[0], m[1], m[4], m[5], m[12], m[13]].join(', ') + ')';
    return 'matrix3d(' + m.join(', ') + ')';
  }
  static fromMatrix(o) { return new this(o); }
}
MKEYS.forEach((k, i) => ObjDefine(DOMMatrixReadOnly.prototype, k, { get() { return slots(this, 'DOMMatrix').m[i]; }, enumerable: true, configurable: true }));
for (const k of Object.keys(ALIAS)) {
  const i = MKEYS.indexOf(ALIAS[k]);
  ObjDefine(DOMMatrixReadOnly.prototype, k, { get() { return slots(this, 'DOMMatrix').m[i]; }, enumerable: true, configurable: true });
}
tag(DOMMatrixReadOnly);
class DOMMatrix extends DOMMatrixReadOnly {}
MKEYS.forEach((k, i) => ObjDefine(DOMMatrix.prototype, k, { get() { return slots(this, 'DOMMatrix').m[i]; }, set(v) { slots(this, 'DOMMatrix').m[i] = num(v); }, enumerable: true, configurable: true }));
for (const k of Object.keys(ALIAS)) {
  const i = MKEYS.indexOf(ALIAS[k]);
  ObjDefine(DOMMatrix.prototype, k, { get() { return slots(this, 'DOMMatrix').m[i]; }, set(v) { slots(this, 'DOMMatrix').m[i] = num(v); }, enumerable: true, configurable: true });
}
tag(DOMMatrix);

// ---- navigator's objects ----

function resolved(v) {
  return Promise.resolve(v);
}
function rejected(name, msg) {
  return Promise.reject(domError(name, msg));
}

const clipboard = Object.create(Object.assign(Object.create(G.EventTarget.prototype), {
  writeText(text) {
    need(arguments, 1, 'writeText', 'Clipboard');
    text = str(text);
    return native.clipboardWrite(text) ? resolved(undefined) : rejected('NotAllowedError', 'Write permission denied.');
  },
  readText() {
    return rejected('NotAllowedError', 'Read permission denied.');
  },
  write() {
    return rejected('NotAllowedError', 'Write permission denied.');
  },
  read() {
    return rejected('NotAllowedError', 'Read permission denied.');
  },
}));
ObjDefine(Object.getPrototypeOf(clipboard), TAG, { value: 'Clipboard', configurable: true });

class PermissionStatus extends NetEventTarget {
  constructor(token, name, state) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    super();
    setSlots(this, { brand: 'PermissionStatus', name, state });
  }
  get name() { return slots(this, 'PermissionStatus').name; }
  get state() { return slots(this, 'PermissionStatus').state; }
}
handlerAttrs(PermissionStatus, ['change']);
tag(PermissionStatus);
enumerable(PermissionStatus, ['name', 'state']);
const PERMISSIONS = ['geolocation', 'notifications', 'push', 'midi', 'camera', 'microphone', 'speaker-selection', 'device-info', 'background-fetch', 'background-sync', 'bluetooth', 'persistent-storage', 'ambient-light-sensor', 'accelerometer', 'gyroscope', 'magnetometer', 'clipboard-read', 'clipboard-write', 'display-capture', 'nfc', 'screen-wake-lock', 'storage-access', 'window-management', 'local-fonts'];
const permissions = {
  query(desc) {
    need(arguments, 1, 'query', 'Permissions');
    if (!isObj(desc) || desc.name === undefined) return Promise.reject(new TypeError("Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': Required member is undefined."));
    const name = str(desc.name);
    if (PERMISSIONS.indexOf(name) < 0) return Promise.reject(new TypeError("Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': The provided value '" + name + "' is not a valid enum value of type PermissionName."));
    const state = name === 'notifications' || name === 'push' ? 'denied' : name === 'clipboard-write' ? 'prompt' : 'prompt';
    return resolved(new PermissionStatus(TOKEN, name, state));
  },
};
ObjDefine(permissions, TAG, { value: 'Permissions', configurable: true });

const connection = new (class NetworkInformation extends NetEventTarget {
  get effectiveType() { return '4g'; }
  get downlink() { return 10; }
  get rtt() { return 50; }
  get saveData() { return false; }
  get type() { return 'unknown'; }
})();

const PERMISSION_DENIED = 1;
const geolocation = {
  getCurrentPosition(success, error) {
    need(arguments, 1, 'getCurrentPosition', 'Geolocation');
    nextTick(() => {
      if (typeof error === 'function') {
        try {
          error({ code: PERMISSION_DENIED, message: 'User denied Geolocation', PERMISSION_DENIED: 1, POSITION_UNAVAILABLE: 2, TIMEOUT: 3 });
        } catch (e) {
          native.report(e);
        }
      }
    });
  },
  watchPosition(success, error) {
    geolocation.getCurrentPosition(success, error);
    return 1;
  },
  clearWatch() {},
};
ObjDefine(geolocation, TAG, { value: 'Geolocation', configurable: true });

const storageManager = {
  estimate: () => resolved({ quota: 128 << 20, usage: 0 }),
  persist: () => resolved(false),
  persisted: () => resolved(false),
};
ObjDefine(storageManager, TAG, { value: 'StorageManager', configurable: true });

function emptyList(tagName, itemFn) {
  const o = { length: 0, item: () => null, namedItem: () => null, refresh() {}, [SymIter]: function* () {} };
  ObjDefine(o, TAG, { value: tagName, configurable: true });
  return o;
}
const plugins = emptyList('PluginArray');
const mimeTypes = emptyList('MimeTypeArray');

// ---- screen.orientation, visualViewport ----

class ScreenOrientation extends NetEventTarget {
  constructor(token) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    super();
  }
  get type() { const v = native.viewport(); return v[0] >= v[1] ? 'landscape-primary' : 'portrait-primary'; }
  get angle() { return 0; }
  lock() { return rejected('NotSupportedError', 'screen.orientation.lock() is not available on this device.'); }
  unlock() {}
}
handlerAttrs(ScreenOrientation, ['change']);
tag(ScreenOrientation);
enumerable(ScreenOrientation, ['type', 'angle', 'lock', 'unlock']);
const screenOrientation = new ScreenOrientation(TOKEN);

class VisualViewport extends NetEventTarget {
  constructor(token) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    super();
  }
  get width() { return native.viewport()[0]; }
  get height() { return native.viewport()[1]; }
  get scale() { return 1; }
  get offsetLeft() { return 0; }
  get offsetTop() { return 0; }
  get pageLeft() { return G.scrollX; }
  get pageTop() { return G.scrollY; }
}
handlerAttrs(VisualViewport, ['resize', 'scroll']);
tag(VisualViewport);
enumerable(VisualViewport, ['width', 'height', 'scale', 'offsetLeft', 'offsetTop', 'pageLeft', 'pageTop']);
const visualViewport = new VisualViewport(TOKEN);

// ---- animations ----
// Nothing is drawn: an animation runs its clock, so `finished` resolves, `finish` and `cancel`
// events come, and a filled-forwards end state lands in the element's inline style.

const KEYFRAME_SPECIAL = ['offset', 'easing', 'composite'];
function keyframeList(kf) {
  if (kf === null || kf === undefined) return [];
  if (typeof kf[SymIter] === 'function') return Array.from(kf).filter(isObj);
  // { prop: [v0, v1, ...] }
  const out = [];
  for (const k of Object.keys(kf)) {
    const vals = Array.isArray(kf[k]) ? kf[k] : [kf[k]];
    vals.forEach((v, i) => {
      if (!out[i]) out[i] = {};
      out[i][k] = v;
    });
  }
  return out;
}
function camel(prop) {
  return prop.replace(/-([a-z])/g, (m, c) => c.toUpperCase());
}
class AnimationEffect {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
}
tag(AnimationEffect);
class KeyframeEffect {
  constructor(target, keyframes, options) {
    const t = {
      duration: 0, delay: 0, endDelay: 0, iterations: 1, fill: 'auto', direction: 'normal', easing: 'linear', iterationStart: 0,
    };
    if (typeof options === 'number') t.duration = options;
    else if (isObj(options)) for (const k of Object.keys(t)) if (options[k] !== undefined) t[k] = options[k];
    t.duration = t.duration === 'auto' ? 0 : Number(t.duration) || 0;
    t.iterations = Number(t.iterations);
    if (!(t.iterations >= 0)) t.iterations = 1;
    setSlots(this, { brand: 'KeyframeEffect', target, keyframes: keyframeList(keyframes), timing: t, id: isObj(options) && options.id !== undefined ? str(options.id) : '' });
  }
  get target() { return slots(this, 'KeyframeEffect').target; }
  getKeyframes() { return slots(this, 'KeyframeEffect').keyframes.slice(); }
  setKeyframes(k) { slots(this, 'KeyframeEffect').keyframes = keyframeList(k); }
  getTiming() { return Object.assign({}, slots(this, 'KeyframeEffect').timing); }
  getComputedTiming() {
    const t = slots(this, 'KeyframeEffect').timing;
    return Object.assign({ endTime: t.delay + t.duration * t.iterations + t.endDelay, activeDuration: t.duration * t.iterations }, t);
  }
  updateTiming(o) { Object.assign(slots(this, 'KeyframeEffect').timing, o); }
}
Object.setPrototypeOf(KeyframeEffect.prototype, AnimationEffect.prototype);
tag(KeyframeEffect);

class DocumentTimeline {
  constructor() {}
  get currentTime() { return perfNow(); }
}
tag(DocumentTimeline);
enumerable(DocumentTimeline, ['currentTime']);
const timeline = new DocumentTimeline();

const liveAnimations = new Set();
class Animation extends NetEventTarget {
  constructor(effect, tl) {
    super();
    const s = { brand: 'Animation', effect: effect === undefined ? null : effect, timeline: tl === undefined ? timeline : tl, state: 'idle', start: 0, hold: 0, rate: 1, id: '', timer: 0 };
    s.finished = newDeferred();
    s.ready = newDeferred();
    setSlots(this, s);
  }
  get effect() { return slots(this, 'Animation').effect; }
  get timeline() { return slots(this, 'Animation').timeline; }
  get id() { return slots(this, 'Animation').id; }
  set id(v) { slots(this, 'Animation').id = str(v); }
  get playState() { return slots(this, 'Animation').state; }
  get pending() { return false; }
  get replaceState() { return 'active'; }
  get playbackRate() { return slots(this, 'Animation').rate; }
  set playbackRate(v) { slots(this, 'Animation').rate = Number(v) || 1; }
  get finished() { return slots(this, 'Animation').finished.promise; }
  get ready() { return slots(this, 'Animation').ready.promise; }
  get startTime() { const s = slots(this, 'Animation'); return s.state === 'running' ? s.start : null; }
  get currentTime() {
    const s = slots(this, 'Animation');
    if (s.state === 'running') return Math.min((perfNow() - s.start) * s.rate, endTimeOf(s));
    return s.state === 'idle' ? null : s.hold;
  }
  set currentTime(v) {
    const s = slots(this, 'Animation');
    s.hold = Number(v);
    if (s.state === 'running') {
      s.start = perfNow() - s.hold / s.rate;
      schedule(this);
    }
  }
  play() {
    const s = slots(this, 'Animation');
    if (s.state === 'finished') { s.hold = 0; s.finished = newDeferred(); }
    if (s.state === 'running') return;
    if (s.state === 'idle') s.hold = 0;
    s.state = 'running';
    s.start = perfNow() - s.hold / s.rate;
    liveAnimations.add(this);
    s.ready.resolve(this);
    schedule(this);
  }
  pause() {
    const s = slots(this, 'Animation');
    if (s.state !== 'running') return;
    s.hold = this.currentTime;
    s.state = 'paused';
    native.clearTimer(s.timer);
  }
  finish() {
    const s = slots(this, 'Animation');
    if (s.state === 'finished') return;
    s.hold = endTimeOf(s);
    done(this);
  }
  cancel() {
    const s = slots(this, 'Animation');
    if (s.state === 'idle') return;
    native.clearTimer(s.timer);
    liveAnimations.delete(this);
    const wasFinished = s.state === 'finished';
    s.state = 'idle';
    s.hold = 0;
    if (!wasFinished) s.finished.reject(domError('AbortError', 'The user aborted a request.'));
    s.finished = newDeferred();
    s.finished.promise.catch(() => {});
    fire(this, new AnimationPlaybackEventLike('cancel', null, null));
  }
  reverse() { const s = slots(this, 'Animation'); s.rate = -s.rate || -1; this.play(); }
  updatePlaybackRate(r) { this.playbackRate = r; }
  persist() {}
  commitStyles() { applyEnd(slots(this, 'Animation')); }
}
handlerAttrs(Animation, ['finish', 'cancel', 'remove']);
tag(Animation);
enumerable(Animation, ['effect', 'timeline', 'id', 'playState', 'pending', 'playbackRate', 'finished', 'ready', 'startTime', 'currentTime', 'play', 'pause', 'finish', 'cancel', 'reverse', 'updatePlaybackRate', 'persist', 'commitStyles']);

function newDeferred() {
  const d = {};
  d.promise = new Promise((res, rej) => {
    d.resolve = res;
    d.reject = rej;
  });
  // (A rejection of `finished` that nobody listens to is not an unhandled one.)
  d.promise.catch(() => {});
  return d;
}
class AnimationPlaybackEventLike extends G.Event {
  constructor(type, currentTime, timelineTime) {
    super(type);
    eventMembers(this, {}, { currentTime, timelineTime });
  }
}
function endTimeOf(s) {
  const e = s.effect;
  if (!e) return 0;
  const t = slots(e, 'KeyframeEffect').timing;
  return t.delay + t.duration * t.iterations + t.endDelay;
}
function schedule(anim) {
  const s = slots(anim, 'Animation');
  native.clearTimer(s.timer);
  const end = endTimeOf(s);
  if (!isFinite(end)) return;
  const left = Math.max(0, (end - (perfNow() - s.start) * s.rate) / Math.abs(s.rate));
  s.timer = native.setTimer(() => {
    if (s.state === 'running') done(anim);
  }, Math.min(left, 2147483647));
}
function applyEnd(s) {
  const e = s.effect;
  if (!e) return;
  const k = slots(e, 'KeyframeEffect');
  const t = k.target;
  const last = k.keyframes[k.keyframes.length - 1];
  if (!last || !t || !t.style) return;
  for (const p of Object.keys(last)) {
    if (KEYFRAME_SPECIAL.indexOf(p) >= 0) continue;
    try { t.style[p === 'cssFloat' ? 'float' : camel(p)] = last[p]; } catch (err) {}
  }
}
function done(anim) {
  const s = slots(anim, 'Animation');
  native.clearTimer(s.timer);
  s.state = 'finished';
  s.hold = endTimeOf(s);
  const fill = s.effect ? slots(s.effect, 'KeyframeEffect').timing.fill : 'none';
  if (fill === 'forwards' || fill === 'both') applyEnd(s);
  s.finished.resolve(anim);
  liveAnimations.delete(anim);
  fire(anim, new AnimationPlaybackEventLike('finish', anim.currentTime, perfNow()));
}

function elementAnimate(keyframes, options) {
  need(arguments, 1, 'animate', 'Element');
  const effect = new KeyframeEffect(this, keyframes, options);
  const anim = new Animation(effect, timeline);
  if (isObj(options) && options.id !== undefined) anim.id = str(options.id);
  anim.play();
  return anim;
}
function elementGetAnimations() {
  const out = [];
  for (const a of liveAnimations) if (a.effect && a.effect.target === this) out.push(a);
  return out;
}
function documentGetAnimations() {
  return Array.from(liveAnimations);
}
