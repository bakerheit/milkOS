// String and regexp work like page scripts do: templating, splitting, replacing, joining.
(function () {
  var rows = [];
  for (var i = 0; i < 4000; i++) {
    rows.push(`<tr class="${i % 2 ? 'odd' : 'even'}"><td>${i}</td><td>Row number ${i}</td></tr>`);
  }
  var html = rows.join('\n');
  var cells = 0;
  html.replace(/<td>(\d+)<\/td>/g, function (m, n) { cells += +n; return m; });
  var words = html.split(/\s+/).length;
  var upper = html.slice(0, 20000).toUpperCase().length;
  var escaped = html.replace(/[<>&"]/g, function (c) { return { '<': '&lt;', '>': '&gt;', '&': '&amp;', '"': '&quot;' }[c]; });
  var count = (escaped.match(/&lt;tr/g) || []).length;
  var s = '';
  for (var j = 0; j < 20000; j++) s += String.fromCharCode(97 + (j % 26));
  var idx = 0;
  for (var k = 0; k < 2000; k++) idx += s.indexOf('xyz', k);
  return cells + ',' + words + ',' + upper + ',' + count + ',' + idx;
})();
