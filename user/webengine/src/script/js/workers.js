// ---- Worker (the page's end of a dedicated worker) -----------------------------------------------------
// The thread, the limits and the messages are the app's and script/worker; this is the class a page
// sees. `postMessage` copies the value (structured clone) and gives away the buffers and ports of its
// transfer list; the worker's answers come back as `message` and `error` events at this object.

// The bytes of a blob: address this page made (for `new Worker(blobURL)`), or undefined.
function blobBytesOf(href) {
  const b = blobUrls.get(href);
  return b === undefined ? undefined : b[S].bytes;
}

class Worker extends NetEventTarget {
  constructor(scriptURL, options) {
    if (arguments.length === 0) throw new TypeError("Failed to construct 'Worker': 1 argument required, but only 0 present.");
    super();
    let type = 'classic';
    let name = '';
    if (options !== undefined && options !== null) {
      if (!isObj(options)) throw new TypeError("Failed to construct 'Worker': The provided value is not of type 'WorkerOptions'.");
      if (options.type !== undefined) {
        type = str(options.type);
        if (type !== 'classic' && type !== 'module') {
          throw new TypeError("Failed to construct 'Worker': The provided value '" + type + "' is not a valid enum value of type WorkerType.");
        }
      }
      if (options.name !== undefined) name = str(options.name);
    }
    const s = setSlots(this, { brand: 'Worker', id: 0 });
    s.id = native.workerNew(this, str(scriptURL), type, name, blobBytesOf);
  }
  postMessage(message) {
    need(arguments, 1, 'postMessage', 'Worker');
    native.workerPost(slots(this, 'Worker').id, message, arguments[1]);
  }
  terminate() {
    native.workerTerminate(slots(this, 'Worker').id);
  }
}
tag(Worker);
handlerAttrs(Worker, ['message', 'messageerror', 'error']);
enumerable(Worker, ['postMessage', 'terminate']);

// An event from the worker, for its object (script/worker/page.rs). Returns true when an `error`
// event was cancelled.
function deliverWorker(w, kind, a, b, c, d) {
  if (kind === 'message' || kind === 'messageerror') {
    fire(w, native.messageEvent(kind, a, b));
    return false;
  }
  const ev = new G.ErrorEvent('error', { message: a, filename: b, lineno: c, colno: d, error: null, cancelable: true });
  fire(w, ev);
  return ev.defaultPrevented;
}

