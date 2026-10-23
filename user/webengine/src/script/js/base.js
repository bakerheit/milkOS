'use strict';
// The page's network API, in script. Run once, the first time a page uses fetch, XMLHttpRequest,
// Headers, URL and the like (script/net.rs), as a function body taking `native`. Nothing here is
// trusted for security: the app applies the same-origin policy and the rules on headers again
// to every request this makes. This is the shape of the API.
//
// Conventions: a class's state lives in a hidden slot (a symbol-keyed property holding an object
// with a `brand`), since the engine has no private fields; methods check it the way WebIDL
// checks `this` ("Illegal invocation").

const G = native.global;
const S = Symbol('slots');
const L = Symbol('listeners');
const H = Symbol('handlers');
const TAG = Symbol.toStringTag;
const hop = Object.prototype.hasOwnProperty;
const SymIter = Symbol.iterator;
const ObjDefine = Object.defineProperty;
const isObj = (v) => v !== null && (typeof v === 'object' || typeof v === 'function');

function setSlots(o, s) {
  ObjDefine(o, S, { value: s });
  return s;
}
function slots(o, brand) {
  const s = isObj(o) ? o[S] : undefined;
  if (s === undefined || s.brand !== brand) throw new TypeError('Illegal invocation');
  return s;
}
function hasBrand(o, brand) {
  const s = isObj(o) ? o[S] : undefined;
  return s !== undefined && s.brand === brand;
}
function tag(C, name) {
  ObjDefine(C.prototype, TAG, { value: name || C.name, configurable: true });
}
// WebIDL-style: the members of a prototype are enumerable.
function enumerable(C, names) {
  for (const n of names) {
    const d = Object.getOwnPropertyDescriptor(C.prototype, n);
    if (d) {
      d.enumerable = true;
      ObjDefine(C.prototype, n, d);
    }
  }
}
function constants(C, map) {
  for (const k of Object.keys(map)) {
    ObjDefine(C, k, { value: map[k], enumerable: true });
    ObjDefine(C.prototype, k, { value: map[k], enumerable: true });
  }
}
function need(args, n, what, ctx) {
  if (args.length < n) {
    throw new TypeError(
      "Failed to execute '" + what + "' on '" + ctx + "': " + n + ' argument' + (n > 1 ? 's' : '') +
        ' required, but only ' + args.length + ' present.'
    );
  }
}
function str(v) {
  if (typeof v === 'symbol') throw new TypeError('Cannot convert a Symbol value to a string');
  return String(v);
}
function warn(text) {
  native.log('warn', text);
}

// ---- DOMException ---------------------------------------------------------------------------

const DOM_CODES = {
  IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5,
  NoModificationAllowedError: 7, NotFoundError: 8, NotSupportedError: 9, InUseAttributeError: 10,
  InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14,
  InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20,
  URLMismatchError: 21, QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24,
  DataCloneError: 25,
};
const DOM_CONSTS = {
  INDEX_SIZE_ERR: 1, DOMSTRING_SIZE_ERR: 2, HIERARCHY_REQUEST_ERR: 3, WRONG_DOCUMENT_ERR: 4,
  INVALID_CHARACTER_ERR: 5, NO_DATA_ALLOWED_ERR: 6, NO_MODIFICATION_ALLOWED_ERR: 7, NOT_FOUND_ERR: 8,
  NOT_SUPPORTED_ERR: 9, INUSE_ATTRIBUTE_ERR: 10, INVALID_STATE_ERR: 11, SYNTAX_ERR: 12,
  INVALID_MODIFICATION_ERR: 13, NAMESPACE_ERR: 14, INVALID_ACCESS_ERR: 15, VALIDATION_ERR: 16,
  TYPE_MISMATCH_ERR: 17, SECURITY_ERR: 18, NETWORK_ERR: 19, ABORT_ERR: 20, URL_MISMATCH_ERR: 21,
  QUOTA_EXCEEDED_ERR: 22, TIMEOUT_ERR: 23, INVALID_NODE_TYPE_ERR: 24, DATA_CLONE_ERR: 25,
};

class DOMException extends Error {
  constructor(message = '', name = 'Error') {
    super(str(message));
    name = str(name);
    ObjDefine(this, 'name', { value: name, writable: true, configurable: true });
    ObjDefine(this, 'code', { value: hop.call(DOM_CODES, name) ? DOM_CODES[name] : 0, configurable: true });
  }
}
for (const k of Object.keys(DOM_CONSTS)) ObjDefine(DOMException, k, { value: DOM_CONSTS[k], enumerable: true });
tag(DOMException);
const domError = (name, message) => new DOMException(message, name);

// ---- events on objects that are not DOM nodes ------------------------------------------------

// The EventTarget methods for the network classes (the page's own EventTarget is for nodes).
class NetEventTarget {
  addEventListener(type, callback, options) {
    need(arguments, 2, 'addEventListener', 'EventTarget');
    if (callback === null || callback === undefined) return;
    type = str(type);
    let capture = false, once = false, signal;
    if (isObj(options)) {
      capture = !!options.capture;
      once = !!options.once;
      signal = options.signal;
    } else {
      capture = !!options;
    }
    if (signal !== undefined && signal.aborted) return;
    const e = addListener(this, type, { cb: callback, capture, once, removed: false, handler: false });
    if (e && signal !== undefined) {
      signal.addEventListener('abort', () => this.removeEventListener(type, callback, capture), { once: true });
    }
  }
  removeEventListener(type, callback, options) {
    need(arguments, 2, 'removeEventListener', 'EventTarget');
    type = str(type);
    const capture = isObj(options) ? !!options.capture : !!options;
    const map = this[L];
    const list = map && map.get(type);
    if (!list) return;
    for (let i = 0; i < list.length; i++) {
      const e = list[i];
      if (!e.handler && e.cb === callback && e.capture === capture) {
        e.removed = true;
        list.splice(i, 1);
        return;
      }
    }
  }
  dispatchEvent(event) {
    need(arguments, 1, 'dispatchEvent', 'EventTarget');
    if (!(event instanceof G.Event)) {
      throw new TypeError("Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.");
    }
    fire(this, event);
    return !event.defaultPrevented;
  }
}
tag(NetEventTarget, 'EventTarget');
// `x instanceof EventTarget` holds for them too.
if (G.EventTarget && G.EventTarget.prototype) Object.setPrototypeOf(NetEventTarget.prototype, G.EventTarget.prototype);

function addListener(t, type, entry) {
  if (!hop.call(t, L)) ObjDefine(t, L, { value: new Map() });
  let list = t[L].get(type);
  if (!list) t[L].set(type, (list = []));
  for (const e of list) {
    if (!e.handler && !entry.handler && e.cb === entry.cb && e.capture === entry.capture) return null;
  }
  list.push(entry);
  return entry;
}

function fire(t, ev) {
  const map = t[L];
  const list = map && map.get(ev.type);
  const set = (k, v) => ObjDefine(ev, k, { value: v, configurable: true, enumerable: true });
  set('target', t);
  set('srcElement', t);
  set('currentTarget', t);
  set('eventPhase', 2);
  if (list) {
    for (const e of list.slice()) {
      if (e.removed) continue;
      if (e.once) {
        e.removed = true;
        const i = list.indexOf(e);
        if (i >= 0) list.splice(i, 1);
      }
      const cb = e.handler ? e.fn : e.cb;
      try {
        if (typeof cb === 'function') cb.call(t, ev);
        else if (isObj(cb) && typeof cb.handleEvent === 'function') cb.handleEvent(ev);
      } catch (err) {
        native.report(err);
      }
      if (ev.cancelBubble) break;
    }
  }
  set('currentTarget', null);
  set('eventPhase', 0);
}

// onload = fn, ...: the handler sits in the listener list where it was first set.
function handlerAttrs(C, types) {
  for (const type of types) {
    ObjDefine(C.prototype, 'on' + type, {
      get() {
        const h = this[H];
        const e = h && h[type];
        return e && e.fn !== undefined ? e.fn : null;
      },
      set(v) {
        if (!hop.call(this, H)) ObjDefine(this, H, { value: Object.create(null) });
        const h = this[H];
        if (!isObj(v)) v = null;
        let e = h[type];
        if (!e) {
          if (v === null) return;
          e = h[type] = { fn: v, handler: true, removed: false, once: false, capture: false };
          addListener(this, type, e);
        } else {
          e.fn = v;
        }
      },
      enumerable: true,
      configurable: true,
    });
  }
}

class ProgressEvent extends G.Event {
  constructor(type, init = {}) {
    need(arguments, 1, 'ProgressEvent', 'ProgressEvent');
    super(type, init);
    init = init === undefined || init === null ? {} : init;
    ObjDefine(this, 'lengthComputable', { value: !!init.lengthComputable, enumerable: true });
    ObjDefine(this, 'loaded', { value: Number(init.loaded) || 0, enumerable: true });
    ObjDefine(this, 'total', { value: Number(init.total) || 0, enumerable: true });
  }
}
tag(ProgressEvent);

function emit(target, type, loaded, total) {
  const ev = new ProgressEvent(type, { lengthComputable: total > 0, loaded: loaded || 0, total: total > 0 ? total : 0 });
  fire(target, ev);
}
function emitPlain(target, type) {
  fire(target, new G.Event(type));
}

const TOKEN = {};

// ---- small helpers ----------------------------------------------------------------------------

const HTTP_WS = /^[\t\n\r ]+|[\t\n\r ]+$/g;
const trimHttp = (s) => s.replace(HTTP_WS, '');
const TOKEN_RE = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const isToken = (s) => TOKEN_RE.test(s);
const nextTick = (f) => native.setTimer(f, 0);
