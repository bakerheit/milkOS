self.onerror = function (message, source, line, col, err) { postMessage('handled in the worker: ' + message + ' ' + (err instanceof Error)); return true; };
setTimeout(function () { throw new Error('inside'); }, 50);
