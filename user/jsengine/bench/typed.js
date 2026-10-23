// Typed arrays: element loops over Float64Array and Uint8Array, DataView reads and writes,
// sort/map/filter/slice on a Uint32Array, and a TextEncoder / TextDecoder / btoa round trip.
(function () {
  var n = 50000;
  var f = new Float64Array(n);
  for (var r = 0; r < 10; r++) for (var i = 0; i < n; i++) f[i] = f[i] * 0.5 + i;
  var fs = 0;
  for (var j = 0; j < n; j++) fs += f[j];

  var u = new Uint8Array(n);
  for (var r2 = 0; r2 < 10; r2++) for (var k = 0; k < n; k++) u[k] = (u[k] + k) & 255;
  var us = 0;
  for (var m = 0; m < n; m++) us += u[m];

  var dv = new DataView(new ArrayBuffer(8 * 4096));
  for (var d = 0; d < 4096; d++) { dv.setFloat64(d * 8, d / 3, true); dv.setUint16(d * 8, d, d % 2 === 0); }
  var ds = 0;
  for (var e = 0; e < 4096; e++) ds += dv.getUint16(e * 8, e % 2 === 0) + dv.getInt8(e * 8 + 7);

  var a = new Uint32Array(40000);
  for (var p = 0; p < a.length; p++) a[p] = (p * 2654435761) >>> 0;
  var sorted = a.slice().sort();
  var ok = true;
  for (var q = 1; q < sorted.length; q++) if (sorted[q - 1] > sorted[q]) ok = false;
  var mapped = a.map(function (x) { return x & 1023; });
  var odd = a.filter(function (x) { return x & 1; });
  var sum = a.reduce(function (s, x) { return (s + x) >>> 0; }, 0);

  var text = '';
  for (var t = 0; t < 400; t++) text += 'row ' + t + ' € 😀 ';
  var bytes = new TextEncoder().encode(text);
  var back = new TextDecoder().decode(bytes);
  var b64 = btoa(String.fromCharCode.apply(null, bytes.subarray(0, 3000)));
  var raw = atob(b64);

  return [Math.round(fs), us, ds, ok, mapped.length, odd.length, sum, bytes.length, back === text, raw.length].join();
})();
