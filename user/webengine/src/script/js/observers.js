// ---- MutationObserver, IntersectionObserver, ResizeObserver -----------------------------------------
// MutationObserver: the DOM keeps a log of what happened to the nodes that are watched
// (dom.rs, `MutLog`); at the microtask checkpoint the host calls `deliverMutations`, which takes
// the records (`native.moTake`), sorts them to the observers that asked for them and calls the
// callbacks. IntersectionObserver and ResizeObserver look at boxes: the host asks for a check
// when the page is laid out again, scrolled or resized (`native.layoutWatch`, `checkLayout`).

const moRegs = new Map(); // watched node -> [{ obs, o }]
let moSeq = 0;
const MO_MAX_QUEUE = 50000;

class MutationRecord {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
}
tag(MutationRecord);

function moOptions(o) {
  const bad = (m) => new TypeError("Failed to execute 'observe' on 'MutationObserver': " + m);
  if (!isObj(o)) throw bad("The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.");
  const flag = (v) => (v === undefined ? undefined : !!v);
  const childList = !!o.childList;
  const subtree = !!o.subtree;
  let attributes = flag(o.attributes);
  let characterData = flag(o.characterData);
  const attributeOldValue = flag(o.attributeOldValue);
  const characterDataOldValue = flag(o.characterDataOldValue);
  if ((attributeOldValue !== undefined || o.attributeFilter !== undefined) && attributes === undefined) attributes = true;
  if (characterDataOldValue !== undefined && characterData === undefined) characterData = true;
  if (!childList && !attributes && !characterData) {
    throw bad("The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.");
  }
  if (attributeOldValue && attributes === false) {
    throw bad("The options object may only set 'attributeOldValue' to true when 'attributes' is true or not present.");
  }
  if (o.attributeFilter !== undefined && attributes === false) {
    throw bad("The options object may only set 'attributeFilter' when 'attributes' is true or not present.");
  }
  if (characterDataOldValue && characterData === false) {
    throw bad("The options object may only set 'characterDataOldValue' to true when 'characterData' is true or not present.");
  }
  return {
    childList,
    subtree,
    attributes: !!attributes,
    characterData: !!characterData,
    attributeOldValue: !!attributeOldValue,
    characterDataOldValue: !!characterDataOldValue,
    filter: o.attributeFilter !== undefined ? Array.from(o.attributeFilter, str) : null,
  };
}

function moUnwatch(s, target) {
  const o = s.regs.get(target);
  if (!o) return;
  s.regs.delete(target);
  native.moWatch(target, o.subtree, -1);
  const list = moRegs.get(target);
  if (list) {
    const i = list.findIndex((x) => x.obs === s);
    if (i >= 0) list.splice(i, 1);
    if (!list.length) moRegs.delete(target);
  }
}

class MutationObserver {
  constructor(callback) {
    need(arguments, 1, 'MutationObserver', 'MutationObserver');
    if (typeof callback !== 'function') throw new TypeError("Failed to construct 'MutationObserver': The callback provided as parameter 1 is not a function.");
    setSlots(this, { brand: 'MutationObserver', cb: callback, regs: new Map(), queue: [], id: ++moSeq, self: this });
  }
  observe(target, options) {
    const s = slots(this, 'MutationObserver');
    need(arguments, 1, 'observe', 'MutationObserver');
    if (!(target instanceof G.Node)) throw new TypeError("Failed to execute 'observe' on 'MutationObserver': parameter 1 is not of type 'Node'.");
    const o = moOptions(options);
    moUnwatch(s, target);
    if (!native.moWatch(target, o.subtree, 1)) {
      throw new DOMException('Too many nodes are being observed.', 'QuotaExceededError');
    }
    s.regs.set(target, o);
    let list = moRegs.get(target);
    if (!list) moRegs.set(target, (list = []));
    list.push({ obs: s, o });
  }
  disconnect() {
    const s = slots(this, 'MutationObserver');
    for (const t of Array.from(s.regs.keys())) moUnwatch(s, t);
    s.queue = [];
  }
  takeRecords() {
    const s = slots(this, 'MutationObserver');
    moDistribute();
    const q = s.queue;
    s.queue = [];
    return q;
  }
}
tag(MutationObserver);
enumerable(MutationObserver, ['observe', 'disconnect', 'takeRecords']);

function moRecord(rec) {
  const r = Object.create(MutationRecord.prototype);
  const kind = rec[0];
  r.type = kind === 0 ? 'childList' : kind === 1 ? 'attributes' : 'characterData';
  r.target = rec[1];
  r.addedNodes = rec[2] ? [rec[2]] : [];
  r.removedNodes = rec[3] ? [rec[3]] : [];
  r.previousSibling = rec[4] || null;
  r.nextSibling = rec[5] || null;
  r.attributeName = kind === 1 ? rec[6] : null;
  r.attributeNamespace = null;
  r.oldValue = null;
  return r;
}

// The records the DOM made, put in the queues of the observers that asked; the observers with
// something new (by age).
function moDistribute() {
  const recs = native.moTake();
  const touched = new Set();
  for (const rec of recs) {
    const kind = rec[0];
    const target = rec[1];
    let mine = null;
    for (const a of rec[8]) {
      const list = moRegs.get(a);
      if (!list) continue;
      for (const reg of list) {
        const o = reg.o;
        if (a !== target && !o.subtree) continue;
        let ok;
        let wantOld = false;
        if (kind === 0) {
          ok = o.childList;
        } else if (kind === 1) {
          ok = o.attributes && (o.filter === null || o.filter.indexOf(rec[6]) >= 0);
          wantOld = o.attributeOldValue;
        } else {
          ok = o.characterData;
          wantOld = o.characterDataOldValue;
        }
        if (!ok) continue;
        if (mine === null) mine = new Map();
        let r = mine.get(reg.obs);
        if (r === undefined) {
          r = moRecord(rec);
          mine.set(reg.obs, r);
          if (reg.obs.queue.length < MO_MAX_QUEUE) reg.obs.queue.push(r);
          touched.add(reg.obs);
        }
        if (wantOld && r.oldValue === null) r.oldValue = rec[7];
      }
    }
  }
  return touched;
}

function deliverMutations() {
  const touched = moDistribute();
  const order = Array.from(touched).sort((a, b) => a.id - b.id);
  for (const s of order) {
    const q = s.queue;
    if (!q.length) continue;
    s.queue = [];
    try {
      s.cb.call(s.self, q, s.self);
    } catch (e) {
      native.report(e);
    }
  }
}
// ---- layout observers --------------------------------------------------------------------------------

const ioSet = new Set(); // IntersectionObserver slots with targets
const roSet = new Set(); // ResizeObserver slots with targets
let layoutOn = false;
function layoutSync() {
  const want = ioSet.size + roSet.size > 0;
  if (want !== layoutOn) {
    layoutOn = want;
    native.layoutWatch(want);
  }
}

const isNode = (v) => v instanceof G.Node;
const isElement = (v) => v instanceof G.Element;
const rendered = (el) => el.isConnected && el.getClientRects().length > 0;

function parseMargin(text) {
  const parts = String(text).trim().split(/\s+/);
  if (parts.length < 1 || parts.length > 4 || parts[0] === '') return null;
  const out = [];
  for (const p of parts) {
    const m = /^(-?(?:\d+\.?\d*|\.\d+))(px|%)$/.exec(p);
    if (!m) return null;
    out.push({ v: parseFloat(m[1]), pct: m[2] === '%' });
  }
  const [t, r = t, b = t, l = r] = out;
  return { top: t, right: r, bottom: b, left: l, text: parts.map((p, i) => p).join(' ') };
}

function rectOf(x, y, w, h) {
  return new DOMRectReadOnly(x, y, w, h);
}

class IntersectionObserverEntry {
  constructor(init) {
    if (!isObj(init)) throw new TypeError("Failed to construct 'IntersectionObserverEntry': 1 argument required, but only 0 present.");
    this.time = Number(init.time) || 0;
    this.rootBounds = init.rootBounds || null;
    this.boundingClientRect = init.boundingClientRect || rectOf(0, 0, 0, 0);
    this.intersectionRect = init.intersectionRect || rectOf(0, 0, 0, 0);
    this.isIntersecting = !!init.isIntersecting;
    this.intersectionRatio = Number(init.intersectionRatio) || 0;
    this.target = init.target;
  }
}
tag(IntersectionObserverEntry);

class IntersectionObserver {
  constructor(callback, options) {
    need(arguments, 1, 'IntersectionObserver', 'IntersectionObserver');
    if (typeof callback !== 'function') throw new TypeError("Failed to construct 'IntersectionObserver': The callback provided as parameter 1 is not a function.");
    const o = options === undefined || options === null ? {} : options;
    if (!isObj(o)) throw new TypeError("Failed to construct 'IntersectionObserver': The provided value is not of type 'IntersectionObserverInit'.");
    let root = o.root === undefined ? null : o.root;
    if (root !== null && !isElement(root) && !(root instanceof G.Document)) {
      throw new TypeError("Failed to construct 'IntersectionObserver': Failed to read the 'root' property from 'IntersectionObserverInit': The provided value is not of type '(Document or Element)'.");
    }
    const margin = parseMargin(o.rootMargin === undefined ? '0px' : o.rootMargin);
    if (margin === null) throw new DOMException("Failed to construct 'IntersectionObserver': Failed to parse rootMargin '" + o.rootMargin + "'.", 'SyntaxError');
    let th = o.threshold === undefined ? [0] : typeof o.threshold === 'object' && o.threshold !== null && typeof o.threshold[SymIter] === 'function' ? Array.from(o.threshold, Number) : [Number(o.threshold)];
    if (th.length === 0) th = [0];
    for (const t of th) {
      if (!(t >= 0 && t <= 1)) throw new RangeError("Failed to construct 'IntersectionObserver': Threshold values must be numbers between 0 and 1.");
    }
    th.sort((a, b) => a - b);
    setSlots(this, { brand: 'IntersectionObserver', cb: callback, root, margin, thresholds: th, targets: new Map(), queue: [], self: this });
  }
  get root() {
    return slots(this, 'IntersectionObserver').root;
  }
  get rootMargin() {
    const m = slots(this, 'IntersectionObserver').margin;
    const f = (x) => x.v + (x.pct ? '%' : 'px');
    return [m.top, m.right, m.bottom, m.left].map(f).join(' ');
  }
  get thresholds() {
    return slots(this, 'IntersectionObserver').thresholds.slice();
  }
  observe(target) {
    const s = slots(this, 'IntersectionObserver');
    need(arguments, 1, 'observe', 'IntersectionObserver');
    if (!isElement(target)) throw new TypeError("Failed to execute 'observe' on 'IntersectionObserver': parameter 1 is not of type 'Element'.");
    if (s.targets.has(target)) return;
    if (s.targets.size >= 20000) throw new DOMException('Too many elements are being observed.', 'QuotaExceededError');
    s.targets.set(target, { idx: -1, inter: null });
    ioSet.add(s);
    layoutSync();
    native.layoutWatch(true);
  }
  unobserve(target) {
    const s = slots(this, 'IntersectionObserver');
    s.targets.delete(target);
    s.queue = s.queue.filter((e) => e.target !== target);
    if (!s.targets.size) ioSet.delete(s);
    layoutSync();
  }
  disconnect() {
    const s = slots(this, 'IntersectionObserver');
    s.targets.clear();
    s.queue = [];
    ioSet.delete(s);
    layoutSync();
  }
  takeRecords() {
    const s = slots(this, 'IntersectionObserver');
    const q = s.queue;
    s.queue = [];
    return q;
  }
}
tag(IntersectionObserver);
enumerable(IntersectionObserver, ['root', 'rootMargin', 'thresholds', 'observe', 'unobserve', 'disconnect', 'takeRecords']);

function ioRootBox(s) {
  let x = 0;
  let y = 0;
  let w = G.innerWidth;
  let h = G.innerHeight;
  if (s.root !== null && isElement(s.root)) {
    const r = s.root.getBoundingClientRect();
    x = r.x;
    y = r.y;
    w = r.width;
    h = r.height;
  }
  const m = s.margin;
  const dx = (e, full) => (e.pct ? (e.v / 100) * full : e.v);
  const top = dx(m.top, h);
  const bottom = dx(m.bottom, h);
  const left = dx(m.left, w);
  const right = dx(m.right, w);
  return { x: x - left, y: y - top, w: w + left + right, h: h + top + bottom };
}

function ioCheck(s, now) {
  const root = ioRootBox(s);
  for (const [target, st] of s.targets) {
    const tr = target.getBoundingClientRect();
    const shown = rendered(target) && (s.root === null || !isElement(s.root) || s.root.contains(target));
    let inter = false;
    let ratio = 0;
    let ix = rectOf(0, 0, 0, 0);
    if (shown) {
      const l = Math.max(tr.left, root.x);
      const t = Math.max(tr.top, root.y);
      const r = Math.min(tr.right, root.x + root.w);
      const b = Math.min(tr.bottom, root.y + root.h);
      if (l <= r && t <= b) {
        inter = true;
        ix = rectOf(l, t, r - l, b - t);
        const area = tr.width * tr.height;
        ratio = area > 0 ? Math.min(1, ((r - l) * (b - t)) / area) : 1;
      }
    }
    let idx = 0;
    for (const t of s.thresholds) if (t <= ratio) idx++;
    if (!inter) idx = s.thresholds.length === 0 ? 0 : idx;
    if (idx === st.idx && inter === st.inter) continue;
    st.idx = idx;
    st.inter = inter;
    const e = Object.create(IntersectionObserverEntry.prototype);
    e.time = now;
    e.rootBounds = s.root === null || !isElement(s.root) ? rectOf(0, 0, G.innerWidth, G.innerHeight) : rectOf(root.x, root.y, root.w, root.h);
    e.boundingClientRect = rectOf(tr.x, tr.y, tr.width, tr.height);
    e.intersectionRect = ix;
    e.isIntersecting = inter;
    e.intersectionRatio = ratio;
    e.target = target;
    if (s.queue.length < MO_MAX_QUEUE) s.queue.push(e);
  }
}

class ResizeObserverEntry {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
}
tag(ResizeObserverEntry);

class ResizeObserver {
  constructor(callback) {
    need(arguments, 1, 'ResizeObserver', 'ResizeObserver');
    if (typeof callback !== 'function') throw new TypeError("Failed to construct 'ResizeObserver': The callback provided as parameter 1 is not a function.");
    setSlots(this, { brand: 'ResizeObserver', cb: callback, targets: new Map(), queue: [], self: this });
  }
  observe(target, options) {
    const s = slots(this, 'ResizeObserver');
    need(arguments, 1, 'observe', 'ResizeObserver');
    if (!isElement(target)) throw new TypeError("Failed to execute 'observe' on 'ResizeObserver': parameter 1 is not of type 'Element'.");
    let box = 'content-box';
    if (isObj(options) && options.box !== undefined) {
      box = str(options.box);
      if (box !== 'content-box' && box !== 'border-box' && box !== 'device-pixel-content-box') {
        throw new TypeError("Failed to execute 'observe' on 'ResizeObserver': Failed to read the 'box' property from 'ResizeObserverOptions': The provided value '" + box + "' is not a valid enum value of type ResizeObserverBoxOptions.");
      }
    }
    if (s.targets.size >= 20000) throw new DOMException('Too many elements are being observed.', 'QuotaExceededError');
    s.targets.set(target, { box, w: -1, h: -1 });
    roSet.add(s);
    layoutSync();
    native.layoutWatch(true);
  }
  unobserve(target) {
    const s = slots(this, 'ResizeObserver');
    s.targets.delete(target);
    if (!s.targets.size) roSet.delete(s);
    layoutSync();
  }
  disconnect() {
    const s = slots(this, 'ResizeObserver');
    s.targets.clear();
    s.queue = [];
    roSet.delete(s);
    layoutSync();
  }
}
tag(ResizeObserver);
enumerable(ResizeObserver, ['observe', 'unobserve', 'disconnect']);

function pxOf(cs, name) {
  const v = parseFloat(cs.getPropertyValue ? cs.getPropertyValue(name) : cs[name]);
  return v > 0 ? v : 0;
}

function roCheck(s) {
  for (const [target, st] of s.targets) {
    let bw = 0;
    let bh = 0;
    let pl = 0;
    let pt = 0;
    let cw = 0;
    let ch = 0;
    if (rendered(target)) {
      const r = target.getBoundingClientRect();
      bw = r.width;
      bh = r.height;
      let cs = null;
      try {
        cs = G.getComputedStyle(target);
      } catch (e) {}
      pl = cs ? pxOf(cs, 'padding-left') : 0;
      pt = cs ? pxOf(cs, 'padding-top') : 0;
      const pr = cs ? pxOf(cs, 'padding-right') : 0;
      const pb = cs ? pxOf(cs, 'padding-bottom') : 0;
      const bl = cs ? pxOf(cs, 'border-left-width') : 0;
      const br = cs ? pxOf(cs, 'border-right-width') : 0;
      const bt = cs ? pxOf(cs, 'border-top-width') : 0;
      const bb = cs ? pxOf(cs, 'border-bottom-width') : 0;
      pl += bl;
      pt += bt;
      cw = Math.max(0, bw - pl - pr - br);
      ch = Math.max(0, bh - pt - pb - bb);
    }
    const w = st.box === 'border-box' ? bw : cw;
    const h = st.box === 'border-box' ? bh : ch;
    if (w === st.w && h === st.h) continue;
    st.w = w;
    st.h = h;
    const e = Object.create(ResizeObserverEntry.prototype);
    e.target = target;
    e.contentRect = rectOf(pl, pt, cw, ch);
    const size = (iw, ih) => Object.freeze([Object.freeze({ inlineSize: iw, blockSize: ih })]);
    e.borderBoxSize = size(bw, bh);
    e.contentBoxSize = size(cw, ch);
    const dpr = G.devicePixelRatio || 1;
    e.devicePixelContentBoxSize = size(Math.round(cw * dpr), Math.round(ch * dpr));
    if (s.queue.length < MO_MAX_QUEUE) s.queue.push(e);
  }
}

// The host found the page laid out again, scrolled or resized: look at the boxes.
function checkLayout() {
  const now = G.performance.now();
  const io = Array.from(ioSet);
  const ro = Array.from(roSet);
  for (const s of io) ioCheck(s, now);
  for (const s of ro) roCheck(s);
  for (const s of ro) {
    const q = s.queue;
    if (!q.length) continue;
    s.queue = [];
    try {
      s.cb.call(s.self, q, s.self);
    } catch (e) {
      native.report(e);
    }
  }
  for (const s of io) {
    const q = s.queue;
    if (!q.length) continue;
    s.queue = [];
    try {
      s.cb.call(s.self, q, s.self);
    } catch (e) {
      native.report(e);
    }
  }
}
