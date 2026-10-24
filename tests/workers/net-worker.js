// fetch, XMLHttpRequest and importScripts from a worker: the worker's own origin, the page's cookies
// (same site, same origin), CORS for the other origin, the script type check.
function report(group, name, value) { console.log('WRKTEST ' + group + ' ' + name + ' ' + value); }
var API = 'http://api.milk.test:@PORT@';
var pending = 0;
function done() { if (--pending === 0) postMessage('finished'); }
function task(name, p) {
  pending++;
  p.then(function (v) { return v; }, function (e) { return 'failed ' + (e && e.name) + ': ' + (e && e.message); }).then(function (v) { report('net', name, v); postMessage({ n: name, v: String(v) }); }).then(done);
}
task('same-origin', fetch('/data.json').then(function (r) { return r.json(); }).then(function (j) { return 'answer ' + j.answer; }));
task('cors-open', fetch(API + '/cors/open.json').then(function (r) { return r.json(); }).then(function (j) { return 'open ' + j.open; }));
task('cors-closed', fetch(API + '/cors/closed.json').then(function (r) { return r.json(); }));
task('echo', fetch('/echo').then(function (r) { return r.json(); }).then(function (j) {
  return 'cookie=' + j.cookie + ' site=' + j.site + ' referer=' + String(j.referer).replace(/:\d+/, ':PORT');
}));
task('echo-cross', fetch(API + '/echo').then(function (r) { return r.json(); }).then(function (j) {
  return 'cookie=' + j.cookie + ' origin=' + String(j.origin).replace(/:\d+/, ':PORT') + ' site=' + j.site;
}));
task('echo-include', fetch(API + '/echo', { credentials: 'include' }).then(function (r) { return r.json(); }).then(function (j) { return 'cookie=' + j.cookie; }));
task('xhr', new Promise(function (resolve, reject) {
  var x = new XMLHttpRequest();
  x.open('GET', '/data.json');
  x.onload = function () { resolve('status ' + x.status + ' ' + x.responseText.replace(/\s+/g, '')); };
  x.onerror = function () { reject(new Error('xhr error')); };
  x.send();
}));
task('xhr-blocked', new Promise(function (resolve, reject) {
  var x = new XMLHttpRequest();
  x.open('GET', API + '/cors/closed.json');
  x.onload = function () { resolve('loaded?!'); };
  x.onerror = function () { resolve('error event'); };
  x.send();
}));
pending++;
(function () {
  var out = [];
  var tries = [['cors', API + '/cors/script.js'], ['nocors', API + '/nocors/script.js'], ['badtype', '/bad-type.js'], ['same', '/lib/part-a.js']];
  tries.forEach(function (t) {
    try { importScripts(t[1]); out.push(t[0] + ':ok'); } catch (e) { out.push(t[0] + ':' + e.name); }
  });
  var v = out.join(' ') + ' corsLoaded=' + typeof corsLoaded + ' nocorsLoaded=' + typeof nocorsLoaded;
  report('net', 'import', v);
  postMessage({ n: 'import', v: v });
  done();
})();
report('net', 'worker-started', self.name);
