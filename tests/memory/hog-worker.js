// A worker that allocates what it is told and keeps it.
importScripts('/report.js', '/hog.js');
var a = args(self.location.search);
var mb = +a.mb || 50, kind = a.kind || 'objects';
report('worker', 'begin', mb + ' ' + kind);
hogRun(mb, kind, function (ms) {
  report('worker', 'held', mb + 'MB in ' + ms + 'ms');
  postMessage('held');
}, function (n) { if (n % 25 === 0) postMessage('progress ' + n); }, !!a.sync);
onmessage = function (e) { postMessage('pong ' + e.data); };
