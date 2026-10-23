
// ---- bytes, Blob, File ------------------------------------------------------------------------------
//
// Bytes are opaque `Bytes` objects held by the host (script/netbytes.rs): immutable, shared freely
// (clone() and slice() copy nothing but the handle or the range), turned into an ArrayBuffer only
// when the page asks for one.

const EMPTY = native.bytesFromString('');

function asciiLowerType(t) {
  t = str(t);
  for (let i = 0; i < t.length; i++) {
    const c = t.charCodeAt(i);
    if (c < 0x20 || c > 0x7e) return '';
  }
  return t.toLowerCase();
}

// The bytes of one part given to new Blob([...]) or a body: a string, a Blob, a buffer.
function partBytes(p) {
  if (hasBrand(p, 'Blob')) return p[S].bytes;
  if (isObj(p)) {
    const b = native.bytesFromBuffer(p);
    if (b !== null) return b;
  }
  return native.bytesFromString(str(p));
}

function makeBlob(bytes, type) {
  const b = Object.create(Blob.prototype);
  setSlots(b, { brand: 'Blob', bytes, type });
  return b;
}

class Blob {
  constructor(parts, options) {
    const s = setSlots(this, { brand: 'Blob', bytes: EMPTY, type: '' });
    if (parts !== undefined) {
      if (!isObj(parts) || typeof parts[SymIter] !== 'function') {
        throw new TypeError("Failed to construct 'Blob': The provided value cannot be converted to a sequence.");
      }
      const list = [];
      for (const p of parts) list.push(partBytes(p));
      s.bytes = native.bytesConcat(list);
    }
    if (options !== undefined && options !== null) {
      if (!isObj(options)) throw new TypeError("Failed to construct 'Blob': The provided value is not of type 'BlobPropertyBag'.");
      if (options.type !== undefined) s.type = asciiLowerType(options.type);
    }
  }
  get size() {
    return native.bytesLength(slots(this, 'Blob').bytes);
  }
  get type() {
    return slots(this, 'Blob').type;
  }
  slice(start, end, contentType) {
    const s = slots(this, 'Blob');
    const size = native.bytesLength(s.bytes);
    const rel = (v, dflt) => {
      if (v === undefined) return dflt;
      v = Math.trunc(Number(v)) || 0;
      return v < 0 ? Math.max(size + v, 0) : Math.min(v, size);
    };
    const a = rel(start, 0);
    const b = rel(end, size);
    return makeBlob(native.bytesSlice(s.bytes, a, Math.max(a, b)), contentType === undefined ? '' : asciiLowerType(contentType));
  }
  text() {
    try {
      return Promise.resolve(native.bytesToString(slots(this, 'Blob').bytes));
    } catch (e) {
      return Promise.reject(e);
    }
  }
  arrayBuffer() {
    try {
      return Promise.resolve(native.bytesToArrayBuffer(slots(this, 'Blob').bytes));
    } catch (e) {
      return Promise.reject(e);
    }
  }
  bytes() {
    try {
      return Promise.resolve(new G.Uint8Array(native.bytesToArrayBuffer(slots(this, 'Blob').bytes)));
    } catch (e) {
      return Promise.reject(e);
    }
  }
  stream() {
    return streamOfBytes(slots(this, 'Blob').bytes);
  }
}
tag(Blob);
enumerable(Blob, ['size', 'type', 'slice', 'text', 'arrayBuffer', 'bytes', 'stream']);

class File extends Blob {
  constructor(bits, name, options) {
    if (arguments.length < 2) throw new TypeError("Failed to construct 'File': 2 arguments required, but only " + arguments.length + ' present.');
    super(bits, options);
    const f = this[S];
    f.brand = 'Blob';
    f.file = true;
    f.name = str(name);
    f.lastModified = options && options.lastModified !== undefined ? Math.trunc(Number(options.lastModified)) || 0 : Math.floor(native.time());
  }
  get name() {
    return slots(this, 'Blob').name;
  }
  get lastModified() {
    return slots(this, 'Blob').lastModified;
  }
  get webkitRelativePath() {
    return '';
  }
}
tag(File);
enumerable(File, ['name', 'lastModified', 'webkitRelativePath']);

function isFile(v) {
  return hasBrand(v, 'Blob') && v[S].file === true;
}

// ---- FileReader -------------------------------------------------------------------------------------

class FileReader extends NetEventTarget {
  constructor() {
    super();
    setSlots(this, { brand: 'FileReader', state: 0, result: null, error: null, token: 0 });
  }
  get readyState() {
    return slots(this, 'FileReader').state;
  }
  get result() {
    return slots(this, 'FileReader').result;
  }
  get error() {
    return slots(this, 'FileReader').error;
  }
  readAsArrayBuffer(blob) {
    startRead(this, blob, 'readAsArrayBuffer', (bytes) => native.bytesToArrayBuffer(bytes));
  }
  readAsBinaryString(blob) {
    startRead(this, blob, 'readAsBinaryString', (bytes) => native.bytesToBinary(bytes));
  }
  readAsText(blob, encoding) {
    startRead(this, blob, 'readAsText', (bytes) => native.bytesDecode(bytes, encoding === undefined ? 'utf-8' : str(encoding)));
  }
  readAsDataURL(blob) {
    startRead(this, blob, 'readAsDataURL', (bytes, type) => {
      return 'data:' + (type || 'application/octet-stream') + ';base64,' + G.btoa(native.bytesToBinary(bytes));
    });
  }
  abort() {
    const s = slots(this, 'FileReader');
    if (s.state !== 1) return;
    s.token++;
    s.state = 2;
    s.result = null;
    emit(this, 'abort', 0, 0);
    emit(this, 'loadend', 0, 0);
  }
}
constants(FileReader, { EMPTY: 0, LOADING: 1, DONE: 2 });
tag(FileReader);
handlerAttrs(FileReader, ['loadstart', 'progress', 'load', 'abort', 'error', 'loadend']);

function startRead(fr, blob, what, convert) {
  const s = slots(fr, 'FileReader');
  if (!hasBrand(blob, 'Blob')) throw new TypeError("Failed to execute '" + what + "' on 'FileReader': parameter 1 is not of type 'Blob'.");
  if (s.state === 1) throw domError('InvalidStateError', "Failed to execute '" + what + "' on 'FileReader': The object is already busy reading Blobs.");
  s.state = 1;
  s.result = null;
  const token = ++s.token;
  const size = native.bytesLength(blob[S].bytes);
  nextTick(() => {
    if (s.token !== token) return;
    emit(fr, 'loadstart', 0, size);
    nextTick(() => {
      if (s.token !== token) return;
      try {
        s.result = convert(blob[S].bytes, blob[S].type);
        s.state = 2;
        emit(fr, 'progress', size, size);
        emit(fr, 'load', size, size);
      } catch (e) {
        s.state = 2;
        s.error = domError('NotReadableError', 'The file could not be read');
        emit(fr, 'error', 0, size);
      }
      emit(fr, 'loadend', size, size);
    });
  });
}

// ---- FormData ---------------------------------------------------------------------------------------

function toFileValue(value, filename) {
  if (hasBrand(value, 'Blob')) {
    if (!isFile(value) || filename !== undefined) {
      const f = new File([value], filename !== undefined ? str(filename) : isFile(value) ? value.name : 'blob', { type: value.type });
      return f;
    }
    return value;
  }
  return str(value);
}

class FormData {
  constructor(form) {
    const s = setSlots(this, { brand: 'FormData', list: [] });
    if (form !== undefined) {
      if (!(G.HTMLFormElement && form instanceof G.HTMLFormElement)) {
        throw new TypeError("Failed to construct 'FormData': parameter 1 is not of type 'HTMLFormElement'.");
      }
      for (const e of native.formEntries(form)) s.list.push([e[0], e[1]]);
    }
  }
  append(name, value, filename) {
    need(arguments, 2, 'append', 'FormData');
    if (arguments.length > 2 && !hasBrand(value, 'Blob')) {
      throw new TypeError("Failed to execute 'append' on 'FormData': parameter 2 is not of type 'Blob'.");
    }
    slots(this, 'FormData').list.push([str(name), toFileValue(value, filename)]);
  }
  delete(name) {
    need(arguments, 1, 'delete', 'FormData');
    const s = slots(this, 'FormData');
    name = str(name);
    s.list = s.list.filter((p) => p[0] !== name);
  }
  get(name) {
    need(arguments, 1, 'get', 'FormData');
    name = str(name);
    for (const p of slots(this, 'FormData').list) if (p[0] === name) return p[1];
    return null;
  }
  getAll(name) {
    need(arguments, 1, 'getAll', 'FormData');
    name = str(name);
    const out = [];
    for (const p of slots(this, 'FormData').list) if (p[0] === name) out.push(p[1]);
    return out;
  }
  has(name) {
    need(arguments, 1, 'has', 'FormData');
    name = str(name);
    for (const p of slots(this, 'FormData').list) if (p[0] === name) return true;
    return false;
  }
  set(name, value, filename) {
    need(arguments, 2, 'set', 'FormData');
    if (arguments.length > 2 && !hasBrand(value, 'Blob')) {
      throw new TypeError("Failed to execute 'set' on 'FormData': parameter 2 is not of type 'Blob'.");
    }
    const s = slots(this, 'FormData');
    name = str(name);
    const v = toFileValue(value, filename);
    let placed = false;
    const out = [];
    for (const p of s.list) {
      if (p[0] !== name) out.push(p);
      else if (!placed) {
        placed = true;
        out.push([name, v]);
      }
    }
    if (!placed) out.push([name, v]);
    s.list = out;
  }
  forEach(callback, thisArg) {
    need(arguments, 1, 'forEach', 'FormData');
    if (typeof callback !== 'function') throw new TypeError("Failed to execute 'forEach' on 'FormData': parameter 1 is not of type 'Function'.");
    const s = slots(this, 'FormData');
    for (let i = 0; i < s.list.length; i++) callback.call(thisArg, s.list[i][1], s.list[i][0], this);
  }
  *entries() {
    const s = slots(this, 'FormData');
    for (let i = 0; i < s.list.length; i++) yield [s.list[i][0], s.list[i][1]];
  }
  *keys() {
    const s = slots(this, 'FormData');
    for (let i = 0; i < s.list.length; i++) yield s.list[i][0];
  }
  *values() {
    const s = slots(this, 'FormData');
    for (let i = 0; i < s.list.length; i++) yield s.list[i][1];
  }
}
FormData.prototype[SymIter] = FormData.prototype.entries;
tag(FormData);
enumerable(FormData, ['append', 'delete', 'get', 'getAll', 'has', 'set', 'forEach', 'entries', 'keys', 'values']);

function randomToken(n) {
  const a = '0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ';
  let out = '';
  for (let i = 0; i < n; i++) out += a.charAt(Math.floor(Math.random() * a.length));
  return out;
}
const escName = (n) => n.replace(/\r/g, '%0D').replace(/\n/g, '%0A').replace(/"/g, '%22');
const crlf = (v) => v.replace(/\r\n|\r|\n/g, '\r\n');

// multipart/form-data (RFC 7578): the bytes and the boundary.
function encodeMultipart(list) {
  const boundary = '----MilkOSFormBoundary' + randomToken(16);
  const parts = [];
  const text = (t) => parts.push(native.bytesFromString(t));
  for (const [name, value] of list) {
    text('--' + boundary + '\r\nContent-Disposition: form-data; name="' + escName(crlf(name)) + '"');
    if (hasBrand(value, 'Blob')) {
      const f = isFile(value) ? value.name : 'blob';
      text('; filename="' + escName(f) + '"\r\nContent-Type: ' + (value.type || 'application/octet-stream') + '\r\n\r\n');
      parts.push(value[S].bytes);
    } else {
      text('\r\n\r\n' + crlf(value));
    }
    text('\r\n');
  }
  text('--' + boundary + '--\r\n');
  return { bytes: native.bytesConcat(parts), boundary };
}

// ---- the body of a Request or Response --------------------------------------------------------------
//
// A body source holds the bytes (or says they are on their way): {state, bytes, error, waiters}.
// clone() shares it; each Request or Response has its own `used` flag.

function readySource(bytes) {
  return { state: 'ready', bytes, error: null, waiters: [] };
}
function pendingSource() {
  return { state: 'pending', bytes: null, error: null, waiters: [] };
}
function srcFinish(src, bytes) {
  if (src.state !== 'pending') return;
  src.state = 'ready';
  src.bytes = bytes;
  const w = src.waiters;
  src.waiters = [];
  for (const x of w) x.resolve(bytes);
}
function srcFail(src, err) {
  if (src.state !== 'pending') return;
  src.state = 'error';
  src.error = err;
  const w = src.waiters;
  src.waiters = [];
  for (const x of w) x.reject(err);
}
function srcBytes(src) {
  if (src.state === 'ready') return Promise.resolve(src.bytes);
  if (src.state === 'error') return Promise.reject(src.error);
  return new Promise((resolve, reject) => src.waiters.push({ resolve, reject }));
}

// What `new Request(url, {body})` and `new Response(body)` accept: the bytes and the
// Content-Type it implies (null: none).
function extractBody(init) {
  if (hasBrand(init, 'Blob')) return { src: readySource(init[S].bytes), type: init[S].type || null };
  if (hasBrand(init, 'FormData')) {
    const m = encodeMultipart(init[S].list);
    return { src: readySource(m.bytes), type: 'multipart/form-data; boundary=' + m.boundary };
  }
  if (hasBrand(init, 'URLSearchParams')) {
    return { src: readySource(native.bytesFromString(serializeQuery(init[S].list))), type: 'application/x-www-form-urlencoded;charset=UTF-8' };
  }
  if (hasBrand(init, 'ReadableStream')) {
    return { src: null, stream: init, type: null };
  }
  if (isObj(init)) {
    const b = native.bytesFromBuffer(init);
    if (b !== null) return { src: readySource(b), type: null };
  }
  return { src: readySource(native.bytesFromString(str(init))), type: 'text/plain;charset=UTF-8' };
}

function newBody(ex) {
  if (ex === null) return { src: null, stream: null, used: false, view: null };
  return { src: ex.src, stream: ex.stream || null, used: false, view: null };
}

function consumeBody(s, how, what) {
  const b = s.body;
  const fail = (m) => Promise.reject(new TypeError("Failed to execute '" + how + "' on '" + what + "': " + m));
  if (b.used || (b.stream && b.stream[S].locked)) return fail('body stream already read');
  b.used = true;
  let p;
  if (b.src) p = srcBytes(b.src);
  else if (b.stream) p = readAllBytes(b.stream);
  else p = Promise.resolve(EMPTY);
  return p.then((bytes) => convertBody(bytes, how, s));
}

function contentTypeOf(s) {
  const list = s.headers ? headersList(s.headers) : [];
  return hdrGet(list, 'content-type');
}

function convertBody(bytes, how, s) {
  switch (how) {
    case 'text':
      return native.bytesToString(bytes);
    case 'json':
      return JSON.parse(native.bytesToString(bytes));
    case 'arrayBuffer':
      return native.bytesToArrayBuffer(bytes);
    case 'bytes':
      return new G.Uint8Array(native.bytesToArrayBuffer(bytes));
    case 'blob':
      return makeBlob(bytes, asciiLowerType(contentTypeOf(s) || ''));
    case 'formData':
      return parseFormData(bytes, contentTypeOf(s) || '');
    default:
      throw new TypeError('bad body type');
  }
}

function parseFormData(bytes, contentType) {
  const fd = new FormData();
  const list = fd[S].list;
  const e = mimeEssence(contentType);
  if (e === 'application/x-www-form-urlencoded') {
    for (const p of parseQuery(native.bytesToString(bytes))) list.push([p[0], p[1]]);
    return fd;
  }
  if (e === 'multipart/form-data') {
    const m = /boundary=(?:"([^"]+)"|([^;\s]+))/i.exec(contentType);
    if (!m) throw new TypeError('Failed to fetch: multipart body without a boundary');
    const boundary = m[1] || m[2];
    // Work on a binary string: one char per byte.
    const bin = native.bytesToBinary(bytes);
    const delim = '--' + boundary;
    let pos = bin.indexOf(delim);
    while (pos >= 0) {
      let start = pos + delim.length;
      if (bin.substr(start, 2) === '--') break;
      if (bin.substr(start, 2) === '\r\n') start += 2;
      const next = bin.indexOf('\r\n' + delim, start);
      if (next < 0) break;
      const part = bin.slice(start, next);
      const split = part.indexOf('\r\n\r\n');
      if (split >= 0) {
        const head = part.slice(0, split);
        const body = part.slice(split + 4);
        const disp = /content-disposition:[^\r\n]*/i.exec(head);
        const nm = disp && /[; ]name="([^"]*)"/i.exec(disp[0]);
        const fn = disp && /filename="([^"]*)"/i.exec(disp[0]);
        const ct = /content-type:\s*([^\r\n]*)/i.exec(head);
        if (nm) {
          const name = native.pctDecode(nm[1].replace(/%(?![0-9A-Fa-f]{2})/g, '%25'));
          const raw = native.bytesFromBinary(body);
          if (fn) list.push([name, makeFileFromBytes(raw, native.pctDecode(fn[1]), ct ? ct[1].trim() : '')]);
          else list.push([name, native.bytesToString(raw)]);
        }
      }
      pos = next + 2;
    }
    return fd;
  }
  throw new TypeError('Failed to fetch: the body is not a form (its Content-Type is ' + (contentType || 'missing') + ')');
}

function makeFileFromBytes(bytes, name, type) {
  const f = Object.create(File.prototype);
  setSlots(f, { brand: 'Blob', bytes, type: asciiLowerType(type), file: true, name, lastModified: Math.floor(native.time()) });
  return f;
}

// The body methods and attributes shared by Request and Response.
function bodyMixin(C, brandName) {
  for (const how of ['arrayBuffer', 'blob', 'bytes', 'formData', 'json', 'text']) {
    ObjDefine(C.prototype, how, {
      value: {
        [how]() {
          let s;
          try {
            s = slots(this, brandName);
          } catch (e) {
            return Promise.reject(e);
          }
          return consumeBody(s, how, brandName);
        },
      }[how],
      writable: true,
      configurable: true,
      enumerable: true,
    });
  }
  ObjDefine(C.prototype, 'bodyUsed', {
    get() {
      const b = slots(this, brandName).body;
      return b.used || !!(b.stream && b.stream[S].disturbed);
    },
    enumerable: true,
    configurable: true,
  });
  ObjDefine(C.prototype, 'body', {
    get() {
      const s = slots(this, brandName);
      return bodyStream(s);
    },
    enumerable: true,
    configurable: true,
  });
}

function bodyStream(s) {
  const b = s.body;
  if (b.stream) return b.stream;
  if (!b.src && !s.hasBodyStream) return null;
  if (!b.view) {
    b.view = streamOfSource(b.src, b);
  }
  return b.view;
}

// ---- ReadableStream (the part pages use: a default reader over chunks) -------------------------------

class ReadableStream {
  constructor(source, strategy) {
    const st = setSlots(this, {
      brand: 'ReadableStream', state: 'readable', queue: [], reads: [], source: source || {}, locked: false,
      disturbed: false, started: false, pulling: false, closeRequested: false, error: undefined, controller: null,
      hwm: strategy && strategy.highWaterMark !== undefined ? Number(strategy.highWaterMark) : 1,
    });
    const self = this;
    const controller = {
      enqueue(chunk) {
        if (st.state !== 'readable' || st.closeRequested) throw new TypeError('Cannot enqueue a chunk into a closed stream');
        if (st.reads.length > 0) st.reads.shift().resolve({ value: chunk, done: false });
        else st.queue.push(chunk);
      },
      close() {
        if (st.state !== 'readable' || st.closeRequested) throw new TypeError('Cannot close a closed stream');
        st.closeRequested = true;
        if (st.queue.length === 0) finishStream(st);
      },
      error(e) {
        errorStream(st, e);
      },
      get desiredSize() {
        return st.state === 'errored' ? null : st.state === 'closed' ? 0 : st.hwm - st.queue.length;
      },
    };
    st.controller = controller;
    const src = st.source;
    let started;
    try {
      started = src.start ? src.start.call(src, controller) : undefined;
    } catch (e) {
      errorStream(st, e);
      return;
    }
    Promise.resolve(started).then(() => {
      st.started = true;
      pullStream(st);
    }, (e) => errorStream(st, e));
  }
  get locked() {
    return slots(this, 'ReadableStream').locked;
  }
  getReader(options) {
    const st = slots(this, 'ReadableStream');
    if (options !== undefined && options !== null && options.mode !== undefined) throw new RangeError('Invalid reader mode');
    if (st.locked) throw new TypeError('ReadableStream is locked');
    st.locked = true;
    const reader = Object.create(ReadableStreamDefaultReader.prototype);
    let closedResolve, closedReject;
    const closed = new Promise((a, b) => {
      closedResolve = a;
      closedReject = b;
    });
    closed.catch(() => {});
    setSlots(reader, { brand: 'ReadableStreamDefaultReader', stream: st, closed, closedResolve, closedReject });
    if (st.state === 'closed') closedResolve(undefined);
    if (st.state === 'errored') closedReject(st.error);
    st.reader = reader[S];
    return reader;
  }
  cancel(reason) {
    const st = slots(this, 'ReadableStream');
    if (st.locked) return Promise.reject(new TypeError('Cannot cancel a locked stream'));
    return cancelStream(st, reason);
  }
  tee() {
    const st = slots(this, 'ReadableStream');
    if (st.locked) throw new TypeError('ReadableStream is locked');
    const chunksP = readAllChunks(this);
    const mk = () => {
      let i = 0;
      return new ReadableStream({
        pull(c) {
          return chunksP.then((chunks) => {
            if (i < chunks.length) c.enqueue(chunks[i++]);
            else c.close();
          }, (e) => c.error(e));
        },
      });
    };
    return [mk(), mk()];
  }
  values() {
    const reader = this.getReader();
    return {
      next: () => reader.read(),
      return: () => {
        reader.releaseLock();
        return Promise.resolve({ value: undefined, done: true });
      },
      [Symbol.asyncIterator]() {
        return this;
      },
    };
  }
}
if (Symbol.asyncIterator) ReadableStream.prototype[Symbol.asyncIterator] = ReadableStream.prototype.values;
tag(ReadableStream);
enumerable(ReadableStream, ['locked', 'getReader', 'cancel', 'tee', 'values']);

class ReadableStreamDefaultReader {
  constructor(stream) {
    if (!hasBrand(stream, 'ReadableStream')) throw new TypeError("Failed to construct 'ReadableStreamDefaultReader': parameter 1 is not of type 'ReadableStream'.");
    const r = stream.getReader();
    ObjDefine(this, S, { value: r[S] });
  }
  read() {
    let r;
    try {
      r = slots(this, 'ReadableStreamDefaultReader');
    } catch (e) {
      return Promise.reject(e);
    }
    const st = r.stream;
    if (st.reader !== r) return Promise.reject(new TypeError('This reader has been released'));
    st.disturbed = true;
    if (st.queue.length > 0) {
      const chunk = st.queue.shift();
      if (st.queue.length === 0 && st.closeRequested) finishStream(st);
      else pullStream(st);
      return Promise.resolve({ value: chunk, done: false });
    }
    if (st.state === 'closed') return Promise.resolve({ value: undefined, done: true });
    if (st.state === 'errored') return Promise.reject(st.error);
    return new Promise((resolve, reject) => {
      st.reads.push({ resolve, reject });
      pullStream(st);
    });
  }
  releaseLock() {
    const r = slots(this, 'ReadableStreamDefaultReader');
    const st = r.stream;
    if (st.reader !== r) return;
    st.reader = null;
    st.locked = false;
    for (const x of st.reads.splice(0)) x.reject(new TypeError('Reader was released'));
  }
  cancel(reason) {
    let r;
    try {
      r = slots(this, 'ReadableStreamDefaultReader');
    } catch (e) {
      return Promise.reject(e);
    }
    if (r.stream.reader !== r) return Promise.reject(new TypeError('This reader has been released'));
    return cancelStream(r.stream, reason);
  }
  get closed() {
    return slots(this, 'ReadableStreamDefaultReader').closed;
  }
}
tag(ReadableStreamDefaultReader);

function pullStream(st) {
  if (!st.started || st.pulling || st.closeRequested || st.state !== 'readable') return;
  if (st.reads.length === 0 && st.queue.length >= Math.max(st.hwm, 1)) return;
  const pull = st.source.pull;
  if (typeof pull !== 'function') return;
  st.pulling = true;
  Promise.resolve()
    .then(() => pull.call(st.source, st.controller))
    .then(
      () => {
        st.pulling = false;
        if (st.reads.length > 0 || st.queue.length < st.hwm) pullStream(st);
      },
      (e) => errorStream(st, e)
    );
}
function finishStream(st) {
  if (st.state !== 'readable') return;
  st.state = 'closed';
  for (const x of st.reads.splice(0)) x.resolve({ value: undefined, done: true });
  if (st.reader) st.reader.closedResolve(undefined);
}
function errorStream(st, e) {
  if (st.state !== 'readable') return;
  st.state = 'errored';
  st.error = e;
  st.queue.length = 0;
  for (const x of st.reads.splice(0)) x.reject(e);
  if (st.reader) st.reader.closedReject(e);
}
function cancelStream(st, reason) {
  st.disturbed = true;
  if (st.state === 'closed') return Promise.resolve();
  if (st.state === 'errored') return Promise.reject(st.error);
  st.queue.length = 0;
  finishStream(st);
  const c = st.source.cancel;
  return Promise.resolve(typeof c === 'function' ? c.call(st.source, reason) : undefined).then(() => undefined);
}
function readAllChunks(stream) {
  const reader = stream.getReader();
  const out = [];
  const step = () =>
    reader.read().then((r) => {
      if (r.done) return out;
      out.push(r.value);
      return step();
    });
  return step();
}
function readAllBytes(stream) {
  return readAllChunks(stream).then((chunks) => {
    const parts = [];
    for (const c of chunks) {
      const b = isObj(c) ? native.bytesFromBuffer(c) : null;
      if (b === null) throw new TypeError('Received non-Uint8Array chunk');
      parts.push(b);
    }
    return native.bytesConcat(parts);
  });
}

// A stream that gives out a body in one chunk, once it is there.
function streamOfSource(src, body) {
  let done = false;
  return new ReadableStream({
    pull(c) {
      if (done) return undefined;
      done = true;
      return srcBytes(src).then((bytes) => {
        body.used = true;
        if (native.bytesLength(bytes) > 0) c.enqueue(new G.Uint8Array(native.bytesToArrayBuffer(bytes)));
        c.close();
      });
    },
    cancel() {
      body.used = true;
    },
  });
}
function streamOfBytes(bytes) {
  return streamOfSource(readySource(bytes), { used: false });
}
