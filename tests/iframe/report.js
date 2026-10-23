// Pages of the iframe test report through console.log: "IFRTEST group name value". A line from a
// frame comes out of the Browser with the frame's name on it, so the test knows who said it.
function report(group, name, value) {
  console.log('IFRTEST ' + group + ' ' + name + ' ' + value);
}
function errName(e) {
  return (e && e.name ? e.name : 'Error') + (e && e.message ? ': ' + e.message : '');
}
function tryit(fn) {
  try { return String(fn()); } catch (e) { return 'threw ' + (e && e.name); }
}
var STYLE = 'body { font-family: sans-serif; margin: 14px; background: #fffdf8; color: #3a2e28; } iframe { border: 2px solid #b8a48c; background: #fff; display: block; margin: 6px 0; }';
