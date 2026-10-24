// Ticks, and asks the server for /count/NAME now and then, so the test can tell from the server's log
// whether it is still alive.
function report(group, name, value) { console.log('WRKTEST ' + group + ' ' + name + ' ' + value); }
var n = 0;
var name = self.name || 'tick';
setInterval(function () { n++; postMessage(n); }, 50);
setInterval(function () { fetch('/count/' + name).catch(function () {}); }, 100);
