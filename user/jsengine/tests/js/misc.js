test('optional chaining', function () {
  var o = { a: { b: { c: 1 } }, f() { return this.a; }, arr: [1] };
  assertEq(o?.a?.b?.c, 1);
  assertEq(o.x?.y.z, undefined);
  assertEq(o.f?.().b.c, 1);
  assertEq(o.nope?.(), undefined);
  assertEq(o.arr?.[0], 1);
  var n = null;
  assertEq(n?.x, undefined);
  assertEq(n?.[0], undefined);
  assertEq(n?.x.y.z(), undefined);
  assertEq((o?.a).b.c, 1);
  assertEq(delete n?.x, true);
  assertEq(delete o?.arr, true);
  assertEq(o.arr, undefined);
  assertThrows(TypeError, function () { return o.x.y; });
  var count = 0;
  var obj = { get g() { count++; return { h: 1 }; } };
  obj?.g?.h;
  assertEq(count, 1);
});
test('getters and dynamic access', function () {
  var o = { ['x' + 1]: 1, [`t${2}`]: 2 };
  assertEq(o.x1 + o.t2, 3);
  var methods = { a() { return 'A'; } };
  var name = 'a';
  assertEq(methods[name](), 'A');
});
test('reflect', function () {
  var o = { a: 1 };
  assertEq(Reflect.get(o, 'a'), 1);
  assert(Reflect.set(o, 'b', 2));
  assertEq(o.b, 2);
  assert(Reflect.has(o, 'b'));
  assertEq(Reflect.ownKeys({ x: 1, [Symbol.iterator]: 0 }).length, 2);
  assert(Reflect.deleteProperty(o, 'b'));
  assertEq(Reflect.apply(Math.max, null, [1, 3]), 3);
  function C(x) { this.x = x; }
  assertEq(Reflect.construct(C, [5]).x, 5);
  assertEq(Reflect.getPrototypeOf([]), Array.prototype);
  assert(Reflect.defineProperty(o, 'z', { value: 1 }));
  assert(!Reflect.defineProperty(Object.freeze({}), 'z', { value: 1 }));
});
test('getter receiver through prototype chain', function () {
  class Base { get kind() { return this.constructor.name; } }
  class Sub extends Base {}
  assertEq(new Sub().kind, 'Sub');
});
test('globalThis and var', function () {
  var before = typeof globalThis.__miscVar;
  assertEq(before, 'undefined');
  globalThis.eval('var __miscVar = 1');
  assertEq(globalThis.__miscVar, 1);
  assertEq(Object.getOwnPropertyDescriptor(globalThis, '__miscVar').configurable, false);
  globalThis.eval('let __lexical = 2');
  assertEq(globalThis.eval('__lexical'), 2);
  assertEq(globalThis.__lexical, undefined);
  assertThrows(SyntaxError, function () { globalThis.eval('let __lexical = 3'); });
});
test('arguments object', function () {
  function f(a) { a = 2; return arguments[0]; }
  assertEq(f(1), 1);
  function g() { return typeof arguments[Symbol.iterator]; }
  assertEq(g(), 'function');
  function h() { return [...arguments].join(''); }
  assertEq(h('a', 'b'), 'ab');
  function len() { return arguments.length; }
  assertEq(len(undefined, undefined), 2);
  var arrow = () => typeof arguments;
  function wrap() { return arrow(); }
  assertEq(typeof wrap(), 'string');
});
test('label and block function edge cases', function () {
  var r = [];
  a: b: for (var i = 0; i < 3; i++) { if (i === 1) continue a; r.push(i); }
  assertEq(r, [0, 2]);
  switch (1) { case 1: function sw() { return 'sw'; } }
  assertEq(sw(), 'sw');
});
test('deep nesting', function () {
  var src = '';
  for (var i = 0; i < 200; i++) src += '(';
  src += '1';
  for (var j = 0; j < 200; j++) src += ')';
  assertEq(eval(src), 1);
  var arr = '1';
  for (var k = 0; k < 100; k++) arr = '[' + arr + ']';
  var x = 5;
  assert(Array.isArray(eval(arr)));
  var tooDeep = '';
  for (var m = 0; m < 5000; m++) tooDeep += '[';
  assertThrows(SyntaxError, function () { eval(tooDeep); });
});
test('getter setter on classes static and instance with symbols', function () {
  var sym = Symbol('s');
  class K { static [sym]() { return 'static sym'; } [sym]() { return 'inst sym'; } }
  assertEq(K[sym](), 'static sym');
  assertEq(new K()[sym](), 'inst sym');
});
test('exotic numbers as keys and values', function () {
  var o = {};
  o[NaN] = 1;
  o[Infinity] = 2;
  assertEq(o.NaN + o.Infinity, 3);
  var a = [];
  a['2'] = 'two';
  assertEq(a.length, 3);
  a['-1'] = 'neg';
  assertEq(a.length, 3);
});
test('date and math in loops (perf sanity)', function () {
  var t = 0;
  for (var i = 0; i < 100000; i++) t += i & 7;
  assertEq(t, 350000);
  var o = {};
  for (var j = 0; j < 5000; j++) o['k' + j] = j;
  assertEq(Object.keys(o).length, 5000);
  var sum = 0;
  for (var k in o) sum += o[k];
  assertEq(sum, 12497500);
});
