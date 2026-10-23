// Promises, async functions and generators: a pipeline of 2000 async steps and a
// generator-driven iteration.
(function () {
  var out = { done: 0, sum: 0 };
  async function step(i) { await null; return i * 2; }
  async function pipeline() {
    var total = 0;
    for (var i = 0; i < 2000; i++) total += await step(i);
    var all = await Promise.all(Array.from({ length: 500 }, (_, i) => step(i)));
    total += all.reduce((a, b) => a + b, 0);
    out.sum = total;
    out.done = 1;
  }
  pipeline();
  function* fib() { var a = 0, b = 1; for (;;) { yield a; [a, b] = [b, a + b]; } }
  var g = 0, n = 0;
  for (var x of fib()) { if (n++ > 70) break; g = x; }
  out.gen = g;
  return out;
})();
