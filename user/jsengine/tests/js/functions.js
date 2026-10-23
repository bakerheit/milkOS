test('closures', function () {
  function counter() { var n = 0; return function () { return ++n; }; }
  var c = counter();
  c(); c();
  assertEq(c(), 3);
  var c2 = counter();
  assertEq(c2(), 1);
  var fs = [];
  for (let i = 0; i < 3; i++) fs.push(() => i);
  assertEq(fs.map(f => f()), [0, 1, 2]);
  var gs = [];
  for (var j = 0; j < 3; j++) gs.push(() => j);
  assertEq(gs.map(f => f()), [3, 3, 3]);
  var hs = [];
  for (const k of [7, 8]) hs.push(() => k);
  assertEq(hs[0]() + hs[1](), 15);
});
test('hoisting', function () {
  assertEq(h(), 'h');
  function h() { return 'h'; }
  assertEq(typeof v, 'undefined');
  var v = 1;
  assertThrows(ReferenceError, function () { t; let t = 1; });
  assertThrows(ReferenceError, function () { u = 2; let u; });
  assertThrows(TypeError, function () { const c = 1; c = 2; });
});
test('defaults rest spread', function () {
  function f(a, b = a + 1, ...rest) { return [a, b, rest.length]; }
  assertEq(f(1), [1, 2, 0]);
  assertEq(f(1, 5, 6, 7), [1, 5, 2]);
  assertEq(f.length, 1);
  function g() { return arguments.length; }
  assertEq(g(1, 2, 3), 3);
  assertEq(g(...[1, 2], 3, ...'ab'), 5);
  assertEq(Math.max(...[1, 9, 3]), 9);
  function args() { return Array.prototype.slice.call(arguments); }
  assertEq(args(1, 'x'), [1, 'x']);
});
test('arrows', function () {
  var o = {
    v: 42,
    m() { return [1].map(() => this.v)[0]; },
    n: function () { var f = () => () => this.v; return f()(); },
  };
  assertEq(o.m(), 42);
  assertEq(o.n(), 42);
  var sq = x => x * x;
  assertEq(sq(4), 16);
  var obj = () => ({ a: 1 });
  assertEq(obj().a, 1);
  assertEq((() => {})(), undefined);
  function outer() { return (() => arguments[0])(); }
  assertEq(outer('arg'), 'arg');
  assertThrows(TypeError, function () { new sq(); });
});
test('this binding', function () {
  function who() { return this; }
  assertEq(who.call(5) instanceof Number, true);
  function strict() { 'use strict'; return this; }
  assertEq(strict(), undefined);
  assertEq(strict.call(5), 5);
  assertEq(who(), globalThis);
  var o = { f: who };
  assertEq(o.f(), o);
  assertEq((o.f)(), o);
  assertEq((0, o.f)(), globalThis);
  var b = who.bind('x');
  assertEq(String(b()), 'x');
  assertEq(b.name, 'bound who');
});
test('call apply bind', function () {
  function add(a, b) { return this.base + a + b; }
  var o = { base: 10 };
  assertEq(add.call(o, 1, 2), 13);
  assertEq(add.apply(o, [1, 2]), 13);
  var b = add.bind(o, 1);
  assertEq(b(2), 13);
  assertEq(b.length, 1);
  function P(x) { this.x = x; }
  var BP = P.bind(null, 3);
  var p = new BP();
  assertEq(p.x, 3);
  assert(p instanceof P);
});
test('names and length', function () {
  function foo(a, b) {}
  assertEq(foo.name, 'foo');
  assertEq(foo.length, 2);
  var bar = function () {};
  assertEq(bar.name, 'bar');
  var baz = () => {};
  assertEq(baz.name, 'baz');
  var o = { m() {}, ['c' + 'd']: function () {}, get g() { return 1; } };
  assertEq(o.m.name, 'm');
  assertEq(o.cd.name, 'cd');
  assertEq(Object.getOwnPropertyDescriptor(o, 'g').get.name, 'get g');
  var named = function inner() { return typeof inner; };
  assertEq(named.name, 'inner');
  assertEq(named(), 'function');
});
test('recursion', function () {
  function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
  assertEq(fib(20), 6765);
  var fact = function f(n) { return n <= 1 ? 1 : n * f(n - 1); };
  assertEq(fact(10), 3628800);
  function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1); }
  assertEq(deep(500), 500);
  assertThrows(RangeError, function () { function inf() { return inf(); } inf(); });
});
test('Function constructor', function () {
  var f = new Function('a', 'b', 'return a * b');
  assertEq(f(3, 4), 12);
  assertEq(Function('return 7')(), 7);
  assertThrows(SyntaxError, function () { Function('}{'); });
  assertThrows(SyntaxError, function () { Function('a', '}), (function(){'); });
  assert(f.toString().indexOf('anonymous') >= 0);
});
test('toString', function () {
  function sample(x) { return x + 1; }
  assertEq(sample.toString(), 'function sample(x) { return x + 1; }');
  assert(Math.max.toString().indexOf('[native code]') > 0);
});
test('getters setters', function () {
  var o = {
    _v: 1,
    get v() { return this._v * 10; },
    set v(x) { this._v = x; },
  };
  assertEq(o.v, 10);
  o.v = 5;
  assertEq(o.v, 50);
  var d = Object.getOwnPropertyDescriptor(o, 'v');
  assert(typeof d.get === 'function' && typeof d.set === 'function');
  assert(d.enumerable && d.configurable);
});
test('iife and scopes', function () {
  var r = (function (x) { return x * 2; })(21);
  assertEq(r, 42);
  { let blockOnly = 1; }
  assertEq(typeof blockOnly, 'undefined');
  var x = 1;
  { let x = 2; assertEq(x, 2); }
  assertEq(x, 1);
  if (true) { function inIf() { return 'ok'; } }
  assertEq(inIf(), 'ok');
});
