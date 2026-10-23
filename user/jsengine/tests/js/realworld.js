// Patterns page scripts use.
test('event emitter', function () {
  class Emitter {
    constructor() { this.handlers = new Map(); }
    on(type, fn) { if (!this.handlers.has(type)) this.handlers.set(type, []); this.handlers.get(type).push(fn); return this; }
    off(type, fn) { var list = this.handlers.get(type) || []; this.handlers.set(type, list.filter(f => f !== fn)); }
    emit(type, ...args) { (this.handlers.get(type) || []).slice().forEach(fn => fn.apply(this, args)); }
    once(type, fn) { var self = this; function wrapper(...a) { self.off(type, wrapper); fn.apply(this, a); } return this.on(type, wrapper); }
  }
  var e = new Emitter(), log = [];
  e.on('x', v => log.push('a' + v)).once('x', v => log.push('once' + v));
  e.emit('x', 1);
  e.emit('x', 2);
  assertEq(log, ['a1', 'once1', 'a2']);
});
test('debounce and memoize', function () {
  var calls = 0;
  function memo(fn) { var cache = {}; return function (n) { return n in cache ? cache[n] : (cache[n] = fn(n)); }; }
  var slowSquare = memo(function (n) { calls++; return n * n; });
  slowSquare(4); slowSquare(4); slowSquare(5);
  assertEq(calls, 2);
  var timers = [];
  function setTimeoutFake(f, ms) { timers.push(f); return timers.length; }
  function debounce(fn, ms) { var t = null; return function (...args) { if (t) timers[t - 1] = null; t = setTimeoutFake(() => fn.apply(this, args), ms); }; }
  var fired = [];
  var d = debounce(v => fired.push(v), 10);
  d(1); d(2); d(3);
  timers.forEach(f => f && f());
  assertEq(fired, [3]);
});
test('templating and escaping', function () {
  var data = { title: 'Milk & <Cookies>', items: [{ n: 'a', p: 1.5 }, { n: 'b', p: 20 }] };
  function esc(s) { return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]); }
  var html = `<h1>${esc(data.title)}</h1><ul>${data.items.map(i => `<li data-n="${i.n}">${i.n}: $${i.p.toFixed(2)}</li>`).join('')}</ul>`;
  assertEq(html, '<h1>Milk &amp; &lt;Cookies&gt;</h1><ul><li data-n="a">a: $1.50</li><li data-n="b">b: $20.00</li></ul>');
  var tpl = 'Hello {{name}}, you have {{count}} new {{what}}';
  var out = tpl.replace(/\{\{(\w+)\}\}/g, (m, k) => ({ name: 'Ann', count: 3 })[k] ?? m);
  assertEq(out, 'Hello Ann, you have 3 new {{what}}');
});
test('query strings and urls', function () {
  function parseQuery(q) {
    return q.replace(/^\?/, '').split('&').filter(Boolean).reduce(function (acc, pair) {
      var [k, v = ''] = pair.split('=').map(decodeURIComponent);
      (acc[k] = acc[k] || []).push(v);
      return acc;
    }, {});
  }
  var q = parseQuery('?a=1&b=two%20words&a=3&empty=&flag');
  assertEq(q.a, ['1', '3']);
  assertEq(q.b, ['two words']);
  assertEq(q.empty, ['']);
  assertEq(q.flag, ['']);
  function build(o) { return Object.entries(o).map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v)).join('&'); }
  assertEq(build({ x: 'a b', 'y&z': 'é' }), 'x=a%20b&y%26z=%C3%A9');
  var m = 'https://host.example:8080/path/to/page.html?x=1#frag'.match(/^(\w+):\/\/([^/:]+)(?::(\d+))?(\/[^?#]*)?(\?[^#]*)?(#.*)?$/);
  assertEq([m[1], m[2], m[3], m[4], m[5], m[6]], ['https', 'host.example', '8080', '/path/to/page.html', '?x=1', '#frag']);
});
test('grouping sorting formatting', function () {
  var people = [
    { name: 'Zoe', dept: 'eng', salary: 120000 },
    { name: 'adam', dept: 'ops', salary: 95000.5 },
    { name: 'Bea', dept: 'eng', salary: 130000 },
  ];
  var byDept = people.reduce((g, p) => { (g[p.dept] ||= []).push(p.name); return g; }, {});
  assertEq(byDept.eng, ['Zoe', 'Bea']);
  var sorted = people.slice().sort((a, b) => a.name.localeCompare(b.name)).map(p => p.name);
  assertEq(sorted, ['adam', 'Bea', 'Zoe']);
  var total = people.reduce((s, p) => s + p.salary, 0);
  assertEq(total.toLocaleString(), '345,000.5');
  assertEq((0.1 + 0.2).toFixed(2), '0.30');
  assertEq(Math.round(2.675 * 100) / 100, 2.68);
  var pct = (17 / 23 * 100).toFixed(1) + '%';
  assertEq(pct, '73.9%');
  function pad(n) { return String(n).padStart(2, '0'); }
  var d = new Date(Date.UTC(2026, 0, 5, 9, 7, 3));
  assertEq(`${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`, '2026-01-05 09:07');
});
test('deep clone, merge and equality', function () {
  var src = { a: 1, b: { c: [1, 2, { d: 'x' }] }, when: '2026-01-01' };
  var clone = JSON.parse(JSON.stringify(src));
  clone.b.c[2].d = 'y';
  assertEq(src.b.c[2].d, 'x');
  function merge(t, s) { for (var k of Object.keys(s)) { if (s[k] && typeof s[k] === 'object' && !Array.isArray(s[k])) { t[k] = merge(t[k] || {}, s[k]); } else t[k] = s[k]; } return t; }
  var m = merge({ a: { x: 1 }, list: [1] }, { a: { y: 2 }, list: [2, 3] });
  assertEq(JSON.stringify(m), '{"a":{"x":1,"y":2},"list":[2,3]}');
  function deepEqual(a, b) {
    if (a === b) return true;
    if (typeof a !== 'object' || typeof b !== 'object' || !a || !b) return false;
    var ka = Object.keys(a), kb = Object.keys(b);
    return ka.length === kb.length && ka.every(k => deepEqual(a[k], b[k]));
  }
  assert(deepEqual({ x: [1, { y: 2 }] }, { x: [1, { y: 2 }] }));
  assert(!deepEqual({ x: [1] }, { x: [2] }));
});
test('form validation style checks', function () {
  var rules = {
    username: v => /^[a-z][a-z0-9_]{2,15}$/i.test(v) || 'bad username',
    zip: v => /^\d{5}(-\d{4})?$/.test(v) || 'bad zip',
    phone: v => /^\(?\d{3}\)?[-. ]?\d{3}[-. ]?\d{4}$/.test(v) || 'bad phone',
    password: v => (v.length >= 8 && /[A-Z]/.test(v) && /\d/.test(v)) || 'weak',
  };
  function validate(form) { return Object.entries(rules).map(([k, f]) => f(form[k])).filter(r => r !== true); }
  assertEq(validate({ username: 'milk_1', zip: '12345-6789', phone: '(555) 123-4567', password: 'Passw0rdX' }), []);
  assertEq(validate({ username: '1bad', zip: '1234', phone: '555', password: 'short' }), ['bad username', 'bad zip', 'bad phone', 'weak']);
});
test('state machine and classes with private-by-convention state', function () {
  class Tabs {
    constructor(names) { this._names = names; this._active = 0; this._listeners = []; }
    get active() { return this._names[this._active]; }
    select(name) {
      var i = this._names.indexOf(name);
      if (i < 0) throw new RangeError('no tab ' + name);
      if (i === this._active) return false;
      var prev = this.active;
      this._active = i;
      this._listeners.forEach(l => l(prev, name));
      return true;
    }
    onChange(f) { this._listeners.push(f); }
  }
  var t = new Tabs(['home', 'about', 'contact']);
  var changes = [];
  t.onChange((a, b) => changes.push(a + '>' + b));
  t.select('about');
  t.select('about');
  t.select('contact');
  assertEq(changes, ['home>about', 'about>contact']);
  assertThrows(RangeError, () => t.select('nope'));
  assertEq(t.active, 'contact');
});
test('array utilities', function () {
  var chunk = (a, n) => Array.from({ length: Math.ceil(a.length / n) }, (_, i) => a.slice(i * n, i * n + n));
  assertEq(chunk([1, 2, 3, 4, 5], 2), [[1, 2], [3, 4], [5]]);
  var uniqBy = (a, f) => [...new Map(a.map(x => [f(x), x])).values()];
  assertEq(uniqBy([{ id: 1, v: 'a' }, { id: 2, v: 'b' }, { id: 1, v: 'c' }], x => x.id).map(x => x.v), ['c', 'b']);
  var zip = (...arrs) => arrs[0].map((_, i) => arrs.map(a => a[i]));
  assertEq(zip([1, 2], ['a', 'b']), [[1, 'a'], [2, 'b']]);
  var range = (s, e, st = 1) => Array.from({ length: Math.max(0, Math.ceil((e - s) / st)) }, (_, i) => s + i * st);
  assertEq(range(0, 10, 3), [0, 3, 6, 9]);
  var flatDeep = a => a.reduce((acc, x) => acc.concat(Array.isArray(x) ? flatDeep(x) : x), []);
  assertEq(flatDeep([1, [2, [3, [4]]]]), [1, 2, 3, 4]);
  var counts = [...'mississippi'].reduce((m, c) => m.set(c, (m.get(c) || 0) + 1), new Map());
  assertEq([...counts].map(([c, n]) => c + n).join(''), 'm1i4s4p2');
});
