// Builtins that call back into script while they hold values: each callback allocates, so
// under the GC stress run a missing root shows up here.
function junk() { var a = []; for (var i = 0; i < 20; i++) a.push({ i: i, s: 'x' + i }); return a; }
test('array callbacks', function () {
  var src = [];
  for (var i = 0; i < 50; i++) src.push({ v: i });
  var m = src.map(function (o) { junk(); return { w: o.v * 2 }; });
  assertEq(m[49].w, 98);
  var f = src.filter(function (o) { junk(); return o.v % 2; });
  assertEq(f.length, 25);
  assertEq(f[0].v, 1);
  var r = src.reduce(function (acc, o) { junk(); return acc.concat([o.v]); }, []);
  assertEq(r.length, 50);
  var s = src.slice().sort(function (a, b) { junk(); return b.v - a.v; });
  assertEq(s[0].v, 49);
  var fm = src.flatMap(function (o) { junk(); return [o, { copy: o.v }]; });
  assertEq(fm[99].copy, 49);
  var fr = Array.from({ length: 30 }, function (_, i) { junk(); return { i: i }; });
  assertEq(fr[29].i, 29);
  var found = src.find(function (o) { junk(); return o.v === 40; });
  assertEq(found.v, 40);
});
test('strings and regexps', function () {
  var str = 'a1b2c3d4e5f6g7h8i9j0'.repeat(5);
  var out = str.replace(/\d/g, function (d) { junk(); return '<' + d + '>'; });
  assertEq(out.length, 200);
  var parts = str.split(/(\d)/);
  assertEq(parts.length, 101);
  var ms = [...str.matchAll(/[a-z]\d/g)].map(function (m) { junk(); return m[0]; });
  assertEq(ms.length, 50);
  var s2 = 'x'.padStart(1000, 'ab').split('').map(function (c) { return c + c; }).join('');
  assertEq(s2.length, 2000);
});
test('json', function () {
  var data = [];
  for (var i = 0; i < 30; i++) data.push({ id: i, tags: ['a', 'b'], nested: { when: new Date(i * 1000) } });
  var text = JSON.stringify(data, function (k, v) { junk(); return v; }, 1);
  var back = JSON.parse(text, function (k, v) { junk(); return v; });
  assertEq(back[29].nested.when, '1970-01-01T00:00:29.000Z');
  var withToJSON = { a: { toJSON: function () { junk(); return { b: junk().length }; } } };
  assertEq(JSON.stringify(withToJSON), '{"a":{"b":20}}');
});
test('objects and getters', function () {
  var src = {};
  for (var i = 0; i < 20; i++) Object.defineProperty(src, 'k' + i, { enumerable: true, get: function () { junk(); return { made: true }; } });
  var copy = Object.assign({}, src);
  assertEq(copy.k19.made, true);
  var spread = { ...src };
  assertEq(spread.k0.made, true);
  var entries = Object.entries(src);
  assertEq(entries[19][1].made, true);
  var vals = Object.values(src);
  assertEq(vals.length, 20);
  var fe = Object.fromEntries(entries.map(function (e) { junk(); return [e[0], e[1]]; }));
  assertEq(fe.k5.made, true);
});
test('iterables into builtins', function () {
  function* g() { for (var i = 0; i < 40; i++) { junk(); yield ['k' + i, { v: i }]; } }
  var m = new Map(g());
  assertEq(m.get('k39').v, 39);
  var s = new Set((function* () { for (var i = 0; i < 40; i++) { junk(); yield { i: i }; } })());
  assertEq(s.size, 40);
  var arr = [...g()];
  assertEq(arr[39][1].v, 39);
  var [a, b, ...rest] = g();
  assertEq(rest.length, 38);
  var fromIter = Array.from(g(), function (e) { junk(); return e[1]; });
  assertEq(fromIter[0].v, 0);
});
asyncTest('promises', async function () {
  var results = await Promise.all([1, 2, 3, 4, 5].map(async function (n) { junk(); await null; junk(); return { n: n }; }));
  assertEq(results[4].n, 5);
  var settled = await Promise.allSettled([Promise.reject({ e: 1 }), (async function () { junk(); return { ok: 1 }; })()]);
  assertEq(settled[0].reason.e, 1);
  assertEq(settled[1].value.ok, 1);
  var chain = Promise.resolve({ c: 0 });
  for (var i = 0; i < 30; i++) chain = chain.then(function (o) { junk(); return { c: o.c + 1 }; });
  assertEq((await chain).c, 30);
  async function* ag() { for (var i = 0; i < 10; i++) { junk(); yield { i: i }; await null; } }
  var n = 0;
  for await (var x of ag()) { junk(); n += x.i; }
  assertEq(n, 45);
});
test('closures and classes under pressure', function () {
  var fns = [];
  for (let i = 0; i < 100; i++) { junk(); fns.push(function () { return { i: i }; }); }
  assertEq(fns[99]().i, 99);
  class Node { constructor(v, next) { this.v = v; this.next = next; junk(); } }
  var list = null;
  for (var j = 0; j < 100; j++) list = new Node(j, list);
  var count = 0;
  for (var p = list; p; p = p.next) count += 1;
  assertEq(count, 100);
  var bound = function (a, b) { junk(); return [this.t, a, b]; }.bind({ t: 'T' }, 'A');
  assertEq(bound('B'), ['T', 'A', 'B']);
  var gen = (function* () { var keep = { k: 'kept' }; for (var i = 0; i < 50; i++) { junk(); yield keep; } })();
  var last;
  for (var v of gen) last = v;
  assertEq(last.k, 'kept');
});
