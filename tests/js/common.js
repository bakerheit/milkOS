// Shared by the js-test pages: report a line to the test (the Browser prints console lines
// on the serial port as "[browser] console log: ..."), and where an element is, so the test
// can click it.
function report(name, what) {
  console.log("JSTEST " + name + " " + what);
}
function where(id) {
  var r = document.getElementById(id).getBoundingClientRect();
  report("pos", id + " " + Math.round(r.left + r.width / 2) + " " + Math.round(r.top + r.height / 2));
}
