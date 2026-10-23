// Private class members, default-parameter TDZ and the other class details modules code relies on.

test('private fields and methods', () => {
  class A {
    #x = 1;
    #y;
    static #count = 0;
    constructor() { A.#count++; this.#y = 2; }
    get sum() { return this.#x + this.#y; }
    #double() { return this.#x * 2; }
    callDouble() { return this.#double(); }
    static get count() { return A.#count; }
  }
  const a = new A();
  new A();
  assertEq(a.sum, 3);
  assertEq(a.callDouble(), 2);
  assertEq(A.count, 2);
  assertEq(Object.keys(a), []);
  assertEq(Reflect.ownKeys(a), []);
  assertEq(Object.getOwnPropertySymbols(a), []);
  assertEq(JSON.stringify(a), '{}');
  assertEq(Object.getOwnPropertyNames(A.prototype).sort(), ['callDouble', 'constructor', 'sum']);
});

test('private accessors', () => {
  class A {
    #v = 0;
    get #p() { return this.#v; }
    set #p(x) { this.#v = x * 2; }
    static get #sp() { return 's'; }
    run() { this.#p = 5; this.#p += 1; return [this.#p, A.#sp]; }
  }
  assertEq(new A().run(), [22, 's']);
  class B {
    get #g() { return 1; }
    set #s(v) {}
    tryBoth() {
      const r = [];
      try { this.#g = 2; } catch (e) { r.push(e.constructor === TypeError); }
      try { this.#s; } catch (e) { r.push(e.constructor === TypeError); }
      return r;
    }
  }
  assertEq(new B().tryBoth(), [true, true]);
  class C {
    set #both(v) { this.last = v; }
    get #both() { return 'got'; }
    t() { this.#both = 1; return this.#both; }
  }
  assertEq(new C().t(), 'got');
});

test('private methods are shared and not writable', () => {
  class A {
    #m() { return 1; }
    static same(a, b) { return a.#m === b.#m; }
    write() { this.#m = 2; }
  }
  assert(A.same(new A(), new A()));
  assertThrows(TypeError, () => new A().write());
});

test('brand checks', () => {
  class A {
    #x = 1;
    static has(o) { return #x in o; }
    static read(o) { return o.#x; }
    static write(o) { o.#x = 5; }
  }
  assertEq(A.has(new A()), true);
  assertEq(A.has({}), false);
  assertEq(A.has(function () {}), false);
  assertThrows(TypeError, () => A.has(1));
  assertThrows(TypeError, () => A.has(null));
  assertThrows(TypeError, () => A.read({}));
  assertThrows(TypeError, () => A.read(null));
  assertThrows(TypeError, () => A.read(undefined));
  assertThrows(TypeError, () => A.read(1));
  assertThrows(TypeError, () => A.write({}));
  class B extends A {}
  assertEq(A.read(new B()), 1);
  class Other { #x = 'other'; static read(o) { return o.#x; } }
  assertThrows(TypeError, () => Other.read(new A()));
  assertThrows(TypeError, () => A.read(new Other()));
});

test('each class evaluation has its own private names', () => {
  function make() { return class { #x = 1; static read(o) { return o.#x; } }; }
  const A = make(), B = make();
  assertEq(A.read(new A()), 1);
  assertThrows(TypeError, () => A.read(new B()));
});

test('derived classes and field order', () => {
  const log = [];
  class Base {
    #b = log.push('base field');
    constructor() { log.push('base ctor'); }
    baseSees() { return this.#b; }
  }
  class Derived extends Base {
    #d = log.push('derived field');
    constructor() { super(); log.push('derived ctor'); }
    derivedSees() { return this.#d; }
  }
  const d = new Derived();
  assertEq(log, ['base field', 'base ctor', 'derived field', 'derived ctor']);
  assertEq(d.baseSees(), 1);
  assertEq(d.derivedSees(), 3);
  assertThrows(TypeError, () => Derived.prototype.derivedSees.call(new Base()));
});

test('private methods are there before fields run', () => {
  class A {
    x = this.#m();
    #m() { return 'ready'; }
  }
  assertEq(new A().x, 'ready');
  class B {
    #f = this.#g;
    get #g() { return 'getter'; }
    f() { return this.#f; }
  }
  assertEq(new B().f(), 'getter');
});

test('initializing twice is an error', () => {
  class Base { constructor(o) { return o; } }
  class A extends Base {
    #x = 1;
    static has(o) { return #x in o; }
  }
  const o = {};
  new A(o);
  assert(A.has(o));
  assertThrows(TypeError, () => new A(o));
  class B extends Base {
    #m() {}
    static has(o) { return #m in o; }
  }
  const p = {};
  new B(p);
  assertThrows(TypeError, () => new B(p));
});

test('frozen and sealed objects keep working private fields', () => {
  class A {
    #n = 0;
    constructor() { Object.freeze(this); }
    inc() { return ++this.#n; }
  }
  const a = new A();
  assertEq(a.inc(), 1);
  assertEq(a.inc(), 2);
  assert(Object.isFrozen(a));
  class Base { constructor() { Object.preventExtensions(this); } }
  class D extends Base { #late = 'added'; get() { return this.#late; } }
  assertEq(new D().get(), 'added');
});

test('operators on private members', () => {
  class A {
    #n = 1; #s = 'a'; #z = null; #o = { k: 1 };
    run() {
      this.#n += 4; this.#n *= 2; this.#n **= 2; this.#n -= 1;
      this.#s += 'b';
      this.#z ??= 'set'; this.#z ||= 'unchanged'; this.#z &&= 'and';
      const r = [this.#n++, ++this.#n, this.#n--, --this.#n];
      this.#o.k++;
      return [this.#n, this.#s, this.#z, r, this.#o.k];
    }
  }
  assertEq(new A().run(), [99, 'ab', 'and', [99, 101, 101, 99], 2]);
});

test('destructuring into private members', () => {
  class A {
    #a; #b; #c;
    run(o, arr) {
      ({ a: this.#a, b: this.#b } = o);
      [this.#c] = arr;
      return [this.#a, this.#b, this.#c];
    }
  }
  assertEq(new A().run({ a: 1, b: 2 }, [3]), [1, 2, 3]);
  class B {
    #x;
    run() { for (this.#x of [1, 2, 3]); return this.#x; }
  }
  assertEq(new B().run(), 3);
});

test('optional chains, calls and templates', () => {
  class A {
    #v = 'v';
    #f() { return this.#v; }
    static read(o) { return o?.#v; }
    static call(o) { return o?.#f(); }
    static deep(o) { return o?.a.#v; }
    #tag(s, ...v) { return s.raw.join('|') + v.join(); }
    tpl() { return this.#tag`x${1}y`; }
  }
  assertEq(A.read(new A()), 'v');
  assertEq(A.read(null), undefined);
  assertEq(A.call(new A()), 'v');
  assertEq(A.call(undefined), undefined);
  assertEq(A.deep({ a: new A() }), 'v');
  assertEq(A.deep(null), undefined);
  assertEq(new A().tpl(), 'x|y1');
});

test('closures and arrows see the private names', () => {
  class A {
    #x = 'x';
    getter() { return () => this.#x; }
    async am() { await null; return this.#x; }
    *gen() { yield this.#x; }
    static #sm() { return 'static'; }
    static s() { return (() => A.#sm())(); }
  }
  const a = new A();
  assertEq(a.getter()(), 'x');
  assertEq([...a.gen()], ['x']);
  assertEq(A.s(), 'static');
  asyncTest('async method', async () => { assertEq(await a.am(), 'x'); });
});

test('nested classes and shadowing', () => {
  class Outer {
    #x = 'outer';
    static Inner = class {
      #x = 'inner';
      read(o) { return o.#x; }
    };
    readOuter(o) { return o.#x; }
    innerReadsOuter() {
      const self = this;
      return new (class { peek() { return self.#x; } })().peek();
    }
  }
  const i = new Outer.Inner();
  assertEq(i.read(i), 'inner');
  assertThrows(TypeError, () => i.read(new Outer()));
  assertEq(new Outer().readOuter(new Outer()), 'outer');
  assertEq(new Outer().innerReadsOuter(), 'outer');
});

test('static private members', () => {
  class A {
    static #secret = 42;
    static #m() { return A.#secret; }
    static get() { return A.#m(); }
    static viaThis() { return this.#secret; }
    static { A.initialized = A.#secret + 1; }
  }
  assertEq(A.get(), 42);
  assertEq(A.initialized, 43);
  assertEq(A.viaThis(), 42);
  class B extends A {}
  assertThrows(TypeError, () => B.viaThis());
  assertEq(Object.getOwnPropertyNames(A).sort(), ['get', 'initialized', 'length', 'name', 'prototype', 'viaThis']);
});

test('private fields with function values are named', () => {
  class A {
    #f = () => {};
    #g = function () {};
    #h = class {};
    names() { return [this.#f.name, this.#g.name, this.#h.name]; }
    #m() {}
    mname() { return this.#m.name; }
  }
  assertEq(new A().names(), ['#f', '#g', '#h']);
  assertEq(new A().mname(), '#m');
});

test('private names are invisible', () => {
  class A { #x = 1; }
  const a = new A();
  assertEq(Object.keys(a).length + Object.getOwnPropertyNames(a).length + Object.getOwnPropertySymbols(a).length, 0);
  assertEq(Object.entries(a), []);
  assertEq(Object.keys({ ...a }).length, 0);
  assertEq(Object.keys(Object.assign({}, a)).length, 0);
  assertEq(structuredClone(a) instanceof Object, true);
  assertEq(String(Object.getOwnPropertyDescriptors(a) && Object.keys(Object.getOwnPropertyDescriptors(a))), '');
  for (const k in a) throw new Error('enumerated ' + k);
  assertEq('#x' in a, false);
  assertEq(a['#x'], undefined);
});

test('private syntax errors', () => {
  const bad = [
    'class A { m() { return this.#nope; } }',
    'class A { #x; #x; }',
    'class A { #x; static #x; }',
    'class A { get #x() {} get #x() {} }',
    'class A { static get #x() {} set #x(v) {} }',
    'class A { #constructor; }',
    'class A { #x; m() { delete this.#x; } }',
    'class A { #x; m() { delete (this.#x); } }',
    'class A { #x; m() { return super.#x; } }',
    'class A { #x; m() { return #x; } }',
    'class A { #x; m() { return #x + 1 in this; } }',
    'class A { #x; m() { return 1 + #x in this; } }',
    'this.#x',
    '({ #x: 1 })',
    'class A { #x; m(o) { return o.# x; } }',
    'function f(o) { return #x in o; }',
    'class A { #x; m() { return (#x) in this; } }',
  ];
  for (const src of bad) {
    assertThrows(SyntaxError, () => (0, eval)(src), src);
  }
  const good = [
    'class A { #x; m(o) { return #x in o; } }',
    'class A { #x; m(o) { return o?.#x; } }',
    'class A { static #x; static m(o) { return #x in o && o.#x; } }',
    'class A { get #x() {} set #x(v) {} }',
    'class A { #a; #b; m(o) { return #a in o && #b in o; } }',
    'class A { #x; m() { class B { #x; n() { return this.#x; } } return B; } }',
    'class A { #x; m(a) { return (a || this).#x; } }',
  ];
  for (const src of good) (0, eval)('{ ' + src + ' }');
});

test('private names in a class expression with a name and heritage', () => {
  const Base = class { static make() { return new this(); } };
  const D = class Named extends Base { #p = 'p'; get() { return this.#p; } };
  assertEq(D.make().get(), 'p');
});

test('toString and inspection of classes with private members', () => {
  class A { #x = 1; m() { return this.#x; } }
  assert(String(A).includes('#x'));
  assertEq(typeof new A().m, 'function');
});

test('default parameters are in their TDZ', () => {
  assertThrows(ReferenceError, () => (function (a = b, b = 1) { return a; })());
  assertThrows(ReferenceError, () => (function (a = a) { return a; })());
  assertThrows(ReferenceError, () => ((a = b, b = 2) => a)());
  assertThrows(ReferenceError, () => (function ({ x = y }, y = 2) { return x; })({}));
  assertThrows(ReferenceError, () => (function (a = (() => b)(), b = 5) { return a; })());
  assertEq((function (a, b = a + 1, c = a + b) { return [a, b, c]; })(1), [1, 2, 3]);
  assertEq((function (a = () => b, b = 5) { return a(); })(), 5);
  assertEq((function (a = 1) { var a; return a; })(), 1);
  assertEq((function (a = 1) { var a = 2; return a; })(), 2);
  assertEq((function (a, a2 = a) { a = 9; return [a, a2]; })(1), [9, 1]);
  assertEq((function f(a = typeof f) { return a; })(), 'function');
  assertEq((function (a = 1, ...rest) { return [a, rest.length]; })(undefined, 7, 8), [1, 2]);
  assertEq((function* (a = 1, b = a) { yield a + b; })().next().value, 2);
  assertEq((function (a, b) { return a + b; })(1, 2), 3);
  assertEq((function (a, a) { return a; })(1, 2), 2);
});

test('class field initialization order', () => {
  const log = [];
  class A {
    [(log.push('key1'), 'k1')] = log.push('init1');
    static [(log.push('key2'), 'k2')] = log.push('static init2');
    ['k3'] = log.push('init3');
    static { log.push('static block'); }
    constructor() { log.push('ctor'); }
  }
  assertEq(log, ['key1', 'key2', 'static init2', 'static block']);
  new A();
  assertEq(log.slice(4), ['init1', 'init3', 'ctor']);
});

test('fields see this and earlier fields', () => {
  class A {
    a = 1;
    b = this.a + 1;
    c = () => this.b + 1;
    static s = this.name + '!';
  }
  const x = new A();
  assertEq([x.a, x.b, x.c()], [1, 2, 3]);
  assertEq(A.s, 'A!');
});

test('error cause and hasOwn and at and replaceAll', () => {
  const e = new Error('outer', { cause: new TypeError('inner') });
  assertEq(e.cause.message, 'inner');
  assertEq('cause' in new Error('x'), false);
  assertEq(Object.hasOwn(e, 'cause'), true);
  assertEq([1, 2, 3].at(-1), 3);
  assertEq('abc'.at(-1), 'c');
  assertEq('a.b.c'.replaceAll('.', '/'), 'a/b/c');
  assertEq('aaa'.replaceAll(/a/g, (m, i) => i), '012');
  assertThrows(TypeError, () => 'a'.replaceAll(/a/, 'b'));
});
