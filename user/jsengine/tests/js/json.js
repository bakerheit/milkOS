test('parse', function () {
  var o = JSON.parse('{"a":[1,2,{"b":null}],"c":"x\\ny","d":true,"e":-1.5e2}');
  assertEq(o.a[2].b, null);
  assertEq(o.c, 'x\ny');
  assertEq(o.e, -150);
  assertEq(JSON.parse(' "\\u0041" '), 'A');
  assertEq(JSON.parse('[]').length, 0);
  assertThrows(SyntaxError, function () { JSON.parse('{a:1}'); });
  assertThrows(SyntaxError, function () { JSON.parse("'x'"); });
  assertThrows(SyntaxError, function () { JSON.parse('[1,]'); });
  assertThrows(SyntaxError, function () { JSON.parse(''); });
  assertThrows(SyntaxError, function () { JSON.parse('01'); });
  var rev = JSON.parse('{"a":1,"b":{"c":2}}', function (k, v) { return typeof v === 'number' ? v * 10 : v; });
  assertEq(rev.b.c, 20);
  var del = JSON.parse('{"a":1,"b":2}', function (k, v) { return k === 'a' ? undefined : v; });
  assertEq(Object.keys(del), ['b']);
  assertEq(JSON.parse('{"__proto__": 1}').__proto__, 1);
  var deep = '';
  for (var i = 0; i < 500; i++) deep += '[';
  for (var j = 0; j < 500; j++) deep += ']';
  assert(Array.isArray(JSON.parse(deep)));
});
test('stringify', function () {
  assertEq(JSON.stringify({ a: 1, b: 'x', c: [true, null] }), '{"a":1,"b":"x","c":[true,null]}');
  assertEq(JSON.stringify('he said "hi"\n'), '"he said \\"hi\\"\\n"');
  assertEq(JSON.stringify({ u: undefined, f: function () {}, s: Symbol() }), '{}');
  assertEq(JSON.stringify([undefined, function () {}]), '[null,null]');
  assertEq(JSON.stringify(undefined), undefined);
  assertEq(JSON.stringify(NaN), 'null');
  assertEq(JSON.stringify(new Date(0)), '"1970-01-01T00:00:00.000Z"');
  assertEq(JSON.stringify({ toJSON() { return 'custom'; } }), '"custom"');
  assertEq(JSON.stringify({ a: 1, b: 2 }, ['b']), '{"b":2}');
  assertEq(JSON.stringify({ a: 1, b: 2 }, (k, v) => typeof v === 'number' ? v + 1 : v), '{"a":2,"b":3}');
  assertEq(JSON.stringify({ a: [1, { b: 2 }] }, null, 2), '{\n  "a": [\n    1,\n    {\n      "b": 2\n    }\n  ]\n}');
  assertEq(JSON.stringify([1], null, '--'), '[\n--1\n]');
  assertEq(JSON.stringify({}, null, 2), '{}');
  assertEq(JSON.stringify(new String('s')), '"s"');
  assertEq(JSON.stringify(' \ud800'), '" \\ud800"');
  var cyc = {};
  cyc.self = cyc;
  assertThrows(TypeError, function () { JSON.stringify(cyc); });
  assertEq(JSON.stringify(Object.create({ inherited: 1 })), '{}');
  assertEq(JSON.stringify({ 2: 'b', 1: 'a' }), '{"1":"a","2":"b"}');
});
test('round trip', function () {
  var data = { name: 'milk', list: [1, 2.5, -3e-7, 'x'], nested: { deep: { t: true, n: null } } };
  var back = JSON.parse(JSON.stringify(data));
  assertEq(JSON.stringify(back), JSON.stringify(data));
  assertEq(back.list[2], -3e-7);
});
