// Pages and workers of the workers test report through console.log: "WRKTEST group name value".
// A line from a worker comes out of the Browser with the worker's name on it, so the test knows who
// said it.
function report(group, name, value) {
  console.log('WRKTEST ' + group + ' ' + name + ' ' + value);
}
var STYLE = 'body { font-family: sans-serif; margin: 14px; background: #fffdf8; color: #3a2e28; } td, th { padding: 3px 10px; text-align: left; border-bottom: 1px solid #d8c8b0; } code { background: #f4ebe0; padding: 0 3px; }';
// Show a result in the page's table and report it.
function shown(group, name, value) {
  var t = document.getElementById('t');
  if (t) {
    var tr = document.createElement('tr');
    var a = document.createElement('th'); a.textContent = name;
    var b = document.createElement('td'); b.textContent = String(value);
    tr.appendChild(a); tr.appendChild(b); t.appendChild(tr);
  }
  report(group, name, value);
}
