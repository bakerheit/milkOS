test('generators', function () {
  function* g() { var x = yield 1; yield x * 2; return 'done'; }
  var it = g();
  assertEq(it.next().value, 1);
  assertEq(it.next(5).value, 10);
  var last = it.next();
  assertEq([last.value, last.done], ['done', true]);
  assertEq(it.next().done, true);
  function* inf() { var i = 0; while (true) yield i++; }
  var taken = [];
  for (var v of inf()) { if (v > 3) break; taken.push(v); }
  assertEq(taken, [0, 1, 2, 3]);
  function* deleg() { yield 'a'; yield* ['b', 'c']; var r = yield* g2(); yield r; }
  function* g2() { yield 'd'; return 'e'; }
  assertEq([...deleg()], ['a', 'b', 'c', 'd', 'e']);
  var gen = g();
  assert(gen[Symbol.iterator]() === gen);
  assertEq(Object.prototype.toString.call(gen), '[object Generator]');
});
test('generator return and throw', function () {
  var log = [];
  function* g() { try { yield 1; yield 2; } finally { log.push('cleanup'); } }
  var it = g();
  it.next();
  var r = it.return(42);
  assertEq([r.value, r.done], [42, true]);
  assertEq(log, ['cleanup']);
  function* h() { try { yield 1; } catch (e) { yield 'caught ' + e; } }
  var t = h();
  t.next();
  assertEq(t.throw('x').value, 'caught x');
  var fresh = h();
  assertThrows(null, function () { fresh.throw(new Error('early')); });
  for (var v of g()) break;
  assertEq(log, ['cleanup', 'cleanup']);
  function* fin() { try { yield 1; } finally { yield 'from finally'; } }
  var f = fin();
  f.next();
  assertEq(f.return(9).value, 'from finally');
  assertEq(f.next().value, 9);
  function* outer() { try { yield* inner(); } finally { log.push('outer'); } }
  function* inner() { try { yield 1; } finally { log.push('inner'); } }
  var o = outer(); o.next(); o.return();
  assertEq(log.slice(-2), ['inner', 'outer']);
});
test('iterator protocol', function () {
  var obj = {
    from: 1, to: 3,
    [Symbol.iterator]() {
      var cur = this.from, to = this.to;
      return { next: () => cur <= to ? { value: cur++, done: false } : { value: undefined, done: true }, return() { obj.closed = true; return {}; } };
    },
  };
  assertEq([...obj], [1, 2, 3]);
  assertEq(Array.from(obj), [1, 2, 3]);
  for (var x of obj) { if (x === 2) break; }
  assert(obj.closed, 'return() called on break');
  var [first] = obj;
  assertEq(first, 1);
  assertThrows(TypeError, function () { for (var y of {}) {} });
  assertThrows(TypeError, function () { [...123]; });
});
test('builtin iterators', function () {
  assertEq([...'a😀b'], ['a', '😀', 'b']);
  assertEq([...[1, 2].keys()], [0, 1]);
  assertEq([...[1, 2].entries()].map(e => e.join(':')), ['0:1', '1:2']);
  var m = new Map([[1, 'one'], [2, 'two']]);
  assertEq([...m.keys()], [1, 2]);
  assertEq([...m.values()], ['one', 'two']);
  var s = new Set('hello');
  assertEq([...s].join(''), 'helo');
  var ai = [1, 2][Symbol.iterator]();
  assertEq(Object.prototype.toString.call(ai), '[object Array Iterator]');
  assertEq(ai.next().value, 1);
  var arr = [1, 2, 3];
  var seen = [];
  for (var v of arr) { seen.push(v); if (v === 1) arr.push(4); }
  assertEq(seen, [1, 2, 3, 4]);
});
test('spread arguments and arrays', function () {
  function f(...a) { return a.length; }
  assertEq(f(...[], ...[1], ...new Set([2, 3])), 3);
  var a = [1, ...[2, 3], 4];
  assertEq(a, [1, 2, 3, 4]);
  var holes = [, 1];
  assertEq([...holes], [undefined, 1]);
  assertEq(Math.max(...'123'), 3);
});
