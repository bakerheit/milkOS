// A Richards-style operating-system simulation: a scheduler running an idle task, a worker,
// two handlers and two devices that pass packets around. Heavy on method calls, property
// access and object allocation. Returns a checksum of queue and hold counts.
(function () {
  var ID_IDLE = 0, ID_WORKER = 1, ID_HANDLER_A = 2, ID_HANDLER_B = 3, ID_DEVICE_A = 4, ID_DEVICE_B = 5;
  var KIND_DEVICE = 0, KIND_WORK = 1;
  var DATA_SIZE = 4;

  function Packet(link, id, kind) {
    this.link = link;
    this.id = id;
    this.kind = kind;
    this.a1 = 0;
    this.a2 = new Array(DATA_SIZE).fill(0);
  }
  Packet.prototype.addTo = function (queue) {
    this.link = null;
    if (queue === null) return this;
    var next = queue;
    while (next.link !== null) next = next.link;
    next.link = this;
    return queue;
  };

  class TaskState {
    constructor() { this.packetPending = true; this.taskWaiting = false; this.taskHolding = false; }
    isRunnable() { return this.packetPending && !this.taskWaiting && !this.taskHolding; }
    isWaitingWithPacket() { return this.packetPending && this.taskWaiting && !this.taskHolding; }
    isHeldOrSuspended() { return this.taskHolding || (!this.packetPending && this.taskWaiting); }
  }

  class Tcb extends TaskState {
    constructor(link, id, priority, queue, task) {
      super();
      this.link = link; this.id = id; this.priority = priority; this.queue = queue; this.task = task;
      // suspended (waiting), and runnable too when it has packets queued
      this.packetPending = queue !== null;
      this.taskWaiting = true;
    }
    checkPriorityAdd(task, packet) {
      if (this.queue === null) {
        this.queue = packet;
        this.packetPending = true;
        if (this.priority > task.priority) return this;
      } else {
        this.queue = packet.addTo(this.queue);
      }
      return task;
    }
    run() {
      var packet;
      if (this.isWaitingWithPacket()) {
        packet = this.queue;
        this.queue = packet.link;
        if (this.queue === null) { this.packetPending = false; this.taskWaiting = false; }
        else { this.packetPending = true; this.taskWaiting = false; }
      } else {
        packet = null;
      }
      return this.task.run(packet);
    }
  }

  class Scheduler {
    constructor() {
      this.queueCount = 0; this.holdCount = 0;
      this.blocks = new Array(6).fill(null);
      this.list = null; this.currentTcb = null; this.currentId = null;
    }
    addTask(id, priority, queue, task) {
      this.currentTcb = new Tcb(this.list, id, priority, queue, task);
      this.list = this.currentTcb;
      this.blocks[id] = this.currentTcb;
    }
    addRunningTask(id, priority, queue, task) {
      this.addTask(id, priority, queue, task);
      this.currentTcb.packetPending = false; this.currentTcb.taskWaiting = false; this.currentTcb.taskHolding = false;
    }
    schedule() {
      this.currentTcb = this.list;
      while (this.currentTcb !== null) {
        if (this.currentTcb.isHeldOrSuspended()) {
          this.currentTcb = this.currentTcb.link;
        } else {
          this.currentId = this.currentTcb.id;
          this.currentTcb = this.currentTcb.run();
        }
      }
    }
    release(id) {
      var tcb = this.blocks[id];
      if (tcb === null) return tcb;
      tcb.taskHolding = false;
      return tcb.priority > this.currentTcb.priority ? tcb : this.currentTcb;
    }
    holdCurrent() {
      this.holdCount++;
      this.currentTcb.taskHolding = true;
      return this.currentTcb.link;
    }
    suspendCurrent() {
      this.currentTcb.taskWaiting = true;
      return this.currentTcb;
    }
    queue(packet) {
      var t = this.blocks[packet.id];
      if (t === null) return t;
      this.queueCount++;
      packet.link = null;
      packet.id = this.currentId;
      return t.checkPriorityAdd(this.currentTcb, packet);
    }
  }

  function IdleTask(s, v1, count) { this.s = s; this.v1 = v1; this.count = count; }
  IdleTask.prototype.run = function () {
    this.count--;
    if (this.count === 0) return this.s.holdCurrent();
    if ((this.v1 & 1) === 0) { this.v1 = this.v1 >> 1; return this.s.release(ID_DEVICE_A); }
    this.v1 = (this.v1 >> 1) ^ 0xD008;
    return this.s.release(ID_DEVICE_B);
  };
  function DeviceTask(s) { this.s = s; this.v1 = null; }
  DeviceTask.prototype.run = function (packet) {
    if (packet === null) {
      if (this.v1 === null) return this.s.suspendCurrent();
      var v = this.v1; this.v1 = null;
      return this.s.queue(v);
    }
    this.v1 = packet;
    return this.s.holdCurrent();
  };
  function WorkerTask(s, v1, v2) { this.s = s; this.v1 = v1; this.v2 = v2; }
  WorkerTask.prototype.run = function (packet) {
    if (packet === null) return this.s.suspendCurrent();
    this.v1 = this.v1 === ID_HANDLER_A ? ID_HANDLER_B : ID_HANDLER_A;
    packet.id = this.v1;
    packet.a1 = 0;
    for (var i = 0; i < DATA_SIZE; i++) {
      this.v2++;
      if (this.v2 > 26) this.v2 = 1;
      packet.a2[i] = this.v2;
    }
    return this.s.queue(packet);
  };
  function HandlerTask(s) { this.s = s; this.v1 = null; this.v2 = null; }
  HandlerTask.prototype.run = function (packet) {
    if (packet !== null) {
      if (packet.kind === KIND_WORK) this.v1 = packet.addTo(this.v1);
      else this.v2 = packet.addTo(this.v2);
    }
    if (this.v1 !== null) {
      var count = this.v1.a1, v;
      if (count < DATA_SIZE) {
        if (this.v2 !== null) {
          v = this.v2; this.v2 = this.v2.link;
          v.a1 = this.v1.a2[count];
          this.v1.a1 = count + 1;
          return this.s.queue(v);
        }
      } else {
        v = this.v1; this.v1 = this.v1.link;
        return this.s.queue(v);
      }
    }
    return this.s.suspendCurrent();
  };

  function runRichards(count) {
    var s = new Scheduler();
    s.addRunningTask(ID_IDLE, 0, null, new IdleTask(s, 1, count));
    var q = new Packet(null, ID_WORKER, KIND_WORK);
    q = new Packet(q, ID_WORKER, KIND_WORK);
    s.addTask(ID_WORKER, 1000, q, new WorkerTask(s, ID_HANDLER_A, 0));
    q = new Packet(null, ID_DEVICE_A, KIND_DEVICE);
    q = new Packet(q, ID_DEVICE_A, KIND_DEVICE);
    q = new Packet(q, ID_DEVICE_A, KIND_DEVICE);
    s.addTask(ID_HANDLER_A, 2000, q, new HandlerTask(s));
    q = new Packet(null, ID_DEVICE_B, KIND_DEVICE);
    q = new Packet(q, ID_DEVICE_B, KIND_DEVICE);
    q = new Packet(q, ID_DEVICE_B, KIND_DEVICE);
    s.addTask(ID_HANDLER_B, 3000, q, new HandlerTask(s));
    s.addTask(ID_DEVICE_A, 4000, null, new DeviceTask(s));
    s.addTask(ID_DEVICE_B, 5000, null, new DeviceTask(s));
    s.schedule();
    return [s.queueCount, s.holdCount];
  }
  var r = runRichards(10000);
  return r[0] + ',' + r[1];
})();
