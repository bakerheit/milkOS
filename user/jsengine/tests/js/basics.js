// Values, operators, conversions, typeof, equality.
test('arithmetic', function () {
  assertEq(1 + 2 * 3, 7);
  assertEq((1 + 2) * 3, 9);
  assertEq(7 % 3, 1);
  assertEq(-7 % 3, -1);
  assertEq(2 ** 10, 1024);
  assertEq(2 ** -1, 0.5);
  assertEq(2 ** 3 ** 2, 512);
  assertEq(0.1 + 0.2, 0.30000000000000004);
  assertEq(1 / 0, Infinity);
  assertEq(-1 / 0, -Infinity);
  assert(isNaN(0 / 0));
  assertEq(-0 * 1, -0);
  assertEq(0 * -1, -0);
  assertEq(5 / 2, 2.5);
  assertEq(6 / 3, 2);
  assertEq(2147483647 + 1, 2147483648);
  assertEq(-2147483648 - 1, -2147483649);
  assertEq(65536 * 65536, 4294967296);
  assertEq(-0 + 0, 0);
  assertEq(5.5 % 2, 1.5);
  assertEq(-(-2147483648), 2147483648);
});
test('bitwise', function () {
  assertEq(5 & 3, 1);
  assertEq(5 | 3, 7);
  assertEq(5 ^ 3, 6);
  assertEq(~5, -6);
  assertEq(1 << 31, -2147483648);
  assertEq(-1 >>> 0, 4294967295);
  assertEq(-8 >> 1, -4);
  assertEq(1 << 33, 2);
  assertEq(2.9 | 0, 2);
  assertEq(-2.9 | 0, -2);
  assertEq(4294967296 | 0, 0);
  assertEq(NaN | 0, 0);
});
test('strings and +', function () {
  assertEq('a' + 1, 'a1');
  assertEq(1 + '2', '12');
  assertEq(1 + 2 + '3', '33');
  assertEq('3' * '4', 12);
  assertEq('10' - 1, 9);
  assertEq([] + [], '');
  assertEq([] + {}, '[object Object]');
  assertEq([1, 2] + '', '1,2');
  assertEq(true + 1, 2);
  assertEq(null + 1, 1);
  assert(isNaN(undefined + 1));
  assertEq('' + null, 'null');
  var s = '';
  for (var i = 0; i < 100; i++) s += i % 10;
  assertEq(s.length, 100);
  assertEq(s.slice(0, 12), '012345678901');
});
test('comparison', function () {
  assert(1 < 2);
  assert('a' < 'b');
  assert('B' < 'a');
  assert('10' < '9');
  assert(10 > '9');
  assert(!(NaN < 1) && !(NaN >= 1));
  assert(null >= 0 && !(null > 0));
  assert(!(undefined >= 0));
  assert([2] > 1);
});
test('equality', function () {
  assert(1 == '1');
  assert(null == undefined);
  assert(!(null == 0));
  assert(0 == '');
  assert('0' == false);
  assert(!(NaN == NaN));
  assert([1] == 1);
  assert(!(1 === '1'));
  var o = {};
  assert(o === o && o == o && !(o == {}));
  assert('abc' === 'ab' + 'c');
  assert(-0 === 0);
  assert(Object.is(NaN, NaN) && !Object.is(0, -0));
});
test('typeof', function () {
  assertEq(typeof 1, 'number');
  assertEq(typeof 'x', 'string');
  assertEq(typeof true, 'boolean');
  assertEq(typeof undefined, 'undefined');
  assertEq(typeof null, 'object');
  assertEq(typeof {}, 'object');
  assertEq(typeof [], 'object');
  assertEq(typeof function () {}, 'function');
  assertEq(typeof class {}, 'function');
  assertEq(typeof Symbol(), 'symbol');
  assertEq(typeof notDefinedAnywhere, 'undefined');
  assertEq(typeof Math.max, 'function');
});
test('logical', function () {
  assertEq(1 && 2, 2);
  assertEq(0 && 2, 0);
  assertEq(0 || 'x', 'x');
  assertEq(null ?? 'd', 'd');
  assertEq(0 ?? 'd', 0);
  assertEq('' ?? 'd', '');
  assertEq(!0, true);
  assertEq(!!'', false);
  var calls = 0;
  function f() { calls++; return true; }
  false && f();
  true || f();
  1 ?? f();
  assertEq(calls, 0);
  var a = null;
  a ??= 5;
  assertEq(a, 5);
  a ||= 6;
  assertEq(a, 5);
  a &&= 7;
  assertEq(a, 7);
});
test('conditional and comma', function () {
  assertEq(true ? 1 : 2, 1);
  assertEq(false ? 1 : 0 ? 2 : 3, 3);
  assertEq((1, 2, 3), 3);
  var x = (1, 'y');
  assertEq(x, 'y');
});
test('increment', function () {
  var i = 5;
  assertEq(i++, 5);
  assertEq(i, 6);
  assertEq(++i, 7);
  assertEq(i--, 7);
  assertEq(--i, 5);
  var o = { n: 1 };
  o.n++;
  ++o.n;
  assertEq(o.n, 3);
  var a = [1];
  a[0]++;
  assertEq(a[0], 2);
  assertEq(a[0]--, 2);
  assertEq(a[0], 1);
  var s = '5';
  s++;
  assertEq(s, 6);
});
test('compound assignment', function () {
  var x = 10;
  x += 5; x -= 3; x *= 2; x /= 4; x %= 4;
  assertEq(x, 2);
  x **= 3;
  assertEq(x, 8);
  x <<= 2; x >>= 1; x >>>= 1; x |= 1; x &= 7; x ^= 2;
  assertEq(x, 3);
  var o = { a: 'x' };
  o.a += 'y';
  o['a'] += 'z';
  assertEq(o.a, 'xyz');
});
test('void delete in instanceof', function () {
  assertEq(void 0, undefined);
  var o = { a: 1, b: 2 };
  assert(delete o.a);
  assert(!('a' in o));
  assert('b' in o);
  assert(delete o.nothing);
  assert([] instanceof Array);
  assert([] instanceof Object);
  assert(!({} instanceof Array));
  assert(0 in [5]);
  assert(!(1 in [5]));
  assertThrows(TypeError, function () { return 'a' in 'abc'; });
  assertThrows(TypeError, function () { return 1 instanceof 2; });
});
test('number literals', function () {
  assertEq(0x1F, 31);
  assertEq(0o17, 15);
  assertEq(0b101, 5);
  assertEq(1e3, 1000);
  assertEq(.5, 0.5);
  assertEq(5., 5);
  assertEq(1_000_000, 1000000);
  assertEq(010, 8);
  assertEq(019, 19);
});
test('string literals', function () {
  assertEq('a\nb'.length, 3);
  assertEq('\x41B\u{43}', 'ABC');
  assertEq('\'', "'");
  assertEq('a\
b', 'ab');
  assertEq('\u{1F600}'.length, 2);
  assertEq('\0'.charCodeAt(0), 0);
});
test('template literals', function () {
  var x = 3;
  assertEq(`a${x}b`, 'a3b');
  assertEq(`${x}${x}`, '33');
  assertEq(`line1
line2`, 'line1\nline2');
  assertEq(`${{ toString() { return 'T'; } }}`, 'T');
  assertEq(`\${x}`, '${x}');
  assertEq(`${`nested ${x}`}`, 'nested 3');
  function tag(s, ...v) { return s.raw.join('|') + ':' + v.join(','); }
  assertEq(tag`a${1}b${2}c`, 'a|b|c:1,2');
  assertEq(tag`\n`, '\\n:');
  function same(s) { return s; }
  var a = []; for (var i = 0; i < 2; i++) a.push(same`x`);
  assert(a[0] === a[1], 'template object is cached per site');
  assert(Object.isFrozen(a[0]));
});
test('asi', function () {
  var a = 1
  var b = 2
  assertEq(a + b, 3)
  function r() {
    return
    42
  }
  assertEq(r(), undefined)
  var c = a
  ++b
  assertEq(b, 3)
  var i = 0
  i
  ++
  i
  assertEq(i, 1)
});
test('labels', function () {
  var n = 0;
  outer: for (var i = 0; i < 5; i++) {
    for (var j = 0; j < 5; j++) {
      if (j === 2) continue outer;
      if (i === 3) break outer;
      n++;
    }
  }
  assertEq(n, 6);
  blk: {
    n = 100;
    break blk;
  }
  assertEq(n, 100);
});
test('globals', function () {
  assertEq(globalThis.Math, Math);
  globalThis.__g1 = 5;
  assertEq(__g1, 5);
  var r = (0, eval)('var __g2 = 7; __g2 + 1');
  assertEq(r, 8);
  assertEq(globalThis.__g2, 7);
  assertThrows(ReferenceError, function () { return totallyUndefinedName; });
});
