test('get, set, has, delete traps', function () {
  var log = [];
  var t = { a: 1 };
  var p = new Proxy(t, {
    get: function (o, k, rcv) { log.push('get:' + String(k)); return Reflect.get(o, k, rcv); },
    set: function (o, k, v, rcv) { log.push('set:' + String(k)); return Reflect.set(o, k, v, o); },
    has: function (o, k) { log.push('has:' + String(k)); return k in o; },
    deleteProperty: function (o, k) { log.push('del:' + String(k)); return delete o[k]; }
  });
  assertEq(p.a, 1);
  p.b = 2;
  assertEq(t.b, 2);
  assertEq('b' in p, true);
  assertEq('zz' in p, false);
  assertEq(delete p.a, true);
  assertEq(t.a, undefined);
  assertEq(log.join(','), 'get:a,set:b,has:b,has:zz,del:a');
});
test('no traps forwards to the target', function () {
  var t = { x: 1 };
  var p = new Proxy(t, {});
  p.y = 2;
  assertEq(t.y, 2);
  assertEq(p.x, 1);
  assertEq(Object.keys(p).join(), 'x,y');
  assertEq(JSON.stringify(p), '{"x":1,"y":2}');
  delete p.x;
  assertEq('x' in t, false);
});
test('ownKeys, getOwnPropertyDescriptor, defineProperty', function () {
  var t = { a: 1, b: 2 };
  var seen = [];
  var p = new Proxy(t, {
    ownKeys: function (o) { seen.push('keys'); return ['b', 'a']; },
    getOwnPropertyDescriptor: function (o, k) { seen.push('gopd:' + k); return Reflect.getOwnPropertyDescriptor(o, k); },
    defineProperty: function (o, k, d) { seen.push('def:' + k); return Reflect.defineProperty(o, k, d); }
  });
  assertEq(Object.keys(p).join(), 'b,a');
  Object.defineProperty(p, 'c', { value: 3, configurable: true, enumerable: true, writable: true });
  assertEq(t.c, 3);
  assert(seen.indexOf('def:c') >= 0);
  assert(seen.indexOf('gopd:a') >= 0);
  var names = [];
  for (var k in p) names.push(k);
  assertEq(names.join(), 'b,a');
});
test('prototype traps', function () {
  var proto = { inherited: 1 };
  var t = {};
  var p = new Proxy(t, {
    getPrototypeOf: function () { return proto; },
    setPrototypeOf: function (o, v) { o.__set = v; return true; }
  });
  assertEq(Object.getPrototypeOf(p), proto);
  assertEq(p.__proto__, proto);
  assert(proto.isPrototypeOf(p));
  assert(p instanceof Object);
  Object.setPrototypeOf(p, null);
  assertEq(t.__set, null);
});
test('apply and construct traps', function () {
  var f = function (a, b) { return a + b; };
  var p = new Proxy(f, {
    apply: function (t, thisArg, args) { return t.apply(thisArg, args) * 10; },
    construct: function (t, args, nt) { return { made: args.length }; }
  });
  assertEq(p(1, 2), 30);
  assertEq(p.call(null, 2, 3), 50);
  assertEq(p.apply(null, [1, 1]), 20);
  assertEq(new p(1, 2, 3).made, 3);
  assertEq(typeof p, 'function');
  assertEq(typeof new Proxy({}, {}), 'object');
  assertThrows(TypeError, function () { new Proxy({}, {})(); });
  assertThrows(TypeError, function () { new (new Proxy({}, {}))(); });
  var plain = new Proxy(function (x) { return x; }, {});
  assertEq(plain(7), 7);
  var C = new Proxy(class A { constructor(v) { this.v = v; } }, {});
  assertEq(new C(4).v, 4);
});
test('arrays through proxies', function () {
  var p = new Proxy([1, 2, 3], {});
  assertEq(Array.isArray(p), true);
  assertEq(p.length, 3);
  assertEq(JSON.stringify(p), '[1,2,3]');
  assertEq(Object.prototype.toString.call(p), '[object Array]');
  assertEq([0].concat(p).length, 4);
  p.push(4);
  assertEq(p.length, 4);
  assertEq(p.map(function (x) { return x * 2; }).join(), '2,4,6,8');
  assertEq(Array.isArray(new Proxy({}, {})), false);
  var sum = 0;
  for (var v of p) sum += v;
  assertEq(sum, 10);
});
test('reactive wrapper style', function () {
  var deps = [];
  function reactive(obj) {
    return new Proxy(obj, {
      get: function (t, k, r) {
        var v = Reflect.get(t, k, r);
        deps.push(String(k));
        return (v && typeof v === 'object') ? reactive(v) : v;
      },
      set: function (t, k, v, r) {
        var had = Object.prototype.hasOwnProperty.call(t, k);
        var ok = Reflect.set(t, k, v, r);
        deps.push((had ? 'upd:' : 'add:') + String(k));
        return ok;
      }
    });
  }
  var s = reactive({ n: { c: 1 }, list: [1] });
  s.n.c = 2;
  s.list.push(5);
  assertEq(s.n.c, 2);
  assertEq(s.list.length, 2);
  assert(deps.indexOf('upd:c') >= 0);
  assert(deps.indexOf('add:1') >= 0);
});
test('receiver is the proxy for inherited accessors', function () {
  var base = { get me() { return this; } };
  var p = new Proxy(Object.create(base), {});
  assertEq(p.me, p);
  var o = Object.create(p);
  o.q = 1;
  assertEq(Object.prototype.hasOwnProperty.call(o, 'q'), true);
});
test('revocable', function () {
  var r = Proxy.revocable({ a: 1 }, {});
  assertEq(r.proxy.a, 1);
  r.revoke();
  assertThrows(TypeError, function () { return r.proxy.a; });
  assertThrows(TypeError, function () { r.proxy.a = 1; });
  assertThrows(TypeError, function () { return 'a' in r.proxy; });
  assertThrows(TypeError, function () { Object.keys(r.proxy); });
  assertThrows(TypeError, function () { Array.isArray(r.proxy); });
  r.revoke();
  assertThrows(TypeError, function () { new Proxy(r.proxy, {}).x; });
});
test('trap results are checked', function () {
  var t = {};
  Object.defineProperty(t, 'fixed', { value: 1, writable: false, configurable: false });
  var p = new Proxy(t, {
    get: function () { return 2; },
    has: function () { return false; },
    deleteProperty: function () { return true; },
    ownKeys: function () { return []; },
    getPrototypeOf: function () { return Array.prototype; }
  });
  assertThrows(TypeError, function () { return p.fixed; });
  assertThrows(TypeError, function () { return 'fixed' in p; });
  assertThrows(TypeError, function () { delete p.fixed; });
  assertThrows(TypeError, function () { Reflect.ownKeys(p); });
  var f = Object.preventExtensions({});
  var q = new Proxy(f, { getPrototypeOf: function () { return Array.prototype; } });
  assertThrows(TypeError, function () { Object.getPrototypeOf(q); });
  var bad = new Proxy({}, { ownKeys: function () { return [1]; } });
  assertThrows(TypeError, function () { Reflect.ownKeys(bad); });
  var dup = new Proxy({}, { ownKeys: function () { return ['a', 'a']; } });
  assertThrows(TypeError, function () { Reflect.ownKeys(dup); });
  var ctor = new Proxy(function () { }, { construct: function () { return 1; } });
  assertThrows(TypeError, function () { new ctor(); });
});
test('traps that throw, and bad handlers', function () {
  var boom = new Proxy({}, { get: function () { throw new RangeError('boom'); } });
  assertThrows(RangeError, function () { return boom.x; });
  assertThrows(TypeError, function () { new Proxy(1, {}); });
  assertThrows(TypeError, function () { new Proxy({}, 1); });
  assertThrows(TypeError, function () { Proxy({}, {}); });
  var notfn = new Proxy({}, { get: 5 });
  assertThrows(TypeError, function () { return notfn.x; });
  var undef = new Proxy({ a: 3 }, { get: undefined });
  assertEq(undef.a, 3);
  var nul = new Proxy({ a: 3 }, { get: null });
  assertEq(nul.a, 3);
});
test('proxy as a prototype and in a chain', function () {
  var log = [];
  var p = new Proxy({}, { get: function (t, k, r) { log.push(String(k)); return k === 'known' ? 9 : undefined; } });
  var o = Object.create(p);
  assertEq(o.known, 9);
  assertEq(o.other, undefined);
  assertEq(log.join(), 'known,other');
  var deep = {};
  for (var i = 0; i < 200; i++) deep = new Proxy(deep, {});
  deep.v = 1;
  assertEq(deep.v, 1);
});
test('proxy loops stop with an error, not a crash', function () {
  var p = new Proxy({}, {});
  var h = { get: function (t, k) { return p[k]; } };
  p = new Proxy({}, h);
  assertThrows(RangeError, function () { return p.x; });
  var self = new Proxy({}, { has: function (t, k) { return k in self; } });
  assertThrows(RangeError, function () { return 'a' in self; });
});
test('Object.assign, spread, entries and freeze work through proxies', function () {
  var p = new Proxy({ a: 1, b: 2 }, {});
  assertEq(Object.assign({}, p).b, 2);
  assertEq(({ ...p }).a, 1);
  assertEq(Object.entries(p).length, 2);
  var { a } = p;
  assertEq(a, 1);
  Object.freeze(p);
  assertEq(Object.isFrozen(p), true);
  assertThrows(TypeError, function () { 'use strict'; p.a = 5; });
  assertEq(Object.isExtensible(p), false);
});
test('Reflect works with proxies and Proxy.name', function () {
  assertEq(Proxy.length, 2);
  assertEq(Proxy.name, 'Proxy');
  assertEq(Proxy.prototype, undefined);
  var p = new Proxy({ a: 1 }, {});
  assertEq(Reflect.has(p, 'a'), true);
  assertEq(Reflect.ownKeys(p).join(), 'a');
  assertEq(Reflect.deleteProperty(p, 'a'), true);
  assertEq(Reflect.getPrototypeOf(p), Object.prototype);
});
test('hostile traps stay contained', function () {
  var r = Proxy.revocable({ a: 1, b: 2 }, {});
  r.proxy = r.proxy;
  var h = {
    ownKeys: function (t) { r.revoke(); return ['a', 'b']; },
    get: function (t, k) { r.revoke(); return 1; }
  };
  var q = new Proxy(r.proxy, h);
  assertThrows(TypeError, function () { Object.keys(q); });
  var big = new Proxy({}, { ownKeys: function () { var a = []; for (var i = 0; i < 20000; i++) a.push('k' + i); return a; } });
  assertEq(Reflect.ownKeys(big).length, 20000);
  var growing = new Proxy([], { get: function (t, k, rc) { return k === 'length' ? 4294967295 : 1; } });
  var n = 0;
  try { Array.prototype.slice.call(growing, 0, 5); n++; } catch (e) { n++; }
  assertEq(n, 1);
  var fn = new Proxy(function () { }, { apply: function (t, th, a) { return fn(); } });
  assertThrows(RangeError, function () { fn(); });
  var tr = new Proxy({}, { set: function () { return false; } });
  assertEq(Reflect.set(tr, 'x', 1), false);
  assertThrows(TypeError, function () { 'use strict'; tr.x = 1; });
  tr.x = 1;
  assertEq(tr.x, undefined);
});
