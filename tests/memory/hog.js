// Allocates and holds ordinary JavaScript data, a slice at a time (so timers and messages keep
// being served), and says how far it got. Used by pages, frames and workers alike.
//   hogRun(mb, kind, done)   kind: objects | strings | typed | mixed
// One "MB" is a count of items that weighs about a megabyte in the engine's own accounting
// (measured: see docs/browser.md "Memory"); the figures are approximate by design.
var HOLD = [];
var PER_MB = { objects: 700, strings: 400, typed: 2, mixed: 450 };
function hogItem(kind, i) {
  if (kind === 'strings') return 'item-' + i + '-' + 'abcdefghij'.repeat(100) + i;
  if (kind === 'typed') { var a = new Uint8Array(1 << 19); a[i & 1023] = 1; return a; }
  return { id: i, name: 'item-' + i, tags: [i, i + 1, i + 2], nested: { a: i, b: 'v' + i } };
}
function hogRun(mb, kind, done, progress) {
  var per = PER_MB[kind] || PER_MB.objects;
  var total = mb * per, made = 0, t0 = Date.now();
  var slice = Math.max(1, per * 2);
  function step() {
    var end = Math.min(total, made + slice);
    for (; made < end; made++) {
      if (kind === 'mixed') {
        HOLD.push(hogItem(made & 1 ? 'strings' : 'objects', made));
        if (made % 900 === 0) HOLD.push(hogItem('typed', made));
      } else {
        HOLD.push(hogItem(kind, made));
      }
    }
    if (progress) progress(Math.floor(made / per));
    if (made < total) setTimeout(step, 0); else done(Date.now() - t0);
  }
  step();
}
