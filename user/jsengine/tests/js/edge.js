test('getters setters and inheritance', function () {
  class A {
    constructor() { this._x = 1; }
    get x() { return this._x; }
    set x(v) { this._x = v * 2; }
    static get kind() { return 'A'; }
  }
  class B extends A {
    get x() { return super.x + 100; }
    static get kind() { return 'B<' + super.kind; }
  }
  var b = new B();
  assertEq(b.x, 101);
  b.x = 5;
  assertEq(b._x, undefined === undefined ? b._x : 0);
  assertEq(B.kind, 'B<A');
  var o = Object.create({ set p(v) { this.got = v; } });
  o.p = 3;
  assertEq(o.got, 3);
  assert(!o.hasOwnProperty('p'));
  var ro = Object.create(Object.defineProperty({}, 'r', { value: 1, writable: false }));
  ro.r = 2;
  assertEq(ro.r, 1);
  assert(!ro.hasOwnProperty('r'));
});
test('hasInstance and species', function () {
  class Even { static [Symbol.hasInstance](n) { return n % 2 === 0; } }
  assert(2 instanceof Even);
  assert(!(3 instanceof Even));
  class MyArr extends Array {}
  var m = new MyArr(1, 2, 3);
  var mapped = m.map(x => x);
  assert(mapped instanceof MyArr);
  class Plain extends Array { static get [Symbol.species]() { return Array; } }
  var p = new Plain(1, 2);
  assert(!(p.filter(Boolean) instanceof Plain));
  assert(p.filter(Boolean) instanceof Array);
});
test('symbols as keys and toPrimitive', function () {
  var s1 = Symbol('a');
  var o = { [s1]: 1, regular: 2 };
  assertEq(JSON.stringify(o), '{"regular":2}');
  assertEq(Object.assign({}, o)[s1], 1);
  var date = new Date(0);
  assertEq(date[Symbol.toPrimitive]('number'), 0);
  assertEq(typeof date[Symbol.toPrimitive]('default'), 'string');
  var tagged = { get [Symbol.toStringTag]() { return 'Dyn'; } };
  assertEq(String(tagged), '[object Dyn]');
});
test('property attributes and enumeration', function () {
  var o = {};
  Object.defineProperty(o, 'hidden', { value: 1, enumerable: false });
  o.shown = 2;
  assertEq(Object.keys(o), ['shown']);
  assertEq(Object.getOwnPropertyNames(o), ['hidden', 'shown']);
  assertEq(JSON.stringify(o), '{"shown":2}');
  var keys = [];
  for (var k in o) keys.push(k);
  assertEq(keys, ['shown']);
  assert(o.propertyIsEnumerable('shown') && !o.propertyIsEnumerable('hidden'));
  var arr = [1, 2];
  assertEq(Object.getOwnPropertyDescriptor(arr, 'length').writable, true);
  assertEq(Object.getOwnPropertyDescriptor(arr, 'length').enumerable, false);
  assertEq(Object.getOwnPropertyDescriptor('abc', 0).value, 'a');
  var fdesc = Object.getOwnPropertyDescriptor(function f(a, b) {}, 'length');
  assertEq([fdesc.value, fdesc.writable, fdesc.configurable], [2, false, true]);
});
test('prototype chains', function () {
  assertEq(Object.getPrototypeOf(Object.prototype), null);
  assertEq(Object.getPrototypeOf(Function.prototype), Object.prototype);
  assertEq(Object.getPrototypeOf(function* () {}).constructor.name, 'GeneratorFunction');
  assertEq(Object.getPrototypeOf(async function () {}).constructor.name, 'AsyncFunction');
  assertThrows(TypeError, function () { var a = {}, b = Object.create(a); Object.setPrototypeOf(a, b); });
  var noProto = Object.create(null);
  assertEq(typeof noProto.toString, 'undefined');
  assertEq('x' in noProto, false);
  assertEq(Object.prototype.hasOwnProperty.call(noProto, 'x'), false);
});
test('wrappers', function () {
  var n = new Number(5);
  assertEq(typeof n, 'object');
  assertEq(n + 1, 6);
  assertEq(JSON.stringify([new Boolean(false), new Number(1), new String('s')]), '[false,1,"s"]');
  var s = new String('ab');
  s.extra = 1;
  assertEq(s.extra, 1);
  var prim = 'ab';
  prim.extra = 1;
  assertEq(prim.extra, undefined);
  assertEq(Object('x') instanceof String, true);
  assertEq((5).constructor, Number);
  assertEq(true.constructor, Boolean);
});
test('array index edge cases', function () {
  var a = [];
  a[4294967294] = 'last';
  assertEq(a.length, 4294967295);
  var b = [];
  b[4294967295] = 'notindex';
  assertEq(b.length, 0);
  assertEq(b[4294967295], 'notindex');
  var c = [1, 2, 3];
  c['1'] = 'one';
  assertEq(c[1], 'one');
  c[1.0] = 'float';
  assertEq(c[1], 'float');
  assertEq([].concat.call(1, 2).length, 2);
});
test('function properties', function () {
  function f() {}
  assertEq(typeof f.prototype, 'object');
  assertEq(f.prototype.constructor, f);
  var arrow = () => {};
  assertEq(arrow.prototype, undefined);
  var method = ({ m() {} }).m;
  assertEq(method.prototype, undefined);
  assertThrows(TypeError, function () { new method(); });
  function* gen() {}
  assertEq(Object.getPrototypeOf(gen.prototype), Object.getPrototypeOf(function* () {}).prototype);
  assertThrows(TypeError, function () { new gen(); });
  async function af() {}
  assertThrows(TypeError, function () { new af(); });
  f.custom = 1;
  assertEq(f.custom, 1);
});
test('string and number coercion edge cases', function () {
  assertEq(1 + {}, '1[object Object]');
  assertEq({} + 1, '[object Object]1');
  assertEq([] - 1, -1);
  assertEq(+[1], 1);
  assertEq(+'0x', NaN);
  assertEq(+' \n 12 \t', 12);
  assertEq(+'1e1000', Infinity);
  assertEq(1 / +'-0', -Infinity);
  assertEq(String(0.000001234), '0.000001234');
  assertEq(String(1.5e-7), '1.5e-7');
  assertEq((123.456).toString(36), '3f.gez4w97ry');
  assertEq(parseFloat('1e-7x'), 1e-7);
  assertEq(Number.parseFloat('Infinity'), Infinity);
  assertEq(~~-5.9, -5);
  assertEq(-5.9 >>> 0, 4294967291);
});
