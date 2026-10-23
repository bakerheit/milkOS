// JSON round trip: build a ~1 MB document of nested records, stringify, parse, compare.
(function () {
  var items = [];
  for (var i = 0; i < 6000; i++) {
    items.push({
      id: i,
      name: 'item-' + i,
      price: Math.round(i * 37.5) / 100,
      tags: ['a' + (i % 7), 'b' + (i % 11), 'milk'],
      dims: { w: i % 100, h: (i * 3) % 100, d: 1.5 },
      active: i % 3 === 0,
      note: i % 10 === 0 ? 'quote " and \\ and \n newline' : null,
    });
  }
  var doc = { generated: '2026-10-23', count: items.length, items: items };
  var total = 0, len = 0;
  for (var r = 0; r < 3; r++) {
    var text = JSON.stringify(doc);
    len = text.length;
    var back = JSON.parse(text);
    total += back.items.length + back.items[5999].dims.h;
  }
  return len + ',' + total;
})();
