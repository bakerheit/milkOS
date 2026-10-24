// Functions long enough to be compiled on their first call: they must behave as if compiled
// at once. (Padding comments make each of them well over the size where that starts.)
var PAD = '// padding padding padding padding padding padding padding padding padding\n';
function mk(src) { return (0, eval)('(' + src + ')'); }
test('closures over variables in loops and blocks', function () {
  var fs = [];
  for (let i = 0; i < 3; i++) {
    fs.push(function () {
      // padding padding padding padding padding padding padding padding padding padding
      let j = i * 10;
      return function () { return i + j; };
    });
  }
  assertEq(fs.map(function (f) { return f()(); }), [0, 11, 22]);
  var x = 1;
  function outer() {
    // padding padding padding padding padding padding padding padding padding padding
    var y = 2;
    function inner() {
      // padding padding padding padding padding padding padding padding padding padding
      x += y; y += 1;
      return x * 100 + y;
    }
    return inner;
  }
  var inner = outer();
  assertEq(inner(), 303);
  assertEq(inner(), 604);
  assertEq(x, 6);
});
test('arguments, this, new.target and super in long functions', function () {
  var o = {
    base: 5,
    m: function () {
      // padding padding padding padding padding padding padding padding padding padding
      var a = arguments;
      var arrow = () => this.base + a.length + arguments[0];
      return arrow();
    },
  };
  assertEq(o.m(10, 20), 17);
  function F() {
    // padding padding padding padding padding padding padding padding padding padding
    this.nt = new.target === F;
  }
  assertEq(new F().nt, true);
  class A { hi() { return 'A'; } }
  class B extends A {
    hi() {
      // padding padding padding padding padding padding padding padding padding padding
      return super.hi() + 'B';
    }
  }
  assertEq(new B().hi(), 'AB');
  class P {
    #secret = 42;
    get() {
      // padding padding padding padding padding padding padding padding padding padding
      return this.#secret + this.#twice();
    }
    #twice() { return this.#secret * 2; }
    static has(o) { return #secret in o; }
  }
  assertEq(new P().get(), 126);
  assertEq(P.has(new P()), true);
  assertEq(P.has({}), false);
});
test('generators, async functions and named expressions', function () {
  function* g(n) {
    // padding padding padding padding padding padding padding padding padding padding
    for (var i = 0; i < n; i++) yield i * i;
    return 'done';
  }
  assertEq(Array.from(g(4)), [0, 1, 4, 9]);
  var fact = function f(n) {
    // padding padding padding padding padding padding padding padding padding padding
    return n <= 1 ? 1 : n * f(n - 1);
  };
  assertEq(fact(6), 720);
  var other = fact;
  fact = null;
  assertEq(other(5), 120);
  assertEq(other.name, 'f');
  assertEq(other.length, 1);
  assert(other.toString().indexOf('padding') > 0);
  var withDefaults = function (a, b = 2, ...rest) {
    // padding padding padding padding padding padding padding padding padding padding
    return a + b + rest.length;
  };
  assertEq(withDefaults.length, 1);
  assertEq(withDefaults(1), 3);
  assertEq(withDefaults(1, 5, 7, 8), 8);
});
asyncTest('async lazily compiled', async function () {
  async function f(x) {
    // padding padding padding padding padding padding padding padding padding padding
    await null;
    return x + 1;
  }
  assertEq(await f(1), 2);
  var arr = await Promise.all([1, 2, 3].map(async (v) => {
    // padding padding padding padding padding padding padding padding padding padding
    return v * 2;
  }));
  assertEq(arr, [2, 4, 6]);
});
test('errors in long functions come with the right place', function () {
  function thrower() {
    // padding padding padding padding padding padding padding padding padding padding
    null.x;
  }
  try { thrower(); } catch (e) {
    assert(e instanceof TypeError);
    assert(/lazy\.js:\d+:\d+/.test(e.stack), e.stack);
  }
  assertThrows(SyntaxError, function () { mk('function () { let a; let a; ' + PAD + '}'); });
  assertThrows(SyntaxError, function () { new Function('a', 'return a +;' + PAD); });
  var f = new Function('a', 'b', PAD + 'return a * b');
  assertEq(f(6, 7), 42);
  assertEq(mk('function () { return eval("1 + 1") ' + PAD + '}')(), 2);
});
test('strictness carries into long functions', function () {
  function sloppy() {
    // padding padding padding padding padding padding padding padding padding padding
    return typeof this;
  }
  function strict() {
    'use strict';
    // padding padding padding padding padding padding padding padding padding padding
    return typeof this;
  }
  assertEq(sloppy(), 'object');
  assertEq(strict(), 'undefined');
  var outerStrict = (function () {
    'use strict';
    return function () {
      // padding padding padding padding padding padding padding padding padding padding
      return typeof this;
    };
  })();
  assertEq(outerStrict(), 'undefined');
});
