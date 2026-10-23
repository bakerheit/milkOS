test('arrays', function () {
  var [a, b, c = 3, ...rest] = [1, 2, undefined, 4, 5];
  assertEq([a, b, c, rest], [1, 2, 3, [4, 5]]);
  var [, second] = 'xy';
  assertEq(second, 'y');
  var [p, [q, r]] = [1, [2, 3]];
  assertEq(p + q + r, 6);
  var x = 1, y = 2;
  [x, y] = [y, x];
  assertEq([x, y], [2, 1]);
  var [m = 'd'] = [];
  assertEq(m, 'd');
  function* g() { yield 1; yield 2; yield 3; }
  var [g1, g2] = g();
  assertEq(g1 + g2, 3);
  var [s1, s2] = new Set([7, 8]);
  assertEq(s1 + s2, 15);
});
test('objects', function () {
  var { a, b: { c }, d = 4, e: f = 5 } = { a: 1, b: { c: 2 } };
  assertEq([a, c, d, f], [1, 2, 4, 5]);
  var key = 'k';
  var { [key]: v } = { k: 'val' };
  assertEq(v, 'val');
  var { length } = 'hello';
  assertEq(length, 5);
  var o = {};
  ({ x: o.x, y: o['y'] } = { x: 1, y: 2 });
  assertEq(o.x + o.y, 3);
  assertThrows(TypeError, function () { var { z } = null; });
  assertThrows(TypeError, function () { var { z } = undefined; });
  var { ...all } = { p: 1, q: 2 };
  assertEq(Object.keys(all), ['p', 'q']);
});
test('params', function () {
  function f({ a, b = 2 } = {}, [c] = [3]) { return a + b + c; }
  assertEq(f({ a: 1 }), 6);
  assertEq(isNaN(f()), true);
  var g = ({ x }) => x * 2;
  assertEq(g({ x: 21 }), 42);
  function h(a, { b } = { b: a }) { return b; }
  assertEq(h(9), 9);
});
test('loops', function () {
  var out = [];
  for (var [k, v] of new Map([['a', 1], ['b', 2]])) out.push(k + v);
  assertEq(out, ['a1', 'b2']);
  var sum = 0;
  for (const { n } of [{ n: 1 }, { n: 2 }]) sum += n;
  assertEq(sum, 3);
  var entries = [];
  for (let [i, ch] of [...'ab'].entries()) entries.push(i + ch);
  assertEq(entries, ['0a', '1b']);
});
test('defaults evaluation order', function () {
  var log = [];
  function d(x) { log.push(x); return x; }
  var [a = d('a'), b = d('b')] = [undefined, 0];
  assertEq(log, ['a']);
  assertEq(b, 0);
  var { c = d('c') } = { c: null };
  assertEq(c, null);
  assertEq(log, ['a']);
});
