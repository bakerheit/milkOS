test('basic class', function () {
  class Point {
    constructor(x, y) { this.x = x; this.y = y; }
    norm() { return Math.sqrt(this.x * this.x + this.y * this.y); }
    get sum() { return this.x + this.y; }
    set sum(v) { this.x = v; this.y = 0; }
    static origin() { return new Point(0, 0); }
    toString() { return `(${this.x}, ${this.y})`; }
  }
  var p = new Point(3, 4);
  assertEq(p.norm(), 5);
  assertEq(p.sum, 7);
  p.sum = 10;
  assertEq(p.x + p.y, 10);
  assertEq(Point.origin().x, 0);
  assertEq(String(p), '(10, 0)');
  assertEq(typeof Point, 'function');
  assertThrows(TypeError, function () { Point(1, 2); });
  assertEq(Object.keys(Point.prototype), []);
  assertEq(Point.name, 'Point');
  assertEq(Point.prototype.constructor, Point);
});
test('inheritance', function () {
  class A {
    constructor(n) { this.n = n; }
    hello() { return 'A' + this.n; }
    static make() { return new this(1); }
  }
  class B extends A {
    constructor(n) { super(n * 10); this.b = true; }
    hello() { return 'B' + super.hello(); }
  }
  var b = new B(2);
  assertEq(b.hello(), 'BA20');
  assert(b instanceof B && b instanceof A);
  assertEq(B.make().n, 10);
  assert(B.make() instanceof B);
  assertEq(Object.getPrototypeOf(B), A);
  class C extends B {}
  var c = new C(1);
  assertEq(c.n, 10);
  assertEq(c.hello(), 'BA10');
  class D extends A { constructor() { var x = 1; super(x); } }
  assertEq(new D().n, 1);
  class Bad extends A { constructor() { this.x = 1; super(); } }
  assertThrows(ReferenceError, function () { new Bad(); });
  class NoSuper extends A { constructor() {} }
  assertThrows(ReferenceError, function () { new NoSuper(); });
});
test('fields', function () {
  var counter = 0;
  class F {
    a = 1;
    b = this.a + 1;
    ['c' + 'd'] = 3;
    e;
    static s = 'static';
    static t = F.s + '!';
    id = ++counter;
    arrow = () => this.a;
  }
  var f = new F();
  assertEq([f.a, f.b, f.cd, f.e], [1, 2, 3, undefined]);
  assert('e' in f);
  assertEq(F.s, 'static');
  assertEq(F.t, 'static!');
  new F();
  assertEq(counter, 2);
  var arrow = f.arrow;
  assertEq(arrow(), 1);
  class G extends F { g = this.a * 10; constructor() { super(); this.after = this.g; } }
  var g = new G();
  assertEq(g.g, 10);
  assertEq(g.after, 10);
});
test('static blocks and accessors', function () {
  var log = [];
  class S {
    static x = 1;
    static { log.push(this.x); this.y = 2; }
    static get z() { return this.y + 1; }
  }
  assertEq(log, [1]);
  assertEq(S.z, 3);
});
test('super properties in objects', function () {
  var base = { greet() { return 'hi'; } };
  var derived = { __proto__: base, greet() { return super.greet() + '!'; } };
  assertEq(derived.greet(), 'hi!');
});
test('extending builtins', function () {
  class MyArr extends Array {
    sum() { return this.reduce((a, b) => a + b, 0); }
  }
  var m = new MyArr();
  m.push(1, 2, 3);
  assertEq(m.length, 3);
  assertEq(m.sum(), 6);
  assert(Array.isArray(m));
  class MyErr extends Error {
    constructor(msg) { super(msg); this.name = 'MyErr'; }
  }
  var e = new MyErr('boom');
  assert(e instanceof Error && e instanceof MyErr);
  assertEq(e.message, 'boom');
  assertEq(String(e), 'MyErr: boom');
  class MyMap extends Map { getOr(k, d) { return this.has(k) ? this.get(k) : d; } }
  var mm = new MyMap([[1, 'a']]);
  assertEq(mm.getOr(1, 'z') + mm.getOr(2, 'z'), 'az');
  class P2 extends Promise {}
  assert(P2.resolve(1) instanceof P2);
});
test('class expressions and names', function () {
  var K = class Inner { who() { return Inner.name; } };
  assertEq(new K().who(), 'Inner');
  assertEq(K.name, 'Inner');
  var Anon = class {};
  assertEq(Anon.name, 'Anon');
  assertThrows(TypeError, function () { class X extends 5 {} });
  class N extends null { constructor() { return Object.create(N.prototype); } }
  assertEq(Object.getPrototypeOf(N.prototype), null);
});
test('new.target', function () {
  function F() { return new.target; }
  assertEq(F(), undefined);
  assertEq(new F() === F, true);
  var nt;
  function G() { nt = new.target; }
  new G();
  assertEq(nt, G);
  class A { constructor() { this.t = new.target.name; } }
  class B extends A {}
  assertEq(new B().t, 'B');
});
test('generators and async methods', function () {
  class C {
    *gen() { yield 1; yield 2; }
    static *sg() { yield 'x'; }
    async am() { return 5; }
  }
  assertEq([...new C().gen()], [1, 2]);
  assertEq([...C.sg()], ['x']);
  assert(new C().am() instanceof Promise);
});
