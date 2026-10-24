test('WeakRef holds its target', function () {
  var o = { a: 1 };
  var r = new WeakRef(o);
  assertEq(r.deref(), o);
  assertEq(Object.prototype.toString.call(r), '[object WeakRef]');
  assert(r instanceof WeakRef);
  assertEq(Object.getOwnPropertySymbols(r).length, 0);
  assertEq(Reflect.ownKeys(r).length, 0);
  var s = Symbol('s');
  assertEq(new WeakRef(s).deref(), s);
  assertThrows(TypeError, function () { new WeakRef(1); });
  assertThrows(TypeError, function () { new WeakRef('x'); });
  assertThrows(TypeError, function () { new WeakRef(Symbol.for('registered')); });
  assertThrows(TypeError, function () { WeakRef({}); });
  assertThrows(TypeError, function () { WeakRef.prototype.deref.call({}); });
  class Sub extends WeakRef { }
  assertEq(new Sub(o).deref(), o);
  assert(new Sub(o) instanceof Sub);
});
test('FinalizationRegistry takes registrations', function () {
  var called = 0;
  var f = new FinalizationRegistry(function () { called++; });
  var o = {}, token = {};
  assertEq(f.register(o, 'held', token), undefined);
  assertEq(f.register({}, 'held'), undefined);
  assertEq(f.unregister(token), true);
  assertEq(f.unregister(token), false);
  assertEq(f.unregister({}), false);
  assertThrows(TypeError, function () { f.register(1, 'x'); });
  assertThrows(TypeError, function () { f.register(o, o); });
  assertThrows(TypeError, function () { f.register(o, 1, 5); });
  assertThrows(TypeError, function () { f.unregister(5); });
  assertThrows(TypeError, function () { new FinalizationRegistry(); });
  assertThrows(TypeError, function () { new FinalizationRegistry(1); });
  assertThrows(TypeError, function () { FinalizationRegistry(function () { }); });
  assertThrows(TypeError, function () { FinalizationRegistry.prototype.register.call({}, o, 1); });
  assertEq(Object.prototype.toString.call(f), '[object FinalizationRegistry]');
  // The huge number of registrations with tokens stays bounded.
  for (var i = 0; i < 1000; i++) f.register({}, i, {});
  assertEq(called, 0);
});
