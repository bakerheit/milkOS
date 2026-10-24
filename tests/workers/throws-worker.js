onmessage = function (e) {
  postMessage('about to fail');
  null.nothing;
};
setTimeout(function () { throw new RangeError('from a timer'); }, 300);
