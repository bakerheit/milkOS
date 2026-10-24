function report(group, name, value) { console.log('WRKTEST ' + group + ' ' + name + ' ' + value); }
var ports = [];
onmessage = function (e) {
  var d = e.data;
  if (d.kind === 'buffer') {
    var u = new Uint8Array(d.buf);
    var sum = 0;
    for (var i = 0; i < u.length; i++) sum += u[i];
    report('xfer', 'worker-buffer', d.buf.byteLength + ' sum=' + sum);
    var back = new Uint32Array(d.buf.byteLength >> 2);
    for (var j = 0; j < back.length; j++) back[j] = j * 3;
    postMessage({ kind: 'back', buf: back.buffer, len: back.length }, [back.buffer]);
    report('xfer', 'worker-detached', back.buffer.byteLength);
  } else if (d.kind === 'port') {
    var p = e.ports[0];
    report('xfer', 'worker-port', e.ports.length + ' ' + (p === d.port));
    p.onmessage = function (m) { p.postMessage('echo:' + m.data); };
    p.postMessage('ready');
  } else if (d.kind === 'make') {
    var mc = new MessageChannel();
    mc.port1.onmessage = function (m) { mc.port1.postMessage('upper:' + String(m.data).toUpperCase()); };
    postMessage({ kind: 'made' }, [mc.port2]);
  } else if (d.kind === 'clone') {
    postMessage({ date: new Date(86400000), map: new Map([[1, 'a']]), set: new Set([1, 2]), re: /x+/gi, nested: { arr: [1, [2, 3]] }, big: 'é' + 'x'.repeat(1000), neg0: -0, nan: NaN, undef: undefined });
  }
};
