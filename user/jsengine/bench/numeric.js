// Floating-point work (an n-body step loop and a Mandelbrot grid): softfloat in the guest.
(function () {
  var bodies = [];
  for (var i = 0; i < 5; i++) bodies.push({ x: i, y: i * 0.5, z: -i, vx: 0.01 * i, vy: 0, vz: -0.01 * i, m: 1 + i });
  for (var step = 0; step < 2000; step++) {
    for (var a = 0; a < bodies.length; a++) {
      var A = bodies[a];
      for (var b = a + 1; b < bodies.length; b++) {
        var B = bodies[b];
        var dx = A.x - B.x, dy = A.y - B.y, dz = A.z - B.z;
        var d2 = dx * dx + dy * dy + dz * dz + 0.01;
        var mag = 0.001 / (d2 * Math.sqrt(d2));
        A.vx -= dx * B.m * mag; A.vy -= dy * B.m * mag; A.vz -= dz * B.m * mag;
        B.vx += dx * A.m * mag; B.vy += dy * A.m * mag; B.vz += dz * A.m * mag;
      }
    }
    for (var c = 0; c < bodies.length; c++) { var C = bodies[c]; C.x += C.vx; C.y += C.vy; C.z += C.vz; }
  }
  var inside = 0;
  for (var py = 0; py < 60; py++) {
    for (var px = 0; px < 80; px++) {
      var x0 = px / 40 - 2, y0 = py / 30 - 1, x = 0, y = 0, it = 0;
      while (x * x + y * y < 4 && it < 50) { var t = x * x - y * y + x0; y = 2 * x * y + y0; x = t; it++; }
      if (it === 50) inside++;
    }
  }
  return Math.round(bodies[0].x * 1e6) + ',' + inside;
})();
