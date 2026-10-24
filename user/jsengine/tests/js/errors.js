test('error objects', function () {
  var e = new Error('msg');
  assertEq(e.message, 'msg');
  assertEq(e.name, 'Error');
  assertEq(String(e), 'Error: msg');
  assert(e instanceof Error);
  assert(typeof e.stack === 'string' && e.stack.indexOf('Error: msg') === 0);
  var t = new TypeError('bad');
  assert(t instanceof TypeError && t instanceof Error);
  assertEq(t.name, 'TypeError');
  assertEq(Object.prototype.toString.call(t), '[object Error]');
  assertEq(new Error().message, '');
  assertEq(Error('x').message, 'x');
  var c = new Error('outer', { cause: 'why' });
  assertEq(c.cause, 'why');
  assert(!Object.keys(e).includes('message'));
  assertEq(new RangeError('r').toString(), 'RangeError: r');
  var custom = { name: 'Custom', message: 'm' };
  assertEq(Error.prototype.toString.call(custom), 'Custom: m');
});
test('builtin error messages', function () {
  function msg(f) { try { f(); } catch (e) { return e.constructor.name + ': ' + e.message; } return 'none'; }
  assertEq(msg(function () { var u; u.x; }), "TypeError: Cannot read properties of undefined (reading 'x')");
  assertEq(msg(function () { var n = null; n.y = 1; }), "TypeError: Cannot set properties of null (setting 'y')");
  assertEq(msg(function () { undefinedVariable; }), 'ReferenceError: undefinedVariable is not defined');
  assertEq(msg(function () { var o = {}; o.method(); }), 'TypeError: o.method is not a function');
  assertEq(msg(function () { var notf = 1; notf(); }), 'TypeError: notf is not a function');
  assertEq(msg(function () { new (function () {}.bind())(); }), 'none');
  assertEq(msg(function () { new Math.max(); }), 'TypeError: Math.max is not a constructor');
  assertEq(msg(function () { null[Symbol.iterator]; }), "TypeError: Cannot read properties of null (reading 'Symbol(Symbol.iterator)')");
});
test('stack traces', function () {
  function inner() { return new Error('deep'); }
  function outer() { return inner(); }
  var s = outer().stack;
  assert(s.indexOf('inner') > 0, 'inner in stack: ' + s);
  assert(s.indexOf('outer') > s.indexOf('inner'), 'outer below inner');
  assert(/errors\.js:\d+:\d+/.test(s), 'file:line:col');
  var obj = {};
  Error.captureStackTrace(obj);
  assertEq(typeof obj.stack, 'string');
});
test('not a function names the callee as written', function () {
  function msg(f) { try { f(); } catch (e) { return e.message; } return 'none'; }
  var V = function () { return {}; };
  assertEq(msg(function () { V(1).concat(V(2)); }), 'V(...).concat is not a function');
  var n = 5;
  assertEq(msg(function () { n(1).concat(2); }), 'n is not a function');
  assertEq(msg(function () { V()(); }), 'V(...) is not a function');
  var o = { a: { b: {} }, list: [1] };
  assertEq(msg(function () { o.a.b.c(); }), 'o.a.b.c is not a function');
  assertEq(msg(function () { o.list[0](); }), 'o.list[0] is not a function');
  assertEq(msg(function () { o.a
    .b
    .c(1,
      2); }), 'o.a.b.c is not a function');
  assertEq(msg(function () { o.a.b.c.d(); }), "Cannot read properties of undefined (reading 'd')");
});
test('stack frames name functions by where they are stored', function () {
  var ns = { util: {} };
  ns.util.go = function () { return new Error('x').stack; };
  var viaVar = function () { return ns.util.go(); };
  var s = viaVar();
  assert(s.indexOf('at ns.util.go (') > 0, s);
  assert(s.indexOf('at viaVar (') > 0, s);
  assertEq(ns.util.go.name, '');
});
test('syntax errors', function () {
  assertThrows(SyntaxError, function () { eval('var 1x;'); });
  assertThrows(SyntaxError, function () { eval('let a; let a;'); });
  assertThrows(SyntaxError, function () { eval('"use strict"; with ({}) {}'); });
  assertThrows(SyntaxError, function () { eval('break;'); });
  assertThrows(SyntaxError, function () { eval('return 1'); });
  assertThrows(SyntaxError, function () { eval('a => { yield 1 }; function* g() { yield = 1 }'); });
  assertThrows(SyntaxError, function () { eval('({a = 1})'); });
  assertThrows(SyntaxError, function () { eval('1 = 2'); });
  assertThrows(SyntaxError, function () { eval('"use strict"; delete x;'); });
  assertThrows(SyntaxError, function () { eval('"use strict"; var eval = 1;'); });
  assertThrows(SyntaxError, function () { eval('class A { constructor(){} constructor(){} }'); });
  assertThrows(SyntaxError, function () { eval('a ?? b || c'); });
  assertThrows(SyntaxError, function () { eval('-2 ** 2'); });
  assertThrows(SyntaxError, function () { eval('/(/'); });
  assertThrows(SyntaxError, function () { eval('import x from "y"'); });
  var e = assertThrows(SyntaxError, function () { eval('\n\n  foo bar'); });
  assert(e.stack.indexOf('eval:3:') > 0, 'position in stack: ' + e.stack);
});
test('strict mode', function () {
  'use strict';
  assertThrows(ReferenceError, function () { undeclared = 1; });
  assertThrows(TypeError, function () { Object.freeze([1])[0] = 2; });
  assertThrows(TypeError, function () { var s = 'abc'; s.length = 1; });
  assertThrows(TypeError, function () { delete Object.prototype; });
  assertEq((function () { return this; })(), undefined);
  assertThrows(TypeError, function () { (function f() { f = 1; })(); });
});
