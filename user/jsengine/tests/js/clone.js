test('structuredClone primitives and plain data', function () {
  assertEq(structuredClone(1), 1);
  assertEq(structuredClone('s'), 's');
  assertEq(structuredClone(null), null);
  assertEq(structuredClone(undefined), undefined);
  assertEq(structuredClone(-0), -0);
  assertEq(structuredClone(true), true);
  var src = { a: 1, b: [1, 2, { c: 3 }], d: 'x', e: null, f: undefined };
  var c = structuredClone(src);
  assert(c !== src && c.b !== src.b && c.b[2] !== src.b[2]);
  assertEq(JSON.stringify(c), JSON.stringify(src));
  assert('f' in c);
  assertEq(Object.getPrototypeOf(c), Object.prototype);
  src.b[2].c = 99;
  assertEq(c.b[2].c, 3);
  assertEq(structuredClone([1, , 3]).hasOwnProperty(1), false);
  assertEq(structuredClone([1, , 3]).length, 3);
  var arr = [1, 2]; arr.extra = 'e';
  assertEq(structuredClone(arr).extra, 'e');
  assertEq(Array.isArray(structuredClone(arr)), true);
});
test('structuredClone cycles and sharing', function () {
  var a = { name: 'a' }; a.self = a; a.list = [a];
  var c = structuredClone(a);
  assert(c.self === c && c.list[0] === c && c !== a);
  var shared = { x: 1 };
  var o = structuredClone({ p: shared, q: shared });
  assert(o.p === o.q && o.p !== shared);
  var m = new Map(); m.set('self', m);
  var cm = structuredClone(m);
  assert(cm.get('self') === cm);
});
test('structuredClone builtin types', function () {
  var d = new Date(1234567890123);
  var cd = structuredClone(d);
  assert(cd instanceof Date && cd !== d && cd.getTime() === 1234567890123);
  var re = /a+b/gi; re.lastIndex = 3;
  var cr = structuredClone(re);
  assertEq(cr.source, 'a+b'); assertEq(cr.flags, 'gi'); assertEq(cr.lastIndex, 0);
  var cs = structuredClone(new Set([1, 'a', { z: 1 }]));
  assertEq(cs.size, 3); assertEq(cs.has('a'), true);
  var cm = structuredClone(new Map([[1, { a: 1 }], ['k', [2]]]));
  assertEq(cm.get(1).a, 1); assertEq(cm.get('k')[0], 2);
  assertEq(typeof structuredClone(new Number(5)), 'object');
  assertEq(structuredClone(new Number(5)).valueOf(), 5);
  assertEq(structuredClone(new String('hi')).length, 2);
  assertEq(structuredClone(new Boolean(false)).valueOf(), false);
  var e = structuredClone(new RangeError('boom'));
  assert(e instanceof RangeError && e.message === 'boom' && e.name === 'RangeError');
  var e2 = structuredClone(new Error('top', { cause: new TypeError('inner') }));
  assert(e2.cause instanceof TypeError && e2.cause.message === 'inner');
  class Custom extends Error {}
  var e3 = structuredClone(new Custom('x'));
  assertEq(Object.getPrototypeOf(e3), Error.prototype);
});
test('structuredClone binary types', function () {
  var buf = new Uint8Array([1, 2, 3, 4]).buffer;
  var cb = structuredClone(buf);
  assert(cb !== buf && cb.byteLength === 4 && new Uint8Array(cb)[3] === 4);
  new Uint8Array(buf)[0] = 9;
  assertEq(new Uint8Array(cb)[0], 1);
  var f = new Float32Array([1.5, 2.5]);
  var cf = structuredClone(f);
  assert(cf instanceof Float32Array && cf !== f && cf.buffer !== f.buffer);
  assertEq(Array.from(cf), [1.5, 2.5]);
  var big = new Uint8Array(16);
  var view = big.subarray(4, 8);
  var both = structuredClone({ big: big, view: view });
  assert(both.view.buffer === both.big.buffer, 'a shared buffer stays shared');
  assertEq(both.view.byteOffset, 4); assertEq(both.view.length, 4);
  var dv = structuredClone(new DataView(new ArrayBuffer(8), 2, 4));
  assertEq(dv.byteLength, 4); assertEq(dv.byteOffset, 2);
  // transfer
  var t = new Uint8Array([5, 6, 7]);
  var moved = structuredClone(t, { transfer: [t.buffer] });
  assertEq(Array.from(moved), [5, 6, 7]);
  assertEq(t.length, 0); assertEq(t.buffer.byteLength, 0);
  var b2 = new ArrayBuffer(4);
  structuredClone(null, { transfer: [b2] });
  assertEq(b2.byteLength, 0);
  assertThrows(Error, function () { structuredClone(1, { transfer: [{}] }); });
  var b3 = new ArrayBuffer(2);
  assertThrows(Error, function () { structuredClone(1, { transfer: [b3, b3] }); });
  assertEq(b3.byteLength, 2);
  var detached = new ArrayBuffer(2); detached.transfer();
  assertThrows(Error, function () { structuredClone(detached); });
});
test('structuredClone failures', function () {
  var e = assertThrows(Error, function () { structuredClone(function () {}); });
  assertEq(e.name, 'DataCloneError');
  assertEq(assertThrows(Error, function () { structuredClone(Symbol('s')); }).name, 'DataCloneError');
  assertEq(assertThrows(Error, function () { structuredClone({ f: function () {} }); }).name, 'DataCloneError');
  assertEq(assertThrows(Error, function () { structuredClone([Promise.resolve(1)]); }).name, 'DataCloneError');
  assertEq(assertThrows(Error, function () { structuredClone(new WeakMap()); }).name, 'DataCloneError');
  assertThrows(TypeError, function () { structuredClone(); });
  var o = { get a() { return 5; }, get b() { throw new RangeError('getter'); } };
  assertThrows(RangeError, function () { structuredClone(o); });
  var o2 = { get a() { return 5; } };
  var c = structuredClone(o2);
  assertEq(Object.getOwnPropertyDescriptor(c, 'a').value, 5);
  // class instances become plain objects; symbol keys and non-enumerables are dropped
  class P { constructor() { this.x = 1; } m() {} }
  var cp = structuredClone(new P());
  assertEq(Object.getPrototypeOf(cp), Object.prototype); assertEq(cp.x, 1); assertEq(cp.m, undefined);
  var sk = { [Symbol('k')]: 1, v: 2 };
  Object.defineProperty(sk, 'hidden', { value: 3, enumerable: false });
  var cs = structuredClone(sk);
  assertEq(Object.getOwnPropertyNames(cs), ['v']);
  assertEq(Object.getOwnPropertySymbols(cs).length, 0);
});
test('structuredClone depth', function () {
  var deep = {};
  var cur = deep;
  for (var i = 0; i < 4000; i++) { cur.next = {}; cur = cur.next; }
  try { structuredClone(deep); } catch (e) { assert(e instanceof RangeError, 'deep object is a RangeError: ' + e); }
  var wide = [];
  for (var j = 0; j < 2000; j++) wide.push({ i: j });
  assertEq(structuredClone(wide)[1999].i, 1999);
});
