// A classic worker: a computation, and what a worker's global looks like.
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
onmessage = function (e) {
  var started = performance.now();
  var r = fib(e.data.n);
  postMessage({
    fib: r,
    name: self.name,
    scope: Object.prototype.toString.call(self),
    dom: [typeof window, typeof document, typeof localStorage].join('/'),
    loc: location.pathname,
    origin: self.origin,
    secure: isSecureContext,
    ms: typeof started,
  });
};
