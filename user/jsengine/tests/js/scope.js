test('closures over loop heads', function () {
  var a = [];
  for (let k in { x: 1, y: 2 }) a.push(() => k);
  assertEq(a.map(f => f()), ['x', 'y']);
  var b = [];
  for (let [i, v] of [[0, 'p'], [1, 'q']]) b.push(() => i + v);
  assertEq(b.map(f => f()), ['0p', '1q']);
  var c = [];
  for (let i = 0, j = 10; i < 3; i++, j--) c.push(() => i * j);
  assertEq(c.map(f => f()), [0, 9, 16]);
  var d = [];
  let n = 0;
  while (n < 3) { let m = n; d.push(() => m); n++; }
  assertEq(d.map(f => f()), [0, 1, 2]);
});
test('tdz positions', function () {
  assertThrows(ReferenceError, function () { typeof x; let x; });
  assertThrows(ReferenceError, function () { function inner() { return y; } inner(); let y = 1; });
  function later() { return z; }
  let z = 5;
  assertEq(later(), 5);
  assertThrows(ReferenceError, function () { let w = w; });
  assertThrows(ReferenceError, function () { class C extends C {} });
  assertThrows(ReferenceError, function () { new K(); class K {} });
  assertThrows(ReferenceError, function () { const f = () => q; f(); const q = 1; });
  assertThrows(ReferenceError, function () { switch (1) { case 0: let s = 1; case 1: s = 2; } });
});
test('parameter scope', function () {
  function f(a, b = a * 2, c = () => a + b) { a = 10; return c(); }
  assertEq(f(1), 12);
  function g(x, y = x) { var x; return y; }
  assertEq(g(3), 3);
  function h(p) { var p; return p; }
  assertEq(h(4), 4);
  function k(p) { function p() {} return typeof p; }
  assertEq(k(1), 'function');
  function dup(a, a) { return a; }
  assertEq(dup(1, 2), 2);
});
test('function name bindings', function () {
  var f = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); };
  var g = f;
  f = null;
  assertEq(g(5), 120);
  var h = function self() { self = 1; return typeof self; };
  assertEq(h(), 'function');
  assertEq(typeof self, 'undefined');
});
test('var hoisting through blocks', function () {
  function f() {
    if (false) { var hoisted = 1; }
    return hoisted;
  }
  assertEq(f(), undefined);
  function g() {
    for (var i = 0; i < 2; i++) { var inLoop = i; }
    return inLoop + i;
  }
  assertEq(g(), 3);
  function t() { try { throw 1; } catch (e) { var e2 = e; } return e2; }
  assertEq(t(), 1);
  function cat() { try { throw 'v'; } catch (e) { var e = 'shadow'; } return typeof e; }
  assertEq(cat(), 'undefined');
});
test('block functions annex b', function () {
  function f() {
    var before = typeof g;
    { function g() { return 1; } }
    return before + ',' + typeof g;
  }
  assertEq(f(), 'undefined,function');
  function k() { 'use strict'; { function inner() {} } return typeof inner; }
  assertEq(k(), 'undefined');
});
test('closures capture variables not values', function () {
  var x = 1;
  var get = () => x;
  x = 2;
  assertEq(get(), 2);
  function counter() { let c = 0; return { inc: () => ++c, get: () => c }; }
  var ctr = counter();
  ctr.inc(); ctr.inc();
  assertEq(ctr.get(), 2);
  var fns = {};
  ['a', 'b'].forEach(function (name) { fns[name] = function () { return name.toUpperCase(); }; });
  assertEq(fns.a() + fns.b(), 'AB');
});
test('this in nested functions', function () {
  var o = {
    v: 1,
    m: function () {
      function inner() { return this; }
      var arrow = () => this;
      return [inner() === globalThis, arrow() === o];
    },
  };
  assertEq(o.m(), [true, true]);
  class C { m() { return (() => (() => this)())(); } }
  var c = new C();
  assertEq(c.m(), c);
});
test('indirect eval is global', function () {
  var local = 'local';
  globalThis.globalForEval = 'g';
  assertEq((0, eval)('typeof local'), 'undefined');
  assertEq((0, eval)('globalForEval'), 'g');
  assertEq((0, eval)('1 + 1'), 2);
  assertEq(eval(5), 5);
});
