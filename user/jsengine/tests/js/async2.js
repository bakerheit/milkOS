asyncTest('classic microtask ordering', async function () {
  var log = [];
  async function a1() { log.push('a1 start'); await a2(); log.push('a1 end'); }
  async function a2() { log.push('a2'); }
  log.push('script start');
  var p = a1();
  new Promise(function (r) { log.push('p1'); r(); }).then(function () { log.push('then1'); }).then(function () { log.push('then2'); });
  log.push('script end');
  await p;
  await null; await null; await null;
  assertEq(log, ['script start', 'a1 start', 'a2', 'p1', 'script end', 'a1 end', 'then1', 'then2']);
});
asyncTest('await of a native promise takes one tick, thenables two', async function () {
  var log = [];
  (async function () { await Promise.resolve(); log.push('native'); })();
  (async function () { await { then(r) { r(); } }; log.push('thenable'); })();
  Promise.resolve().then(() => log.push('t1')).then(() => log.push('t2')).then(() => log.push('t3'));
  for (var i = 0; i < 6; i++) await null;
  assertEq(log, ['native', 't1', 'thenable', 't2', 't3']);
});
asyncTest('resolve with a promise adopts it', async function () {
  var log = [];
  var inner = Promise.resolve('inner');
  new Promise(r => r(inner)).then(v => log.push(v));
  Promise.resolve().then(() => log.push('a')).then(() => log.push('b')).then(() => log.push('c'));
  for (var i = 0; i < 6; i++) await null;
  assertEq(log, ['a', 'b', 'inner', 'c']);
});
asyncTest('async return values and rejection flows', async function () {
  async function returnsPromise() { return Promise.resolve(7); }
  assertEq(await returnsPromise(), 7);
  async function throwsLater() { await null; throw new RangeError('later'); }
  var caught = await throwsLater().catch(e => e.name);
  assertEq(caught, 'RangeError');
  var results = [];
  await Promise.all([1, 2, 3].map(async function (n) { await new Promise(r => Promise.resolve().then(r)); results.push(n); }));
  assertEq(results, [1, 2, 3]);
  async function finallyOrder() { try { return 'try'; } finally { await null; results.push('finally'); } }
  assertEq(await finallyOrder(), 'try');
  assertEq(results[3], 'finally');
});
asyncTest('async generators queue requests', async function () {
  var log = [];
  async function* g() { log.push('start'); yield 1; log.push('after 1'); yield 2; log.push('end'); }
  var it = g();
  var p1 = it.next(), p2 = it.next(), p3 = it.next();
  var r = await Promise.all([p1, p2, p3]);
  assertEq(r.map(x => x.value), [1, 2, undefined]);
  assertEq(r.map(x => x.done), [false, false, true]);
  assertEq(log, ['start', 'after 1', 'end']);
  async function* thrower() { yield 1; throw new Error('boom'); }
  var t = thrower();
  await t.next();
  try { await t.next(); assert(false); } catch (e) { assertEq(e.message, 'boom'); }
  assertEq((await t.next()).done, true);
  async function* awaitsYield() { var x = yield Promise.resolve('resolved'); yield x; }
  var ay = awaitsYield();
  assertEq((await ay.next()).value, 'resolved');
  assertEq((await ay.next('sent')).value, 'sent');
});
asyncTest('promise statics with custom thenables', async function () {
  var thenable = { then(res) { res(42); } };
  assertEq(await Promise.resolve(thenable), 42);
  assertEq(await Promise.all([thenable, 1]), [42, 1]);
  var rejecting = { then(_, rej) { rej('no'); } };
  assertEq(await Promise.allSettled([rejecting]).then(r => r[0].reason), 'no');
  var count = 0;
  var p = new Promise(function (res, rej) { res(1); res(2); rej(3); count++; });
  assertEq(await p, 1);
  assertEq(count, 1);
});
asyncTest('then callbacks run with undefined this and handlers are optional', async function () {
  var thisVal = 'unset';
  await Promise.resolve().then(function () { 'use strict'; thisVal = this; });
  assertEq(thisVal, undefined);
  assertEq(await Promise.resolve(5).then(null, null), 5);
  assertEq(await Promise.reject(6).then(null).catch(x => x), 6);
  var p = Promise.resolve();
  assert(p.then() !== p);
});
