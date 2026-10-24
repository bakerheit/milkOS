
// ---- MessageChannel, MessagePort ----------------------------------------------------------------------
// The ports themselves are host objects made by `native.channel`; the table of what is waiting,
// who owns which port, and the delivery as tasks of the event loop are in script/ports.rs. A port
// is an event target: `message` events (and `messageerror` for a message that cannot be read)
// come to its listeners and `onmessage`, which also starts it. The ones with a listener are held
// by the realm until they are closed, so the other end can still reach them.

class MessagePort extends NetEventTarget {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  postMessage(message) {
    need(arguments, 1, 'postMessage', 'MessagePort');
    let list = arguments[1];
    if (isObj(list) && !Array.isArray(list) && typeof list[SymIter] !== 'function') list = list.transfer;
    if (list !== undefined && list !== null) {
      if (!isObj(list) || typeof list[SymIter] !== 'function') {
        throw new TypeError("Failed to execute 'postMessage' on 'MessagePort': The provided value cannot be converted to a sequence.");
      }
      list = Array.from(list);
    } else {
      list = [];
    }
    native.portPost(this, message, list);
  }
  start() {
    native.portStart(this);
  }
  close() {
    native.portClose(this);
  }
  addEventListener(type, callback, options) {
    NetEventTarget.prototype.addEventListener.call(this, type, callback, options);
    if (callback !== null && callback !== undefined && String(type) === 'message') native.portHold(this, true);
  }
}
tag(MessagePort);
handlerAttrs(MessagePort, ['message', 'messageerror']);
{
  // Setting `onmessage` starts the port.
  const d = Object.getOwnPropertyDescriptor(MessagePort.prototype, 'onmessage');
  ObjDefine(MessagePort.prototype, 'onmessage', {
    get: d.get,
    set(v) {
      d.set.call(this, v);
      if (isObj(v)) {
        native.portHold(this, true);
        native.portStart(this);
      }
    },
    enumerable: true,
    configurable: true,
  });
}
enumerable(MessagePort, ['postMessage', 'start', 'close']);

class MessageChannel {
  constructor() {
    const ports = native.channel(MessagePort.prototype);
    setSlots(this, { brand: 'MessageChannel', port1: ports[0], port2: ports[1] });
  }
  get port1() {
    return slots(this, 'MessageChannel').port1;
  }
  get port2() {
    return slots(this, 'MessageChannel').port2;
  }
}
tag(MessageChannel);
enumerable(MessageChannel, ['port1', 'port2']);

// A message for `port` (the realm's event loop): the data, or `messageerror` when the copy
// cannot be read.
function deliverPort(port, text, ports) {
  let data;
  let type = 'message';
  try {
    data = text === '' ? undefined : native.decode(text, ports);
  } catch (e) {
    type = 'messageerror';
  }
  const ev = native.messageEvent(type, data, ports);
  fire(port, ev);
}
