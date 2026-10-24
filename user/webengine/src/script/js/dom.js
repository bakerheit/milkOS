// ---- DOMParser, XMLSerializer, DOMImplementation, TreeWalker, NodeIterator, Range, Selection, ---------
// ---- custom elements registry, document.fonts ----------------------------------------------------------
// All of it is script over the DOM the page already has. Documents made by DOMParser and
// createHTMLDocument hold their nodes in the page's own DOM under a detached holder (`native.
// parseMarkup`), so they belong to the page's document as far as `ownerDocument` goes; the
// document object in front of them answers the Document members pages use on parsed markup.

const doc = () => G.document;
const kids = (n) => Array.from(n.childNodes);
const isCharData = (n) => n.nodeType === 3 || n.nodeType === 4 || n.nodeType === 7 || n.nodeType === 8;
const nodeLen = (n) => (n.nodeType === 10 ? 0 : isCharData(n) ? n.data.length : n.childNodes.length);
function indexOf(n) {
  let i = 0;
  while ((n = n.previousSibling)) i++;
  return i;
}
function rootOf(n) {
  while (n.parentNode) n = n.parentNode;
  return n;
}
function isAncestor(a, b) {
  for (let n = b; n; n = n.parentNode) if (n === a) return true;
  return false;
}
const domEx = (name, msg) => new DOMException(msg, name);

// ---- parsed documents ---------------------------------------------------------------------------------

const HTML_TYPES = ['text/html'];
const XML_TYPES = ['text/xml', 'application/xml', 'application/xhtml+xml', 'image/svg+xml'];

class ParsedDocument extends G.Document {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
}
{
  const proto = ParsedDocument.prototype;
  const def = (name, get, set) => ObjDefine(proto, name, { get, set, configurable: true, enumerable: true });
  const val = (name, f) => ObjDefine(proto, name, { value: f, writable: true, configurable: true, enumerable: true });
  const sl = (d) => slots(d, 'ParsedDocument');
  const top = (d) => kids(sl(d).holder).filter((n) => n.nodeType === 1);
  def('nodeType', () => 9);
  def('nodeName', () => '#document');
  def('nodeValue', () => null);
  def('textContent', () => null);
  def('ownerDocument', () => null);
  def('parentNode', () => null);
  def('isConnected', () => true);
  def('contentType', function () { return sl(this).type; });
  def('characterSet', () => 'UTF-8');
  def('charset', () => 'UTF-8');
  def('inputEncoding', () => 'UTF-8');
  def('compatMode', function () { return sl(this).doctype !== null || !sl(this).html ? 'CSS1Compat' : 'BackCompat'; });
  def('URL', () => 'about:blank');
  def('documentURI', () => 'about:blank');
  def('referrer', () => '');
  def('cookie', () => '');
  def('domain', () => '');
  def('defaultView', () => null);
  def('readyState', () => 'complete');
  def('hidden', () => true);
  def('visibilityState', () => 'hidden');
  def('location', () => null);
  def('implementation', () => domImpl);
  def('doctype', function () {
    const s = sl(this);
    return s.doctype === null ? null : makeDoctype(s.doctype, '', '');
  });
  def('documentElement', function () { return top(this)[0] || null; });
  def('head', function () {
    const h = top(this)[0];
    return (h && kids(h).find((n) => n.nodeType === 1 && n.localName === 'head')) || null;
  });
  def('body', function () {
    const h = top(this)[0];
    return (h && kids(h).find((n) => n.nodeType === 1 && (n.localName === 'body' || n.localName === 'frameset'))) || null;
  });
  def('title', function () {
    const t = this.documentElement && this.documentElement.getElementsByTagName('title')[0];
    return t ? t.textContent.replace(/[\t\n\f\r ]+/g, ' ').trim() : '';
  }, function (v) {
    const root = this.documentElement;
    if (!root) return;
    let t = root.getElementsByTagName('title')[0];
    if (!t) {
      const head = this.head;
      if (!head) return;
      t = doc().createElement('title');
      head.appendChild(t);
    }
    t.textContent = String(v);
  });
  def('childNodes', function () {
    const s = sl(this);
    const out = [];
    if (s.doctype !== null) out.push(this.doctype);
    return out.concat(kids(s.holder));
  });
  def('firstChild', function () { return this.childNodes[0] || null; });
  def('lastChild', function () { const c = this.childNodes; return c[c.length - 1] || null; });
  def('children', function () { return top(this); });
  def('firstElementChild', function () { return top(this)[0] || null; });
  def('lastElementChild', function () { const t = top(this); return t[t.length - 1] || null; });
  def('childElementCount', function () { return top(this).length; });
  def('activeElement', () => null);
  def('forms', function () { return Array.from(this.querySelectorAll('form')); });
  def('images', function () { return Array.from(this.querySelectorAll('img')); });
  def('links', function () { return Array.from(this.querySelectorAll('a[href],area[href]')); });
  def('scripts', function () { return Array.from(this.querySelectorAll('script')); });
  val('hasChildNodes', function () { return this.childNodes.length > 0; });
  val('contains', function (n) { return n === this || (isObj(n) && n.nodeType && isAncestor(sl(this).holder, n) && n !== sl(this).holder); });
  val('querySelector', function (s) { return sl(this).holder.querySelector(s); });
  val('querySelectorAll', function (s) { return sl(this).holder.querySelectorAll(s); });
  val('getElementById', function (id) {
    id = String(id);
    const walk = (n) => {
      for (const c of kids(n)) {
        if (c.nodeType !== 1) continue;
        if (c.id === id) return c;
        const f = walk(c);
        if (f) return f;
      }
      return null;
    };
    return walk(sl(this).holder);
  });
  val('getElementsByTagName', function (name) {
    const root = this.documentElement;
    if (!root) return [];
    const nm = String(name).toLowerCase();
    const all = [root].concat(Array.from(root.getElementsByTagName('*')));
    return nm === '*' ? all : all.filter((e) => e.localName.toLowerCase() === nm);
  });
  val('getElementsByClassName', function (name) {
    const root = this.documentElement;
    return root ? Array.from(root.getElementsByClassName(name)).concat(root.classList.contains(String(name)) ? [root] : []) : [];
  });
  for (const m of ['createElement', 'createElementNS', 'createTextNode', 'createComment', 'createDocumentFragment', 'createEvent', 'importNode', 'adoptNode', 'createTreeWalker', 'createNodeIterator', 'createRange']) {
    val(m, function () { return doc()[m].apply(doc(), arguments); });
  }
  val('getSelection', () => null);
  val('hasFocus', () => false);
  val('write', () => {});
  val('writeln', () => {});
  val('open', function () { return this; });
  val('close', () => {});
  val('cloneNode', function () { throw domEx('NotSupportedError', 'Cloning a parsed document is not supported.'); });
  ObjDefine(proto, TAG, { get() { return slots(this, 'ParsedDocument').html ? 'HTMLDocument' : 'XMLDocument'; }, configurable: true });
}
function makeDoctype(name, pub, sys) {
  const o = { nodeType: 10, nodeName: name, name, publicId: pub, systemId: sys, ownerDocument: null, parentNode: null, childNodes: [], textContent: null };
  ObjDefine(o, TAG, { value: 'DocumentType', configurable: true });
  return o;
}
function newParsedDocument(holder, doctype, type) {
  const d = Object.create(ParsedDocument.prototype);
  setSlots(d, { brand: 'ParsedDocument', holder, doctype, type, html: type === 'text/html' });
  return d;
}

class DOMParser {
  constructor() {}
  parseFromString(string, type) {
    need(arguments, 2, 'parseFromString', 'DOMParser');
    string = String(string);
    type = String(type).toLowerCase();
    if (HTML_TYPES.indexOf(type) < 0 && XML_TYPES.indexOf(type) < 0) {
      throw new TypeError("Failed to execute 'parseFromString' on 'DOMParser': The provided value '" + type + "' is not a valid enum value of type SupportedType.");
    }
    if (type === 'text/html') {
      const r = native.parseMarkup(string, true);
      return newParsedDocument(r[0], r[1], type);
    }
    // XML: read with the HTML parser (tag and attribute names come lower-cased, there are no
    // parse error documents).
    const text = string.replace(/^\s*<\?xml[^>]*\?>/, '').replace(/^\s*<!DOCTYPE[^>]*>/i, '');
    const r = native.parseMarkup(text, false);
    return newParsedDocument(r[0], null, type);
  }
}
tag(DOMParser);
enumerable(DOMParser, ['parseFromString']);

const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']);
const XHTML = 'http://www.w3.org/1999/xhtml';
const escText = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const escAttr = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/\t/g, '&#9;').replace(/\n/g, '&#10;').replace(/\r/g, '&#13;');
function serializeXml(n, parentNs, out, depth) {
  if (depth > 500) throw domEx('InvalidStateError', 'The node is nested too deeply to serialize.');
  switch (n.nodeType) {
    case 1: {
      const ns = n.namespaceURI || null;
      const name = n.localName;
      out.push('<' + name);
      if (ns !== parentNs) out.push(' xmlns="' + escAttr(ns || '') + '"');
      for (const a of Array.from(n.attributes || [])) {
        if (a.name === 'xmlns') continue;
        out.push(' ' + a.name + '="' + escAttr(a.value) + '"');
      }
      const html = ns === XHTML;
      const cs = kids(n.localName === 'template' && n.content ? n.content : n);
      if (cs.length === 0 && (html ? VOID.has(name) : true)) {
        out.push(html ? ' />' : '/>');
        return;
      }
      out.push('>');
      for (const c of cs) serializeXml(c, ns, out, depth + 1);
      out.push('</' + name + '>');
      return;
    }
    case 3:
      out.push(escText(n.data));
      return;
    case 4:
      out.push('<![CDATA[' + n.data + ']]>');
      return;
    case 8:
      out.push('<!--' + n.data + '-->');
      return;
    case 10:
      out.push('<!DOCTYPE ' + n.name + (n.publicId ? ' PUBLIC "' + n.publicId + '"' + (n.systemId ? ' "' + n.systemId + '"' : '') : n.systemId ? ' SYSTEM "' + n.systemId + '"' : '') + '>');
      return;
    case 9:
    case 11:
      for (const c of Array.from(n.childNodes)) serializeXml(c, parentNs, out, depth + 1);
      return;
    default:
      return;
  }
}
class XMLSerializer {
  constructor() {}
  serializeToString(node) {
    need(arguments, 1, 'serializeToString', 'XMLSerializer');
    if (!isObj(node) || typeof node.nodeType !== 'number') {
      throw new TypeError("Failed to execute 'serializeToString' on 'XMLSerializer': parameter 1 is not of type 'Node'.");
    }
    const out = [];
    serializeXml(node, node.nodeType === 1 ? undefined : null, out, 0);
    return out.join('');
  }
}
tag(XMLSerializer);
enumerable(XMLSerializer, ['serializeToString']);

class DOMImplementation {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  createHTMLDocument(title) {
    let html = '<!DOCTYPE html><html><head></head><body></body></html>';
    if (title !== undefined) html = '<!DOCTYPE html><html><head><title>' + escText(String(title)) + '</title></head><body></body></html>';
    const r = native.parseMarkup(html, true);
    return newParsedDocument(r[0], r[1], 'text/html');
  }
  createDocument(ns, qname, doctype) {
    const r = native.parseMarkup('', false);
    const d = newParsedDocument(r[0], doctype ? doctype.name : null, ns === XHTML ? 'application/xhtml+xml' : ns === 'http://www.w3.org/2000/svg' ? 'image/svg+xml' : 'application/xml');
    if (qname !== null && qname !== undefined && String(qname) !== '') r[0].appendChild(doc().createElementNS(ns, String(qname)));
    return d;
  }
  createDocumentType(name, pub, sys) {
    need(arguments, 3, 'createDocumentType', 'DOMImplementation');
    return makeDoctype(String(name), String(pub), String(sys));
  }
  hasFeature() {
    return true;
  }
}
tag(DOMImplementation);
enumerable(DOMImplementation, ['createDocumentType', 'createDocument', 'createHTMLDocument', 'hasFeature']);
const domImpl = Object.create(DOMImplementation.prototype);

// ---- TreeWalker, NodeIterator, NodeFilter ---------------------------------------------------------------

const NodeFilter = {};
ObjDefine(NodeFilter, TAG, { value: 'NodeFilter', configurable: true });
for (const [k, v] of Object.entries({
  FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
  SHOW_ALL: 0xffffffff, SHOW_ELEMENT: 1, SHOW_ATTRIBUTE: 2, SHOW_TEXT: 4, SHOW_CDATA_SECTION: 8,
  SHOW_ENTITY_REFERENCE: 16, SHOW_ENTITY: 32, SHOW_PROCESSING_INSTRUCTION: 64, SHOW_COMMENT: 128,
  SHOW_DOCUMENT: 256, SHOW_DOCUMENT_TYPE: 512, SHOW_DOCUMENT_FRAGMENT: 1024, SHOW_NOTATION: 2048,
})) ObjDefine(NodeFilter, k, { value: v, enumerable: true });

function accept(s, node) {
  if (s.active) throw domEx('InvalidStateError', 'The filter is already running.');
  const bit = node.nodeType - 1;
  if (bit < 0 || bit > 31 || ((s.what >>> bit) & 1) === 0) return 3;
  const f = s.filter;
  if (f === null) return 1;
  s.active = true;
  try {
    const r = typeof f === 'function' ? f.call(undefined, node) : f.acceptNode(node);
    return Number(r) & 0xffff;
  } finally {
    s.active = false;
  }
}
function checkRoot(root, what) {
  if (!isObj(root) || typeof root.nodeType !== 'number') {
    throw new TypeError("Failed to execute '" + what + "' on 'Document': parameter 1 is not of type 'Node'.");
  }
}
function filterArg(f) {
  if (f === undefined || f === null) return null;
  if (typeof f === 'function' || (isObj(f) && typeof f.acceptNode === 'function')) return f;
  throw new TypeError('The provided callback is not a function.');
}

class TreeWalker {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get root() { return slots(this, 'TreeWalker').root; }
  get whatToShow() { return slots(this, 'TreeWalker').what; }
  get filter() { return slots(this, 'TreeWalker').filter; }
  get currentNode() { return slots(this, 'TreeWalker').cur; }
  set currentNode(n) {
    if (!isObj(n) || typeof n.nodeType !== 'number') throw new TypeError("Failed to set the 'currentNode' property on 'TreeWalker': The provided value is not of type 'Node'.");
    slots(this, 'TreeWalker').cur = n;
  }
  parentNode() {
    const s = slots(this, 'TreeWalker');
    let n = s.cur;
    while (n && n !== s.root) {
      n = n.parentNode;
      if (n && accept(s, n) === 1) return (s.cur = n);
    }
    return null;
  }
  firstChild() { return twChild(slots(this, 'TreeWalker'), true); }
  lastChild() { return twChild(slots(this, 'TreeWalker'), false); }
  nextSibling() { return twSibling(slots(this, 'TreeWalker'), true); }
  previousSibling() { return twSibling(slots(this, 'TreeWalker'), false); }
  nextNode() {
    const s = slots(this, 'TreeWalker');
    let node = s.cur;
    let result = 1;
    for (;;) {
      while (result !== 2 && node.firstChild) {
        node = node.firstChild;
        result = accept(s, node);
        if (result === 1) return (s.cur = node);
      }
      let sib = null;
      let t = node;
      while (t) {
        if (t === s.root) return null;
        sib = t.nextSibling;
        if (sib) {
          node = sib;
          break;
        }
        t = t.parentNode;
      }
      if (!sib) return null;
      result = accept(s, node);
      if (result === 1) return (s.cur = node);
    }
  }
  previousNode() {
    const s = slots(this, 'TreeWalker');
    let node = s.cur;
    while (node !== s.root) {
      let sib = node.previousSibling;
      while (sib) {
        node = sib;
        let result = accept(s, node);
        while (result !== 2 && node.lastChild) {
          node = node.lastChild;
          result = accept(s, node);
        }
        if (result === 1) return (s.cur = node);
        sib = node.previousSibling;
      }
      if (node === s.root || !node.parentNode) return null;
      node = node.parentNode;
      if (accept(s, node) === 1) return (s.cur = node);
    }
    return null;
  }
}
tag(TreeWalker);
enumerable(TreeWalker, ['root', 'whatToShow', 'filter', 'currentNode', 'parentNode', 'firstChild', 'lastChild', 'previousSibling', 'nextSibling', 'previousNode', 'nextNode']);
function twChild(s, first) {
  let node = first ? s.cur.firstChild : s.cur.lastChild;
  while (node) {
    const r = accept(s, node);
    if (r === 1) return (s.cur = node);
    if (r === 3) {
      const c = first ? node.firstChild : node.lastChild;
      if (c) {
        node = c;
        continue;
      }
    }
    while (node) {
      const sib = first ? node.nextSibling : node.previousSibling;
      if (sib) {
        node = sib;
        break;
      }
      const p = node.parentNode;
      if (!p || p === s.root || p === s.cur) return null;
      node = p;
    }
  }
  return null;
}
function twSibling(s, next) {
  let node = s.cur;
  if (node === s.root) return null;
  for (;;) {
    let sib = next ? node.nextSibling : node.previousSibling;
    while (sib) {
      node = sib;
      const r = accept(s, node);
      if (r === 1) return (s.cur = node);
      sib = next ? node.firstChild : node.lastChild;
      if (r === 2 || !sib) sib = next ? node.nextSibling : node.previousSibling;
    }
    node = node.parentNode;
    if (!node || node === s.root) return null;
    if (accept(s, node) === 1) return null;
  }
}

class NodeIterator {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get root() { return slots(this, 'NodeIterator').root; }
  get whatToShow() { return slots(this, 'NodeIterator').what; }
  get filter() { return slots(this, 'NodeIterator').filter; }
  get referenceNode() { return slots(this, 'NodeIterator').ref; }
  get pointerBeforeReferenceNode() { return slots(this, 'NodeIterator').before; }
  nextNode() { return niStep(slots(this, 'NodeIterator'), true); }
  previousNode() { return niStep(slots(this, 'NodeIterator'), false); }
  detach() {}
}
tag(NodeIterator);
enumerable(NodeIterator, ['root', 'referenceNode', 'pointerBeforeReferenceNode', 'whatToShow', 'filter', 'nextNode', 'previousNode', 'detach']);
function following(node, root) {
  if (node.firstChild) return node.firstChild;
  for (let n = node; n && n !== root; n = n.parentNode) if (n.nextSibling) return n.nextSibling;
  return null;
}
function preceding(node, root) {
  if (node === root) return null;
  let sib = node.previousSibling;
  if (sib) {
    while (sib.lastChild) sib = sib.lastChild;
    return sib;
  }
  return node.parentNode;
}
function niStep(s, next) {
  let node = s.ref;
  let before = s.before;
  for (;;) {
    if (next) {
      if (!before) {
        node = following(node, s.root);
        if (!node) return null;
      } else {
        before = false;
      }
    } else if (before) {
      node = preceding(node, s.root);
      if (!node) return null;
    } else {
      before = true;
    }
    if (accept(s, node) === 1) break;
  }
  s.ref = node;
  s.before = before;
  return node;
}
function createTreeWalker(root, what, filter) {
  need(arguments, 1, 'createTreeWalker', 'Document');
  checkRoot(root, 'createTreeWalker');
  const t = Object.create(TreeWalker.prototype);
  setSlots(t, { brand: 'TreeWalker', root, cur: root, what: what === undefined ? 0xffffffff : what >>> 0, filter: filterArg(filter), active: false });
  return t;
}
function createNodeIterator(root, what, filter) {
  need(arguments, 1, 'createNodeIterator', 'Document');
  checkRoot(root, 'createNodeIterator');
  const t = Object.create(NodeIterator.prototype);
  setSlots(t, { brand: 'NodeIterator', root, ref: root, before: true, what: what === undefined ? 0xffffffff : what >>> 0, filter: filterArg(filter), active: false });
  return t;
}

// ---- Range and Selection ---------------------------------------------------------------------------------

// -1, 0, 1: where point A lies relative to point B.
function cmp(an, ao, bn, bo) {
  if (an === bn) return ao === bo ? 0 : ao < bo ? -1 : 1;
  if (isAncestor(bn, an)) return -cmp(bn, bo, an, ao);
  if (isAncestor(an, bn)) {
    let c = bn;
    while (c.parentNode !== an) c = c.parentNode;
    return indexOf(c) < ao ? 1 : -1;
  }
  return an.compareDocumentPosition(bn) & 4 ? -1 : 1;
}
function checkPoint(node, offset, what) {
  if (!isObj(node) || typeof node.nodeType !== 'number') throw new TypeError("Failed to execute '" + what + "' on 'Range': parameter 1 is not of type 'Node'.");
  if (node.nodeType === 10) throw domEx('InvalidNodeTypeError', "Failed to execute '" + what + "' on 'Range': The node provided is a doctype.");
  offset = offset >>> 0;
  if (offset > nodeLen(node)) throw domEx('IndexSizeError', "Failed to execute '" + what + "' on 'Range': The offset " + offset + ' is larger than the node\'s length (' + nodeLen(node) + ').');
  return offset;
}

// Copy, move or delete what lies between two points (the DOM's "extract" algorithm).
function extractBetween(sc, so, ec, eo, mode) {
  const frag = mode === 'delete' ? null : doc().createDocumentFragment();
  if (sc === ec && so === eo) return frag;
  if (sc === ec && isCharData(sc)) {
    if (frag) {
      const c = sc.cloneNode(false);
      c.data = sc.data.substring(so, eo);
      frag.appendChild(c);
    }
    if (mode !== 'clone') sc.data = sc.data.substring(0, so) + sc.data.substring(eo);
    return frag;
  }
  let common = sc;
  while (!isAncestor(common, ec)) common = common.parentNode;
  const partial = (n) => isAncestor(n, sc) !== isAncestor(n, ec);
  const contained = (n) => cmp(n, 0, sc, so) > 0 && cmp(n, nodeLen(n), ec, eo) < 0;
  const cs = kids(common);
  const firstPartial = isAncestor(sc, ec) ? null : cs.find(partial) || null;
  const lastPartial = isAncestor(ec, sc) ? null : cs.slice().reverse().find(partial) || null;
  const inside = cs.filter(contained);
  if (inside.some((n) => n.nodeType === 10)) throw domEx('HierarchyRequestError', 'The range contains a doctype.');
  if (firstPartial) {
    if (isCharData(firstPartial)) {
      if (frag) {
        const c = firstPartial.cloneNode(false);
        c.data = firstPartial.data.substring(so);
        frag.appendChild(c);
      }
      if (mode !== 'clone') firstPartial.data = firstPartial.data.substring(0, so);
    } else {
      if (frag) {
        const c = firstPartial.cloneNode(false);
        frag.appendChild(c);
        c.appendChild(extractBetween(sc, so, firstPartial, nodeLen(firstPartial), mode));
      } else {
        extractBetween(sc, so, firstPartial, nodeLen(firstPartial), mode);
      }
    }
  }
  for (const n of inside) {
    if (mode === 'clone') frag.appendChild(n.cloneNode(true));
    else if (mode === 'extract') frag.appendChild(n);
    else n.parentNode.removeChild(n);
  }
  if (lastPartial) {
    if (isCharData(lastPartial)) {
      if (frag) {
        const c = lastPartial.cloneNode(false);
        c.data = lastPartial.data.substring(0, eo);
        frag.appendChild(c);
      }
      if (mode !== 'clone') lastPartial.data = lastPartial.data.substring(eo);
    } else if (frag) {
      const c = lastPartial.cloneNode(false);
      frag.appendChild(c);
      c.appendChild(extractBetween(lastPartial, 0, ec, eo, mode));
    } else {
      extractBetween(lastPartial, 0, ec, eo, mode);
    }
  }
  return frag;
}
function afterRemoval(s) {
  // Where the range collapses to after its contents went.
  if (isAncestor(s.sc, s.ec)) return [s.sc, s.so];
  let ref = s.sc;
  while (ref.parentNode && !isAncestor(ref.parentNode, s.ec)) ref = ref.parentNode;
  return [ref.parentNode, indexOf(ref) + 1];
}
const rangeSet = new Set(); // live ranges (weakly held ones are not available; bounded)

class AbstractRange {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
}
class Range {
  constructor() {
    setSlots(this, { brand: 'Range', sc: doc(), so: 0, ec: doc(), eo: 0 });
  }
  get startContainer() { return slots(this, 'Range').sc; }
  get startOffset() { return slots(this, 'Range').so; }
  get endContainer() { return slots(this, 'Range').ec; }
  get endOffset() { return slots(this, 'Range').eo; }
  get collapsed() { const s = slots(this, 'Range'); return s.sc === s.ec && s.so === s.eo; }
  get commonAncestorContainer() {
    const s = slots(this, 'Range');
    let c = s.sc;
    while (!isAncestor(c, s.ec)) c = c.parentNode;
    return c;
  }
  setStart(node, offset) {
    need(arguments, 2, 'setStart', 'Range');
    const s = slots(this, 'Range');
    offset = checkPoint(node, offset, 'setStart');
    s.sc = node;
    s.so = offset;
    if (rootOf(s.ec) !== rootOf(node) || cmp(node, offset, s.ec, s.eo) > 0) {
      s.ec = node;
      s.eo = offset;
    }
    changed();
  }
  setEnd(node, offset) {
    need(arguments, 2, 'setEnd', 'Range');
    const s = slots(this, 'Range');
    offset = checkPoint(node, offset, 'setEnd');
    s.ec = node;
    s.eo = offset;
    if (rootOf(s.sc) !== rootOf(node) || cmp(node, offset, s.sc, s.so) < 0) {
      s.sc = node;
      s.so = offset;
    }
    changed();
  }
  setStartBefore(n) { this.setStart(parentOf(n, 'setStartBefore'), indexOf(n)); }
  setStartAfter(n) { this.setStart(parentOf(n, 'setStartAfter'), indexOf(n) + 1); }
  setEndBefore(n) { this.setEnd(parentOf(n, 'setEndBefore'), indexOf(n)); }
  setEndAfter(n) { this.setEnd(parentOf(n, 'setEndAfter'), indexOf(n) + 1); }
  collapse(toStart) {
    const s = slots(this, 'Range');
    if (toStart) {
      s.ec = s.sc;
      s.eo = s.so;
    } else {
      s.sc = s.ec;
      s.so = s.eo;
    }
    changed();
  }
  selectNode(n) {
    need(arguments, 1, 'selectNode', 'Range');
    const s = slots(this, 'Range');
    const p = parentOf(n, 'selectNode');
    const i = indexOf(n);
    s.sc = s.ec = p;
    s.so = i;
    s.eo = i + 1;
    changed();
  }
  selectNodeContents(n) {
    need(arguments, 1, 'selectNodeContents', 'Range');
    if (n.nodeType === 10) throw domEx('InvalidNodeTypeError', "Failed to execute 'selectNodeContents' on 'Range': The node provided is a doctype.");
    const s = slots(this, 'Range');
    s.sc = s.ec = n;
    s.so = 0;
    s.eo = nodeLen(n);
    changed();
  }
  cloneRange() {
    const s = slots(this, 'Range');
    const r = new Range();
    Object.assign(slots(r, 'Range'), { sc: s.sc, so: s.so, ec: s.ec, eo: s.eo });
    return r;
  }
  detach() {}
  compareBoundaryPoints(how, source) {
    need(arguments, 2, 'compareBoundaryPoints', 'Range');
    const a = slots(this, 'Range');
    const b = slots(source, 'Range');
    if (how > 3 || how < 0) throw domEx('NotSupportedError', "Failed to execute 'compareBoundaryPoints' on 'Range': The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'.");
    if (rootOf(a.sc) !== rootOf(b.sc)) throw domEx('WrongDocumentError', 'The two Ranges are not in the same tree.');
    const [tn, to] = how === 0 || how === 3 ? [a.sc, a.so] : [a.ec, a.eo];
    const [on, oo] = how === 0 || how === 1 ? [b.sc, b.so] : [b.ec, b.eo];
    // START_TO_END (1): this end vs source start. END_TO_START (3): this start vs source end.
    if (how === 1) return cmp(a.ec, a.eo, b.sc, b.so);
    if (how === 3) return cmp(a.sc, a.so, b.ec, b.eo);
    return cmp(tn, to, on, oo);
  }
  isPointInRange(node, offset) {
    const s = slots(this, 'Range');
    if (rootOf(node) !== rootOf(s.sc)) return false;
    offset = checkPoint(node, offset, 'isPointInRange');
    return cmp(node, offset, s.sc, s.so) >= 0 && cmp(node, offset, s.ec, s.eo) <= 0;
  }
  comparePoint(node, offset) {
    const s = slots(this, 'Range');
    if (rootOf(node) !== rootOf(s.sc)) throw domEx('WrongDocumentError', 'The node is not in the same tree as the Range.');
    offset = checkPoint(node, offset, 'comparePoint');
    if (cmp(node, offset, s.sc, s.so) < 0) return -1;
    if (cmp(node, offset, s.ec, s.eo) > 0) return 1;
    return 0;
  }
  intersectsNode(node) {
    const s = slots(this, 'Range');
    if (rootOf(node) !== rootOf(s.sc)) return false;
    const p = node.parentNode;
    if (!p) return true;
    const i = indexOf(node);
    return cmp(p, i, s.ec, s.eo) < 0 && cmp(p, i + 1, s.sc, s.so) > 0;
  }
  toString() {
    const s = slots(this, 'Range');
    if (s.sc === s.ec && isCharData(s.sc)) return s.sc.data.substring(s.so, s.eo);
    let out = '';
    const walk = (n) => {
      if (n.nodeType === 3) {
        const from = n === s.sc ? s.so : 0;
        const to = n === s.ec ? s.eo : n.data.length;
        if (cmp(n, to, s.sc, s.so) > 0 && cmp(n, from, s.ec, s.eo) < 0) out += n.data.substring(from, to);
      }
      for (const c of kids(n)) walk(c);
    };
    let common = s.sc;
    while (!isAncestor(common, s.ec)) common = common.parentNode;
    walk(common);
    return out;
  }
  cloneContents() {
    const s = slots(this, 'Range');
    return extractBetween(s.sc, s.so, s.ec, s.eo, 'clone');
  }
  extractContents() {
    const s = slots(this, 'Range');
    const [n, o] = afterRemoval(s);
    const f = extractBetween(s.sc, s.so, s.ec, s.eo, 'extract');
    s.sc = s.ec = n;
    s.so = s.eo = o;
    changed();
    return f;
  }
  deleteContents() {
    const s = slots(this, 'Range');
    const [n, o] = afterRemoval(s);
    extractBetween(s.sc, s.so, s.ec, s.eo, 'delete');
    s.sc = s.ec = n;
    s.so = s.eo = o;
    changed();
  }
  insertNode(node) {
    need(arguments, 1, 'insertNode', 'Range');
    const s = slots(this, 'Range');
    if (s.sc.nodeType === 8 || (s.sc.nodeType === 3 && !s.sc.parentNode)) throw domEx('HierarchyRequestError', "Failed to execute 'insertNode' on 'Range': Nodes of type '" + node.nodeName + "' may not be inserted inside nodes of type '" + s.sc.nodeName + "'.");
    let ref = s.sc.nodeType === 3 ? s.sc : s.sc.childNodes[s.so] || null;
    const parent = s.sc.nodeType === 3 ? s.sc.parentNode : s.sc;
    if (s.sc.nodeType === 3) ref = s.sc.splitText(s.so);
    const count = node.nodeType === 11 ? node.childNodes.length : 1;
    const at = (ref === null ? parent.childNodes.length : indexOf(ref)) + count;
    const collapsed = s.sc === s.ec && s.so === s.eo;
    parent.insertBefore(node, ref);
    if (collapsed) {
      s.ec = parent;
      s.eo = at;
    }
    changed();
  }
  surroundContents(newParent) {
    need(arguments, 1, 'surroundContents', 'Range');
    const f = this.extractContents();
    while (newParent.firstChild) newParent.removeChild(newParent.firstChild);
    this.insertNode(newParent);
    newParent.appendChild(f);
    this.selectNode(newParent);
  }
  createContextualFragment(html) {
    const t = doc().createElement('template');
    t.innerHTML = String(html);
    return t.content;
  }
  getBoundingClientRect() {
    const s = slots(this, 'Range');
    let el = null;
    if (s.sc === s.ec && s.sc.nodeType === 1 && s.eo - s.so === 1) el = s.sc.childNodes[s.so];
    else if (!(s.sc === s.ec && s.so === s.eo)) {
      let c = this.commonAncestorContainer;
      el = c.nodeType === 1 ? c : c.parentElement;
    }
    if (el && el.nodeType === 1 && el.isConnected) return el.getBoundingClientRect();
    return new DOMRect(0, 0, 0, 0);
  }
  getClientRects() {
    const r = this.getBoundingClientRect();
    return r.width || r.height ? [r] : [];
  }
}
function parentOf(n, what) {
  if (!isObj(n) || !n.parentNode) throw domEx('InvalidNodeTypeError', "Failed to execute '" + what + "' on 'Range': the given Node has no parent.");
  return n.parentNode;
}
tag(AbstractRange);
tag(Range);
Object.setPrototypeOf(Range, AbstractRange);
Object.setPrototypeOf(Range.prototype, AbstractRange.prototype);
for (const [k, v] of Object.entries({ START_TO_START: 0, START_TO_END: 1, END_TO_END: 2, END_TO_START: 3 })) {
  ObjDefine(Range, k, { value: v, enumerable: true });
  ObjDefine(Range.prototype, k, { value: v, enumerable: true });
}
enumerable(Range, ['startContainer', 'startOffset', 'endContainer', 'endOffset', 'collapsed', 'commonAncestorContainer', 'setStart', 'setEnd', 'setStartBefore', 'setStartAfter', 'setEndBefore', 'setEndAfter', 'collapse', 'selectNode', 'selectNodeContents', 'cloneRange', 'detach', 'compareBoundaryPoints', 'isPointInRange', 'comparePoint', 'intersectsNode', 'cloneContents', 'extractContents', 'deleteContents', 'insertNode', 'surroundContents', 'createContextualFragment', 'getBoundingClientRect', 'getClientRects']);
function createRange() {
  return new Range();
}

let selectionPending = false;
function changed() {
  if (selectionPending || !selectionObj) return;
  selectionPending = true;
  nextTick(() => {
    selectionPending = false;
    try {
      doc().dispatchEvent(new G.Event('selectionchange'));
    } catch (e) {
      native.report(e);
    }
  });
}
let selectionObj = null;
class Selection {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get rangeCount() { return slots(this, 'Selection').range ? 1 : 0; }
  get isCollapsed() { const r = slots(this, 'Selection').range; return !r || r.collapsed; }
  get type() { const r = slots(this, 'Selection').range; return !r ? 'None' : r.collapsed ? 'Caret' : 'Range'; }
  get direction() { const s = slots(this, 'Selection'); return !s.range || s.range.collapsed ? 'none' : s.backward ? 'backward' : 'forward'; }
  get anchorNode() { const s = slots(this, 'Selection'); return s.range ? (s.backward ? s.range.endContainer : s.range.startContainer) : null; }
  get anchorOffset() { const s = slots(this, 'Selection'); return s.range ? (s.backward ? s.range.endOffset : s.range.startOffset) : 0; }
  get focusNode() { const s = slots(this, 'Selection'); return s.range ? (s.backward ? s.range.startContainer : s.range.endContainer) : null; }
  get focusOffset() { const s = slots(this, 'Selection'); return s.range ? (s.backward ? s.range.startOffset : s.range.endOffset) : 0; }
  get baseNode() { return this.anchorNode; }
  get baseOffset() { return this.anchorOffset; }
  get extentNode() { return this.focusNode; }
  get extentOffset() { return this.focusOffset; }
  getRangeAt(i) {
    const s = slots(this, 'Selection');
    if (!s.range || i !== 0) throw domEx('IndexSizeError', "Failed to execute 'getRangeAt' on 'Selection': " + i + ' is not a valid index.');
    return s.range;
  }
  addRange(r) {
    need(arguments, 1, 'addRange', 'Selection');
    const s = slots(this, 'Selection');
    slots(r, 'Range');
    if (!s.range && rootOf(r.startContainer) === doc()) {
      s.range = r;
      s.backward = false;
      changed();
    }
  }
  removeRange(r) {
    const s = slots(this, 'Selection');
    if (s.range !== r) throw domEx('NotFoundError', "Failed to execute 'removeRange' on 'Selection': The given range isn't in the selection.");
    s.range = null;
    changed();
  }
  removeAllRanges() {
    slots(this, 'Selection').range = null;
    changed();
  }
  empty() { this.removeAllRanges(); }
  collapse(node, offset) {
    const s = slots(this, 'Selection');
    if (node === null || node === undefined) return this.removeAllRanges();
    const o = checkPoint(node, offset === undefined ? 0 : offset, 'collapse');
    const r = new Range();
    r.setStart(node, o);
    r.collapse(true);
    s.range = r;
    s.backward = false;
    changed();
  }
  setPosition(node, offset) { this.collapse(node, offset); }
  collapseToStart() {
    const s = slots(this, 'Selection');
    if (!s.range) throw domEx('InvalidStateError', "Failed to execute 'collapseToStart' on 'Selection': there is no selection.");
    s.range.collapse(true);
    s.backward = false;
  }
  collapseToEnd() {
    const s = slots(this, 'Selection');
    if (!s.range) throw domEx('InvalidStateError', "Failed to execute 'collapseToEnd' on 'Selection': there is no selection.");
    s.range.collapse(false);
    s.backward = false;
  }
  extend(node, offset) {
    const s = slots(this, 'Selection');
    if (!s.range) throw domEx('InvalidStateError', "Failed to execute 'extend' on 'Selection': This Selection object doesn't have any Ranges.");
    const o = checkPoint(node, offset === undefined ? 0 : offset, 'extend');
    const ar = s.backward ? [s.range.endContainer, s.range.endOffset] : [s.range.startContainer, s.range.startOffset];
    const r = new Range();
    if (cmp(node, o, ar[0], ar[1]) < 0) {
      r.setStart(node, o);
      r.setEnd(ar[0], ar[1]);
      s.backward = true;
    } else {
      r.setStart(ar[0], ar[1]);
      r.setEnd(node, o);
      s.backward = false;
    }
    s.range = r;
    changed();
  }
  setBaseAndExtent(an, ao, fn, fo) {
    need(arguments, 4, 'setBaseAndExtent', 'Selection');
    const s = slots(this, 'Selection');
    const a = checkPoint(an, ao, 'setBaseAndExtent');
    const f = checkPoint(fn, fo, 'setBaseAndExtent');
    const r = new Range();
    if (cmp(an, a, fn, f) <= 0) {
      r.setStart(an, a);
      r.setEnd(fn, f);
      s.backward = false;
    } else {
      r.setStart(fn, f);
      r.setEnd(an, a);
      s.backward = true;
    }
    s.range = r;
    changed();
  }
  selectAllChildren(node) {
    need(arguments, 1, 'selectAllChildren', 'Selection');
    const r = new Range();
    r.selectNodeContents(node);
    const s = slots(this, 'Selection');
    s.range = r;
    s.backward = false;
    changed();
  }
  deleteFromDocument() {
    const s = slots(this, 'Selection');
    if (s.range) s.range.deleteContents();
  }
  containsNode(node, partial) {
    const s = slots(this, 'Selection');
    if (!s.range || rootOf(node) !== rootOf(s.range.startContainer)) return false;
    const r = s.range;
    const p = node.parentNode;
    if (!p) return true;
    const i = indexOf(node);
    const startsBefore = cmp(p, i, r.startContainer, r.startOffset) >= 0;
    const endsAfter = cmp(p, i + 1, r.endContainer, r.endOffset) <= 0;
    return partial ? r.intersectsNode(node) : startsBefore && endsAfter;
  }
  modify() {}
  toString() {
    const r = slots(this, 'Selection').range;
    return r ? r.toString() : '';
  }
}
tag(Selection);
enumerable(Selection, ['anchorNode', 'anchorOffset', 'focusNode', 'focusOffset', 'isCollapsed', 'rangeCount', 'type', 'direction', 'getRangeAt', 'addRange', 'removeRange', 'removeAllRanges', 'empty', 'collapse', 'setPosition', 'collapseToStart', 'collapseToEnd', 'extend', 'setBaseAndExtent', 'selectAllChildren', 'deleteFromDocument', 'containsNode', 'modify']);
function getSelection() {
  if (!selectionObj) {
    selectionObj = Object.create(Selection.prototype);
    setSlots(selectionObj, { brand: 'Selection', range: null, backward: false });
  }
  return selectionObj;
}

// Text.splitText and the CharacterData data methods.
function splitText(offset) {
  offset = offset >>> 0;
  const len = this.data.length;
  if (offset > len) throw domEx('IndexSizeError', "Failed to execute 'splitText' on 'Text': The offset " + offset + ' is larger than the Text node\'s length (' + len + ').');
  const tail = this.data.substring(offset);
  const n = doc().createTextNode(tail);
  if (this.parentNode) this.parentNode.insertBefore(n, this.nextSibling);
  this.data = this.data.substring(0, offset);
  return n;
}
function dataCheck(node, offset, what) {
  offset = offset >>> 0;
  if (offset > node.data.length) throw domEx('IndexSizeError', "Failed to execute '" + what + "' on 'CharacterData': The offset " + offset + ' is greater than the node\'s length (' + node.data.length + ').');
  return offset;
}
function substringData(offset, count) {
  need(arguments, 2, 'substringData', 'CharacterData');
  offset = dataCheck(this, offset, 'substringData');
  return this.data.substring(offset, offset + (count >>> 0));
}
function insertData(offset, d) {
  need(arguments, 2, 'insertData', 'CharacterData');
  offset = dataCheck(this, offset, 'insertData');
  this.data = this.data.substring(0, offset) + String(d) + this.data.substring(offset);
}
function deleteData(offset, count) {
  need(arguments, 2, 'deleteData', 'CharacterData');
  offset = dataCheck(this, offset, 'deleteData');
  this.data = this.data.substring(0, offset) + this.data.substring(offset + (count >>> 0));
}
function replaceData(offset, count, d) {
  need(arguments, 3, 'replaceData', 'CharacterData');
  offset = dataCheck(this, offset, 'replaceData');
  this.data = this.data.substring(0, offset) + String(d) + this.data.substring(offset + (count >>> 0));
}

// ---- custom elements, document.fonts ------------------------------------------------------------------------

const ceDefs = new Map();
const ceWaiting = new Map();
const CE_NAME = /^[a-z][-.0-9_a-z·À-ÖØ-öø-ͽͿ-῿‌‍‿⁀⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�]*-[-.0-9_a-z·À-ÖØ-öø-ͽͿ-῿‌‍‿⁀⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�]*$/;
const CE_RESERVED = new Set(['annotation-xml', 'color-profile', 'font-face', 'font-face-src', 'font-face-uri', 'font-face-format', 'font-face-name', 'missing-glyph']);
class CustomElementRegistry {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  define(name, ctor, options) {
    need(arguments, 2, 'define', 'CustomElementRegistry');
    name = String(name);
    if (typeof ctor !== 'function') throw new TypeError("Failed to execute 'define' on 'CustomElementRegistry': The callback provided as parameter 2 is not a constructor.");
    if (!CE_NAME.test(name) || CE_RESERVED.has(name)) throw domEx('SyntaxError', "Failed to execute 'define' on 'CustomElementRegistry': \"" + name + '" is not a valid custom element name');
    if (ceDefs.has(name)) throw domEx('NotSupportedError', "Failed to execute 'define' on 'CustomElementRegistry': the name \"" + name + '" has already been used with this registry');
    for (const d of ceDefs.values()) {
      if (d.ctor === ctor) throw domEx('NotSupportedError', "Failed to execute 'define' on 'CustomElementRegistry': this constructor has already been used with this registry");
    }
    if (ceDefs.size >= 4096) throw domEx('QuotaExceededError', 'Too many custom elements are defined.');
    ceDefs.set(name, { ctor, options });
    const w = ceWaiting.get(name);
    if (w) {
      ceWaiting.delete(name);
      w.resolve(ctor);
    }
  }
  get(name) {
    const d = ceDefs.get(String(name));
    return d ? d.ctor : undefined;
  }
  getName(ctor) {
    for (const [k, d] of ceDefs) if (d.ctor === ctor) return k;
    return null;
  }
  whenDefined(name) {
    name = String(name);
    if (!CE_NAME.test(name)) return Promise.reject(domEx('SyntaxError', "Failed to execute 'whenDefined' on 'CustomElementRegistry': \"" + name + '" is not a valid custom element name'));
    const d = ceDefs.get(name);
    if (d) return Promise.resolve(d.ctor);
    let w = ceWaiting.get(name);
    if (!w) {
      w = {};
      w.promise = new Promise((res) => { w.resolve = res; });
      ceWaiting.set(name, w);
    }
    return w.promise;
  }
  upgrade() {}
}
tag(CustomElementRegistry);
enumerable(CustomElementRegistry, ['define', 'get', 'getName', 'whenDefined', 'upgrade']);
const customElements = Object.create(CustomElementRegistry.prototype);

class FontFace {
  constructor(family, source, descriptors) {
    need(arguments, 2, 'FontFace', 'FontFace');
    const d = isObj(descriptors) ? descriptors : {};
    setSlots(this, { brand: 'FontFace', family: String(family), source, status: 'unloaded' });
    for (const k of ['style', 'weight', 'stretch', 'unicodeRange', 'variant', 'featureSettings', 'display']) {
      ObjDefine(this, k, { value: d[k] === undefined ? (k === 'style' || k === 'weight' || k === 'stretch' ? 'normal' : k === 'unicodeRange' ? 'U+0-10FFFF' : k === 'display' ? 'auto' : 'normal') : String(d[k]), writable: true, enumerable: true, configurable: true });
    }
    const s = this[S];
    s.loaded = new Promise((res) => { s.res = res; });
    s.loaded.catch(() => {});
  }
  get family() { return slots(this, 'FontFace').family; }
  set family(v) { slots(this, 'FontFace').family = String(v); }
  get status() { return slots(this, 'FontFace').status; }
  get loaded() { return slots(this, 'FontFace').loaded; }
  load() {
    const s = slots(this, 'FontFace');
    s.status = 'loaded';
    s.res(this);
    return s.loaded;
  }
}
tag(FontFace);
class FontFaceSet extends NetEventTarget {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get size() { return slots(this, 'FontFaceSet').faces.size; }
  get status() { return 'loaded'; }
  get ready() { return slots(this, 'FontFaceSet').ready; }
  add(f) { slots(this, 'FontFaceSet').faces.add(f); return this; }
  delete(f) { return slots(this, 'FontFaceSet').faces.delete(f); }
  has(f) { return slots(this, 'FontFaceSet').faces.has(f); }
  clear() { slots(this, 'FontFaceSet').faces.clear(); }
  check() { return true; }
  load() { return Promise.resolve([]); }
  forEach(cb, thisArg) { slots(this, 'FontFaceSet').faces.forEach((f) => cb.call(thisArg, f, f, this)); }
  values() { return slots(this, 'FontFaceSet').faces.values(); }
  keys() { return slots(this, 'FontFaceSet').faces.values(); }
  entries() { return slots(this, 'FontFaceSet').faces.entries(); }
  [SymIter]() { return slots(this, 'FontFaceSet').faces.values(); }
}
tag(FontFaceSet);
handlerAttrs(FontFaceSet, ['loading', 'loadingdone', 'loadingerror']);
function makeFonts() {
  const f = Object.create(FontFaceSet.prototype);
  setSlots(f, { brand: 'FontFaceSet', faces: new Set(), ready: Promise.resolve(undefined) });
  return f;
}
const documentFonts = makeFonts();

// ---- BroadcastChannel (within the page's own scripts), Notification, Audio ------------------------------------

const bcChannels = new Map(); // name -> channels of this realm
let bcCount = 0;
class BroadcastChannel extends NetEventTarget {
  constructor(name) {
    super();
    need(arguments, 1, 'BroadcastChannel', 'BroadcastChannel');
    if (bcCount >= 1000) throw domEx('QuotaExceededError', 'Too many broadcast channels are open.');
    setSlots(this, { brand: 'BroadcastChannel', name: String(name), closed: false });
    let set = bcChannels.get(this[S].name);
    if (!set) bcChannels.set(this[S].name, (set = new Set()));
    set.add(this);
    bcCount++;
  }
  get name() { return slots(this, 'BroadcastChannel').name; }
  postMessage(message) {
    need(arguments, 1, 'postMessage', 'BroadcastChannel');
    const s = slots(this, 'BroadcastChannel');
    if (s.closed) throw domEx('InvalidStateError', "Failed to execute 'postMessage' on 'BroadcastChannel': Channel is closed");
    const set = bcChannels.get(s.name);
    if (!set) return;
    const copies = [];
    for (const ch of Array.from(set)) if (ch !== this) copies.push([ch, G.structuredClone(message)]);
    for (const [ch, data] of copies) {
      nextTick(() => {
        if (ch[S].closed) return;
        fire(ch, native.messageEvent('message', data, []));
      });
    }
  }
  close() {
    const s = slots(this, 'BroadcastChannel');
    if (s.closed) return;
    s.closed = true;
    const set = bcChannels.get(s.name);
    if (set) {
      set.delete(this);
      if (!set.size) bcChannels.delete(s.name);
    }
    bcCount--;
  }
}
tag(BroadcastChannel);
handlerAttrs(BroadcastChannel, ['message', 'messageerror']);
enumerable(BroadcastChannel, ['name', 'postMessage', 'close']);

// Notifications are not shown: the permission is "denied", as it is for a page the user never
// allowed, so pages that ask for it go on without.
class Notification extends NetEventTarget {
  constructor(title, options) {
    super();
    need(arguments, 1, 'Notification', 'Notification');
    const o = isObj(options) ? options : {};
    setSlots(this, { brand: 'Notification', title: String(title), body: o.body === undefined ? '' : String(o.body), tag: o.tag === undefined ? '' : String(o.tag) });
    nextTick(() => fire(this, new G.Event('error')));
  }
  static get permission() { return 'denied'; }
  static get maxActions() { return 2; }
  static requestPermission(callback) {
    if (typeof callback === 'function') nextTick(() => callback('denied'));
    return Promise.resolve('denied');
  }
  get title() { return slots(this, 'Notification').title; }
  get body() { return slots(this, 'Notification').body; }
  get tag() { return slots(this, 'Notification').tag; }
  close() {}
}
tag(Notification);
handlerAttrs(Notification, ['click', 'close', 'error', 'show']);

// `new Audio(src)`: an <audio> element that cannot play (there is no sound output).
function Audio(src) {
  if (new.target === undefined) throw new TypeError("Failed to construct 'Audio': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
  const a = doc().createElement('audio');
  if (src !== undefined) a.setAttribute('src', String(src));
  const def = (k, v) => { if (!(k in a) || typeof a[k] !== 'function') ObjDefine(a, k, { value: v, writable: true, configurable: true }); };
  def('play', function () { return Promise.reject(domEx('NotSupportedError', 'The media could not be played: there is no audio output.')); });
  def('pause', function () {});
  def('load', function () {});
  def('canPlayType', function () { return ''; });
  for (const [k, v] of Object.entries({ paused: true, ended: false, muted: false, volume: 1, currentTime: 0, duration: NaN, readyState: 0, networkState: 0, loop: false, autoplay: false, controls: false, playbackRate: 1 })) {
    if (!(k in a)) ObjDefine(a, k, { value: v, writable: true, configurable: true, enumerable: true });
  }
  return a;
}
