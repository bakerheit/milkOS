// Arrays and closures: sort, map/filter/reduce chains, Map/Set, destructuring, spread.
(function () {
  var seed = 42;
  function rand() { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed; }
  var a = [];
  for (var i = 0; i < 50000; i++) a.push(rand() % 100000);
  var sorted = a.slice().sort(function (x, y) { return x - y; });
  var sum = a.map(x => x * 2).filter(x => x % 3 === 0).reduce((s, x) => s + x, 0);
  var m = new Map();
  for (var j = 0; j < a.length; j++) m.set(a[j] % 1000, (m.get(a[j] % 1000) || 0) + 1);
  var uniq = new Set(a).size;
  var pairs = 0;
  for (var [k, v] of m) pairs += k * v;
  var objs = a.slice(0, 5000).map((x, i) => ({ x, i, s: 'k' + (x % 50) }));
  objs.sort((p, q) => p.s < q.s ? -1 : p.s > q.s ? 1 : p.i - q.i);
  var spread = Math.max(...a.slice(0, 1000));
  return sorted[25000] + ',' + sum + ',' + uniq + ',' + pairs + ',' + objs[0].s + ',' + spread;
})();
