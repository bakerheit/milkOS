test('loops', function () {
  var s = 0;
  for (var i = 0; i < 10; i++) s += i;
  assertEq(s, 45);
  var w = 0;
  while (w < 5) w++;
  assertEq(w, 5);
  var d = 0;
  do { d++; } while (d < 3);
  assertEq(d, 3);
  var n = 0;
  for (;;) { if (++n > 4) break; }
  assertEq(n, 5);
  var c = 0;
  for (var j = 0; j < 10; j++) { if (j % 2) continue; c++; }
  assertEq(c, 5);
  do continue; while (false);
});
test('switch', function () {
  function f(x) {
    switch (x) {
      case 1: return 'one';
      case 2:
      case 3: return 'two-three';
      default: return 'other';
      case 4: return 'four';
    }
  }
  assertEq([f(1), f(3), f(4), f(9)], ['one', 'two-three', 'four', 'other']);
  var log = [];
  switch (2) { case 1: log.push(1); case 2: log.push(2); case 3: log.push(3); break; case 4: log.push(4); }
  assertEq(log, [2, 3]);
  switch ('1') { case 1: log.push('loose'); break; default: log.push('strict'); }
  assertEq(log[2], 'strict');
  switch (1) { case 1: let scoped = 'x'; log.push(scoped); }
  assertEq(log[3], 'x');
});
test('try catch finally', function () {
  var log = [];
  try { log.push('t'); throw 1; } catch (e) { log.push('c' + e); } finally { log.push('f'); }
  assertEq(log, ['t', 'c1', 'f']);
  function r() { try { return 'try'; } finally { log.push('fin'); } }
  assertEq(r(), 'try');
  assertEq(log[3], 'fin');
  function override() { try { return 1; } finally { return 2; } }
  assertEq(override(), 2);
  function nested() {
    try {
      try { throw new Error('inner'); } finally { log.push('inner-finally'); }
    } catch (e) { return e.message; }
  }
  assertEq(nested(), 'inner');
  var x = 0;
  for (var i = 0; i < 3; i++) { try { if (i === 1) continue; x++; } finally { x += 10; } }
  assertEq(x, 32);
  outer: for (var a = 0; a < 2; a++) { try { break outer; } finally { log.push('broke'); } }
  assertEq(log[log.length - 1], 'broke');
  try { throw 'nobinding'; } catch { log.push('optional catch'); }
  assertEq(log[log.length - 1], 'optional catch');
  function throwsInFinally() { try { throw 1; } finally { throw 2; } }
  try { throwsInFinally(); } catch (e) { assertEq(e, 2); }
  var caught;
  try { try { throw 'a'; } catch (e) { throw e + 'b'; } } catch (e2) { caught = e2; }
  assertEq(caught, 'ab');
});
test('exception values', function () {
  var types = [];
  [1, 'str', null, undefined, { o: 1 }, [1]].forEach(function (v) {
    try { throw v; } catch (e) { types.push(e === v); }
  });
  assertEq(types, [true, true, true, true, true, true]);
});
test('block scoping in loops', function () {
  var fns = [];
  for (let i = 0; i < 3; i++) { let j = i * 2; fns.push(function () { return i + j; }); }
  assertEq(fns.map(f => f()), [0, 3, 6]);
  let k = 'outer';
  for (let k = 0; k < 1; k++) {}
  assertEq(k, 'outer');
  var fs2 = [];
  for (let i = 0; i < 3; i++) { fs2.push(() => i); i++; }
  assertEq(fs2.map(f => f()), [1, 3]);
});
test('for in variants', function () {
  var o = { a: 1, b: 2 };
  var ks = [];
  for (const k in o) ks.push(k);
  assertEq(ks, ['a', 'b']);
  var target = {};
  for (target.prop in o);
  assertEq(target.prop, 'b');
  for (var x in null) throw 1;
  for (var y in undefined) throw 1;
  var str = [];
  for (var i in 'ab') str.push(i);
  assertEq(str, ['0', '1']);
});
test('comma and void in loops', function () {
  for (var i = 0, j = 10; i < j; i++, j--);
  assertEq(i, 5);
});
