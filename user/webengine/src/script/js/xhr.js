
// ---- XMLHttpRequest ---------------------------------------------------------------------------------------
//
// The state machine of the XHR standard over the same request machinery as fetch. What differs
// from a browser: a synchronous request is refused (it would freeze the page, and the Browser's
// pages run on one thread); `responseType = "document"` is accepted but the response is null (there
// is no HTML parser for scripts to call yet); `responseText` fills in when the body is complete,
// not while it arrives (readyState 3 and `progress` events still come).

class XMLHttpRequestEventTarget extends NetEventTarget {
  constructor() {
    super();
  }
}
handlerAttrs(XMLHttpRequestEventTarget, ['loadstart', 'progress', 'abort', 'error', 'load', 'timeout', 'loadend']);
tag(XMLHttpRequestEventTarget);

class XMLHttpRequestUpload extends XMLHttpRequestEventTarget {
  constructor(token) {
    if (token !== TOKEN) throw new TypeError('Illegal constructor');
    super();
  }
}
tag(XMLHttpRequestUpload);

const XHR_TYPES = ['', 'arraybuffer', 'blob', 'document', 'json', 'text'];

function xhrFail(name, message) {
  return domError(name, message);
}

class XMLHttpRequest extends XMLHttpRequestEventTarget {
  constructor() {
    super();
    setSlots(this, {
      brand: 'XMLHttpRequest', state: 0, sendFlag: false, method: '', url: '', headers: [], timeout: 0,
      withCredentials: false, responseType: '', upload: new XMLHttpRequestUpload(TOKEN), reqId: 0, token: 0,
      status: 0, statusText: '', responseHeaders: [], responseURL: '', bytes: null, error: false, mime: null,
      timer: 0, hasBody: false, bodySize: 0, cached: undefined, cachedFor: -1, loaded: 0, total: 0,
    });
  }
  get readyState() {
    return slots(this, 'XMLHttpRequest').state;
  }
  get upload() {
    return slots(this, 'XMLHttpRequest').upload;
  }
  get timeout() {
    return slots(this, 'XMLHttpRequest').timeout;
  }
  set timeout(v) {
    const s = slots(this, 'XMLHttpRequest');
    v = Math.trunc(Number(v));
    s.timeout = v >= 0 && v <= 4294967295 ? v : 0;
  }
  get withCredentials() {
    return slots(this, 'XMLHttpRequest').withCredentials;
  }
  set withCredentials(v) {
    const s = slots(this, 'XMLHttpRequest');
    if ((s.state !== 0 && s.state !== 1) || s.sendFlag) {
      throw xhrFail('InvalidStateError', "Failed to set the 'withCredentials' property on 'XMLHttpRequest': The value may only be set if the object's state is UNSENT or OPENED.");
    }
    s.withCredentials = !!v;
  }
  get responseType() {
    return slots(this, 'XMLHttpRequest').responseType;
  }
  set responseType(v) {
    const s = slots(this, 'XMLHttpRequest');
    v = str(v);
    if (XHR_TYPES.indexOf(v) < 0) return;
    if (s.state === 3 || s.state === 4) {
      throw xhrFail('InvalidStateError', "Failed to set the 'responseType' property on 'XMLHttpRequest': The response type cannot be set if the object's state is LOADING or DONE.");
    }
    if (v === 'document') warn("XMLHttpRequest: responseType 'document' isn't supported; the response will be null");
    s.responseType = v;
  }
  get responseURL() {
    return slots(this, 'XMLHttpRequest').responseURL;
  }
  get status() {
    return slots(this, 'XMLHttpRequest').status;
  }
  get statusText() {
    return slots(this, 'XMLHttpRequest').statusText;
  }
  get responseText() {
    const s = slots(this, 'XMLHttpRequest');
    if (s.responseType !== '' && s.responseType !== 'text') {
      throw xhrFail('InvalidStateError', "Failed to read the 'responseText' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'text' (was '" + s.responseType + "').");
    }
    return textOf(s);
  }
  get responseXML() {
    const s = slots(this, 'XMLHttpRequest');
    if (s.responseType !== '' && s.responseType !== 'document') {
      throw xhrFail('InvalidStateError', "Failed to read the 'responseXML' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'document' (was '" + s.responseType + "').");
    }
    return null;
  }
  get response() {
    const s = slots(this, 'XMLHttpRequest');
    switch (s.responseType) {
      case '':
      case 'text':
        return textOf(s);
      case 'document':
        return null;
      default:
        if (s.state !== 4 || s.error || s.bytes === null) return null;
        if (s.cachedFor === s.reqId && s.cached !== undefined) return s.cached;
        {
          let v;
          if (s.responseType === 'arraybuffer') {
            v = native.bytesToArrayBuffer(s.bytes);
          } else if (s.responseType === 'blob') {
            v = makeBlob(s.bytes, asciiLowerType(responseMime(s) || ''));
          } else {
            try {
              v = JSON.parse(native.bytesToString(s.bytes));
            } catch (e) {
              v = null;
            }
          }
          s.cached = v;
          s.cachedFor = s.reqId;
          return v;
        }
    }
  }
  open(method, url, async, username, password) {
    const s = slots(this, 'XMLHttpRequest');
    need(arguments, 2, 'open', 'XMLHttpRequest');
    method = str(method);
    if (!isToken(method)) throw xhrFail('SyntaxError', "Failed to execute 'open' on 'XMLHttpRequest': '" + method + "' is not a valid HTTP method.");
    if (FORBIDDEN_METHODS.has(method.toLowerCase())) throw xhrFail('SecurityError', "Failed to execute 'open' on 'XMLHttpRequest': '" + method + "' HTTP method is unsupported.");
    const up = method.toUpperCase();
    if (NORMAL_METHODS.indexOf(up) >= 0) method = up;
    const rec = resolveUrl(str(url));
    if (!rec) throw xhrFail('SyntaxError', "Failed to execute 'open' on 'XMLHttpRequest': Invalid URL");
    if (async !== undefined && !async) {
      warn("Synchronous XMLHttpRequest isn't supported by this browser (it would freeze the page): " + method + ' ' + rec.href);
      throw xhrFail('InvalidAccessError', "Failed to execute 'open' on 'XMLHttpRequest': Synchronous requests are not supported.");
    }
    // Any request in flight is dropped.
    abortRequest(s);
    s.method = method;
    s.url = rec.href;
    s.headers = [];
    s.sendFlag = false;
    s.status = 0;
    s.statusText = '';
    s.responseHeaders = [];
    s.responseURL = '';
    s.bytes = null;
    s.error = false;
    s.cached = undefined;
    if (s.state !== 1) {
      s.state = 1;
      emitPlain(this, 'readystatechange');
    }
  }
  setRequestHeader(name, value) {
    const s = slots(this, 'XMLHttpRequest');
    need(arguments, 2, 'setRequestHeader', 'XMLHttpRequest');
    if (s.state !== 1) throw xhrFail('InvalidStateError', "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': The object's state must be OPENED.");
    if (s.sendFlag) throw xhrFail('InvalidStateError', "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': The object's state must be OPENED.");
    name = str(name);
    if (!isToken(name)) throw xhrFail('SyntaxError', "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': '" + name + "' is not a valid HTTP header field name.");
    value = trimHttp(str(value));
    for (let i = 0; i < value.length; i++) {
      const c = value.charCodeAt(i);
      if (c === 0 || c === 10 || c === 13 || c > 255) {
        throw xhrFail('SyntaxError', "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': '" + value + "' is not a valid HTTP header field value.");
      }
    }
    const lower = name.toLowerCase();
    if (forbiddenRequestHeader(lower)) {
      warn("Refused to set unsafe header \"" + name + '"');
      return;
    }
    for (const p of s.headers) {
      if (p[0] === lower) {
        p[1] += ', ' + value;
        return;
      }
    }
    s.headers.push([lower, value]);
  }
  send(body) {
    const s = slots(this, 'XMLHttpRequest');
    if (s.state !== 1 || s.sendFlag) throw xhrFail('InvalidStateError', "Failed to execute 'send' on 'XMLHttpRequest': The object's state must be OPENED.");
    let bytes = null;
    const headers = s.headers.map((p) => [p[0], p[1]]);
    if (body !== undefined && body !== null && s.method !== 'GET' && s.method !== 'HEAD') {
      const ex = extractBody(body);
      if (ex.stream) throw new TypeError("Failed to execute 'send' on 'XMLHttpRequest': ReadableStream bodies aren't supported.");
      bytes = ex.src.bytes;
      if (ex.type !== null && !headers.some((p) => p[0] === 'content-type')) headers.push(['content-type', ex.type]);
    }
    const me = this;
    const token = ++s.token;
    s.sendFlag = true;
    s.error = false;
    s.cached = undefined;
    s.hasBody = bytes !== null;
    s.bodySize = bytes === null ? 0 : native.bytesLength(bytes);
    s.loaded = 0;
    s.total = 0;
    emit(this, 'loadstart', 0, 0);
    if (s.hasBody && s.state === 1 && s.token === token) emit(s.upload, 'loadstart', 0, s.bodySize);
    if (s.token !== token) return;
    const id = native.start({
      kind: 'xhr', url: s.url, method: s.method, headers, body: bytes, mode: 'cors',
      credentials: s.withCredentials ? 'include' : 'same-origin', redirect: 'follow', referrer: 'about:client',
      referrerPolicy: '', cache: 'default', timeout: s.timeout,
    }, (kind, data) => {
      if (s.token !== token || !s.sendFlag) return;
      xhrEvent(me, s, kind, data);
    });
    if (!id) {
      s.reqId = 0;
      nextTick(() => {
        if (s.token === token && s.sendFlag) xhrError(me, s, 'error');
      });
      return;
    }
    s.reqId = id;
    if (s.timeout > 0) {
      s.timer = native.setTimer(() => {
        if (s.token === token && s.sendFlag) {
          native.cancel(id);
          xhrError(me, s, 'timeout');
        }
      }, s.timeout);
    }
  }
  abort() {
    const s = slots(this, 'XMLHttpRequest');
    const active = s.sendFlag && (s.state === 1 || s.state === 2 || s.state === 3);
    abortRequest(s);
    if (active) {
      s.sendFlag = false;
      s.state = 4;
      s.status = 0;
      s.statusText = '';
      s.responseHeaders = [];
      s.bytes = null;
      emitPlain(this, 'readystatechange');
      if (s.hasBody) {
        emit(s.upload, 'abort', 0, s.bodySize);
        emit(s.upload, 'loadend', 0, s.bodySize);
      }
      emit(this, 'abort', 0, 0);
      emit(this, 'loadend', 0, 0);
    }
    if (s.state === 4) {
      s.state = 0;
      s.error = false;
    }
  }
  getResponseHeader(name) {
    const s = slots(this, 'XMLHttpRequest');
    need(arguments, 1, 'getResponseHeader', 'XMLHttpRequest');
    if (s.state < 2 || s.error) return null;
    name = str(name).toLowerCase();
    if (!isToken(name) || FORBIDDEN_RESPONSE.has(name)) return null;
    return hdrGet(s.responseHeaders, name);
  }
  getAllResponseHeaders() {
    const s = slots(this, 'XMLHttpRequest');
    if (s.state < 2 || s.error) return '';
    let out = '';
    for (const p of sortedList({ list: s.responseHeaders })) out += p[0] + ': ' + p[1] + '\r\n';
    return out;
  }
  overrideMimeType(mime) {
    const s = slots(this, 'XMLHttpRequest');
    need(arguments, 1, 'overrideMimeType', 'XMLHttpRequest');
    if (s.state === 3 || s.state === 4) throw xhrFail('InvalidStateError', "Failed to execute 'overrideMimeType' on 'XMLHttpRequest': MimeType cannot be overridden when the state is LOADING or DONE.");
    s.mime = str(mime);
  }
}
constants(XMLHttpRequest, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 });
handlerAttrs(XMLHttpRequest, ['readystatechange']);
tag(XMLHttpRequest);
enumerable(XMLHttpRequest, ['readyState', 'upload', 'timeout', 'withCredentials', 'responseType', 'responseURL', 'status', 'statusText', 'responseText', 'responseXML', 'response', 'open', 'setRequestHeader', 'send', 'abort', 'getResponseHeader', 'getAllResponseHeaders', 'overrideMimeType']);

function abortRequest(s) {
  s.token++;
  if (s.timer) {
    native.clearTimer(s.timer);
    s.timer = 0;
  }
  if (s.reqId) {
    native.cancel(s.reqId);
    s.reqId = 0;
  }
}

function responseMime(s) {
  if (s.mime !== null) return s.mime;
  return hdrGet(s.responseHeaders, 'content-type');
}

function textOf(s) {
  if (s.state !== 4 || s.error || s.bytes === null) return '';
  if (s.cachedFor === s.reqId && typeof s.cached === 'string') return s.cached;
  // The charset of the Content-Type (or the override), else UTF-8.
  const ct = responseMime(s) || '';
  const m = /charset\s*=\s*"?([^";\s]+)/i.exec(ct);
  const text = native.bytesDecode(s.bytes, m ? m[1] : 'utf-8');
  s.cached = text;
  s.cachedFor = s.reqId;
  return text;
}

function finishRequest(s) {
  if (s.timer) {
    native.clearTimer(s.timer);
    s.timer = 0;
  }
  s.reqId = 0;
  s.sendFlag = false;
}

function xhrEvent(x, s, kind, data) {
  switch (kind) {
    case 'head': {
      s.status = data.status;
      s.statusText = data.statusText;
      s.responseURL = data.url.replace(/#.*$/, '');
      s.responseHeaders = [];
      if (data.type !== 'opaque' && data.type !== 'opaqueredirect') {
        for (const p of data.headers) {
          const lower = String(p[0]).toLowerCase();
          if (isToken(lower) && !FORBIDDEN_RESPONSE.has(lower)) s.responseHeaders.push([lower, String(p[1])]);
        }
      }
      if (s.hasBody) {
        emit(s.upload, 'progress', s.bodySize, s.bodySize);
        emit(s.upload, 'load', s.bodySize, s.bodySize);
        emit(s.upload, 'loadend', s.bodySize, s.bodySize);
      }
      s.state = 2;
      emitPlain(x, 'readystatechange');
      break;
    }
    case 'progress': {
      s.loaded = data.loaded;
      s.total = data.total >= 0 ? data.total : 0;
      if (s.state === 2) {
        s.state = 3;
        emitPlain(x, 'readystatechange');
      }
      if (s.sendFlag) emit(x, 'progress', s.loaded, s.total);
      break;
    }
    case 'done': {
      const size = native.bytesLength(data);
      s.bytes = data;
      finishRequest(s);
      s.state = 4;
      emitPlain(x, 'readystatechange');
      emit(x, 'progress', size, size);
      emit(x, 'load', size, size);
      emit(x, 'loadend', size, size);
      break;
    }
    case 'failed':
      xhrError(x, s, data.kind === 'timeout' ? 'timeout' : 'error');
      break;
    default:
      break;
  }
}

// The request failed: a network error ("error") or its time ran out ("timeout").
function xhrError(x, s, type) {
  const hadBody = s.hasBody;
  const size = s.bodySize;
  finishRequest(s);
  s.token++;
  s.state = 4;
  s.error = true;
  s.status = 0;
  s.statusText = '';
  s.responseHeaders = [];
  s.bytes = null;
  emitPlain(x, 'readystatechange');
  if (hadBody) {
    emit(s.upload, type, 0, size);
    emit(s.upload, 'loadend', 0, size);
  }
  emit(x, type, 0, 0);
  emit(x, 'loadend', 0, 0);
}
