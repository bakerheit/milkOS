importScripts('lib/part-a.js', 'lib/part-b.js');
var out = fromB() + ' ' + parts.join('');
try { importScripts('/nothing-here.js'); out += ' missing:no-error'; } catch (e) { out += ' missing:' + e.name; }
postMessage(out);
