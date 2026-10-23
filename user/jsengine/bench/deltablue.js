// A DeltaBlue-style one-way constraint network: variables connected by equality and
// scale constraints; editing an input re-plans and propagates through the chain.
// Exercises closures, polymorphic method calls, arrays and Maps.
(function () {
  var REQUIRED = 0, STRONG = 1, MEDIUM = 2, WEAK = 3, WEAKEST = 4;
  class Variable {
    constructor(name, value) { this.name = name; this.value = value; this.constraints = []; this.determinedBy = null; this.mark = 0; this.walkStrength = WEAKEST; this.stay = true; }
    addConstraint(c) { this.constraints.push(c); }
    removeConstraint(c) { this.constraints = this.constraints.filter(x => x !== c); if (this.determinedBy === c) this.determinedBy = null; }
  }
  class Constraint {
    constructor(strength) { this.strength = strength; }
    isSatisfied() { return this.satisfied; }
    stronger(a, b) { return a < b; }
  }
  class Unary extends Constraint {
    constructor(v, strength) { super(strength); this.output = v; this.satisfied = false; v.addConstraint(this); }
    inputsKnown() { return true; }
    chooseMethod(mark) { this.satisfied = this.output.mark !== mark && this.stronger(this.strength, this.output.walkStrength); }
    markInputs() {}
    recalculate() { this.output.walkStrength = this.strength; this.output.stay = !this.isInput(); if (this.output.stay) this.execute(); }
    outputVar() { return this.output; }
  }
  class Stay extends Unary { isInput() { return false; } execute() {} }
  class Edit extends Unary { isInput() { return true; } execute() {} }
  class Binary extends Constraint {
    constructor(a, b, strength) { super(strength); this.v1 = a; this.v2 = b; this.direction = 0; a.addConstraint(this); b.addConstraint(this); }
    isSatisfied() { return this.direction !== 0; }
    chooseMethod(mark) {
      if (this.v1.mark === mark) {
        this.direction = (this.v2.mark !== mark && this.stronger(this.strength, this.v2.walkStrength)) ? 1 : 0;
      } else if (this.v2.mark === mark) {
        this.direction = (this.v1.mark !== mark && this.stronger(this.strength, this.v1.walkStrength)) ? -1 : 0;
      } else {
        this.direction = this.v1.walkStrength > this.v2.walkStrength ? -1 : 1;
      }
    }
    input() { return this.direction === 1 ? this.v1 : this.v2; }
    outputVar() { return this.direction === 1 ? this.v2 : this.v1; }
    recalculate() {
      var i = this.input(), o = this.outputVar();
      o.walkStrength = Math.max(this.strength, i.walkStrength);
      o.stay = i.stay;
      if (o.stay) this.execute();
    }
  }
  class Equality extends Binary { execute() { this.outputVar().value = this.input().value; } }
  class Scale extends Binary {
    constructor(a, b, scale, offset, strength) { super(a, b, strength); this.scale = scale; this.offset = offset; }
    execute() {
      if (this.direction === 1) this.v2.value = this.v1.value * this.scale.value + this.offset.value;
      else this.v1.value = (this.v2.value - this.offset.value) / this.scale.value;
    }
  }
  class Planner {
    constructor() { this.mark = 0; }
    newMark() { return ++this.mark; }
    satisfy(c) {
      var mark = this.newMark();
      c.chooseMethod(mark);
      if (!c.isSatisfied()) return;
      var out = c.outputVar();
      out.mark = mark;
      out.determinedBy = c;
      c.recalculate();
      this.propagate(out, mark);
    }
    propagate(v, mark) {
      var todo = [v];
      while (todo.length) {
        var x = todo.pop();
        for (var i = 0; i < x.constraints.length; i++) {
          var c = x.constraints[i];
          if (c === x.determinedBy || !(c instanceof Binary)) continue;
          c.chooseMethod(mark);
          if (c.isSatisfied()) {
            var o = c.outputVar();
            if (o.mark === mark) continue;
            o.mark = mark;
            o.determinedBy = c;
            c.recalculate();
            todo.push(o);
          }
        }
      }
    }
  }
  function chainTest(n) {
    var planner = new Planner();
    var vars = [];
    for (var i = 0; i <= n; i++) vars.push(new Variable('v' + i, 0));
    var cs = [];
    for (var j = 0; j < n; j++) cs.push(new Equality(vars[j], vars[j + 1], REQUIRED));
    new Stay(vars[n], STRONG);
    var edit = new Edit(vars[0], STRONG);
    cs.forEach(c => planner.satisfy(c));
    var ok = 0;
    for (var k = 0; k < 100; k++) {
      vars[0].value = k;
      vars[0].mark = 0;
      planner.propagate(vars[0], planner.newMark());
      if (vars[n].value === k) ok++;
    }
    return ok;
  }
  function projectionTest(n) {
    var planner = new Planner();
    var scale = new Variable('scale', 10), offset = new Variable('offset', 1000);
    var src = [], dst = [];
    for (var i = 0; i < n; i++) {
      var s = new Variable('src' + i, i), d = new Variable('dst' + i, i);
      src.push(s); dst.push(d);
      new Stay(s, MEDIUM);
      planner.satisfy(new Scale(s, d, scale, offset, REQUIRED));
    }
    var sum = 0;
    for (var k = 0; k < 10; k++) {
      for (var i2 = 0; i2 < n; i2++) { src[i2].value = i2 + k; planner.propagate(src[i2], planner.newMark()); }
      sum += dst[n - 1].value;
    }
    return sum;
  }
  var a = 0, b = 0;
  for (var round = 0; round < 4; round++) { a += chainTest(400); b += projectionTest(400); }
  return a + ',' + b;
})();
