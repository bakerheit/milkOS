// Pages and workers of the memory test report through console.log: "MEMTEST group name value".
// A line from a worker carries the worker's name in the Browser's serial log.
function report(group, name, value) {
  console.log('MEMTEST ' + group + ' ' + name + ' ' + value);
}
var STYLE = 'body { font-family: sans-serif; margin: 14px; background: #fffdf8; color: #3a2e28; }';
// The query string as an object (mb=100&kind=objects).
function args(search) {
  var out = {};
  String(search || '').replace(/^\?/, '').split('&').forEach(function (p) {
    if (!p) return;
    var i = p.indexOf('=');
    out[decodeURIComponent(i < 0 ? p : p.slice(0, i))] = i < 0 ? '' : decodeURIComponent(p.slice(i + 1));
  });
  return out;
}
