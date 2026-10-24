function report(group, name, value) { console.log('WRKTEST ' + group + ' ' + name + ' ' + value); }
onmessage = function (e) {
  report('runaway', 'worker-begins', self.name);
  var n = 0;
  while (true) { n++; }
};
