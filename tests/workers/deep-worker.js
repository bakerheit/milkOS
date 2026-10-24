// Things that eat native stack: a worker has a thread of its own with a smaller stack than the page's,
// so the engine's limits must hold (an error in script, never a crash).
function report(group, name, value) { console.log('WRKTEST ' + group + ' ' + name + ' ' + value); }
function rec(n) { return rec(n + 1) + 1; }
try { rec(0); report('deep', 'recursion', 'no error?'); } catch (e) { report('deep', 'recursion', e.name); }
var nested = '[' .repeat(4000) + ']'.repeat(4000);
try { JSON.parse(nested); report('deep', 'json', 'parsed'); } catch (e) { report('deep', 'json', e.name); }
try { eval(nested); report('deep', 'eval', 'ran'); } catch (e) { report('deep', 'eval', e.name); }
try { new RegExp('('.repeat(3000) + 'a' + ')'.repeat(3000)).test('a'); report('deep', 'regexp', 'ran'); } catch (e) { report('deep', 'regexp', e.name); }
var o = {}; var cur = o; for (var i = 0; i < 20000; i++) { cur.n = {}; cur = cur.n; }
try { structuredClone(o); report('deep', 'clone', 'cloned'); } catch (e) { report('deep', 'clone', e.name); }
try { JSON.stringify(o); report('deep', 'stringify', 'done'); } catch (e) { report('deep', 'stringify', e.name); }
try { postMessage(o); report('deep', 'post', 'sent'); } catch (e) { report('deep', 'post', e.name); }
var f = function () {}; for (var j = 0; j < 3000; j++) { f = (function (g) { return function () { return g() + 1; }; })(f); }
try { f(); report('deep', 'closures', 'ran'); } catch (e) { report('deep', 'closures', e.name); }
report('deep', 'alive', 'yes');
postMessage('end');
