asyncTest('promise ordering', async function () {
  var log = [];
  var p = Promise.resolve();
  p.then(() => log.push('then1')).then(() => log.push('then2'));
  queue(() => log.push('micro'));
  log.push('sync');
  await null;
  await null;
  await null;
  assertEq(log, ['sync', 'then1', 'micro', 'then2']);
  function queue(f) { Promise.resolve().then(f); }
});
asyncTest('async await basics', async function () {
  async function add(a, b) { return a + b; }
  assertEq(await add(1, 2), 3);
  async function fails() { throw new TypeError('nope'); }
  try { await fails(); assert(false); } catch (e) { assert(e instanceof TypeError); }
  var p = fails();
  assert(p instanceof Promise);
  await p.catch(() => {});
  var order = [];
  async function a() { order.push('a1'); await null; order.push('a2'); }
  var pa = a();
  order.push('after call');
  await pa;
  assertEq(order, ['a1', 'after call', 'a2']);
  var arrow = async x => x * 2;
  assertEq(await arrow(4), 8);
  var obj = { async m() { return this; } };
  assertEq(await obj.m(), obj);
});
asyncTest('promise combinators', async function () {
  var all = await Promise.all([1, Promise.resolve(2), new Promise(r => r(3))]);
  assertEq(all, [1, 2, 3]);
  var settled = await Promise.allSettled([Promise.resolve(1), Promise.reject('x')]);
  assertEq(settled.map(s => s.status), ['fulfilled', 'rejected']);
  assertEq(settled[1].reason, 'x');
  assertEq(await Promise.race([new Promise(() => {}), Promise.resolve('fast')]), 'fast');
  assertEq(await Promise.any([Promise.reject(1), Promise.resolve(2)]), 2);
  try { await Promise.any([Promise.reject(1)]); assert(false); } catch (e) { assert(e instanceof AggregateError); assertEq(e.errors, [1]); }
  try { await Promise.all([Promise.resolve(1), Promise.reject('bad')]); assert(false); } catch (e) { assertEq(e, 'bad'); }
  assertEq(await Promise.all([]), []);
});
asyncTest('then chains and thenables', async function () {
  var v = await new Promise(r => r({ then(res) { res('thenable'); } }));
  assertEq(v, 'thenable');
  var chained = await Promise.resolve(1).then(x => x + 1).then(x => { throw x * 10; }).catch(e => e + 1);
  assertEq(chained, 21);
  var fin = [];
  await Promise.resolve('v').finally(() => fin.push('f')).then(x => fin.push(x));
  assertEq(fin, ['f', 'v']);
  var p = Promise.resolve(5);
  assertEq(Promise.resolve(p), p);
  var self = new Promise(r => setTimeoutLike(() => r(self)));
  function setTimeoutLike(f) { Promise.resolve().then(f); }
  try { await self; assert(false); } catch (e) { assert(e instanceof TypeError); }
  var executorThrows = new Promise(() => { throw 'ex'; });
  try { await executorThrows; } catch (e) { assertEq(e, 'ex'); }
});
asyncTest('async iteration', async function () {
  async function* ag() { yield 1; await null; yield 2; }
  var out = [];
  for await (var v of ag()) out.push(v);
  assertEq(out, [1, 2]);
  var out2 = [];
  for await (var w of [Promise.resolve('a'), 'b']) out2.push(w);
  assertEq(out2, ['a', 'b']);
  var it = ag();
  var r1 = await it.next();
  assertEq([r1.value, r1.done], [1, false]);
  var r2 = await it.return('early');
  assertEq([r2.value, r2.done], ['early', true]);
});
asyncTest('await in loops and try', async function () {
  var total = 0;
  for (var i = 0; i < 5; i++) total += await Promise.resolve(i);
  assertEq(total, 10);
  var log = [];
  try { await Promise.reject(new Error('r')); } catch (e) { log.push(e.message); } finally { log.push('fin'); }
  assertEq(log, ['r', 'fin']);
  var results = await Promise.all([1, 2, 3].map(async x => { await null; return x * x; }));
  assertEq(results, [1, 4, 9]);
});
test('unhandled sync check', function () {
  var p = Promise.reject(new Error('handled later'));
  p.catch(function () {});
});
