
// ---- the rest of the Event family ---------------------------------------------------------------------
// Subclasses of the engine's own Event / UIEvent / MouseEvent: their members are plain readonly
// data properties, as the built-in ones are.

function eventNeeds(args, name) {
  if (args.length === 0) throw new TypeError("Failed to construct '" + name + "': 1 argument required, but only 0 present.");
}

function eventMembers(ev, init, defs) {
  for (const k of Object.keys(defs)) {
    let v = isObj(init) && init[k] !== undefined ? init[k] : defs[k];
    ObjDefine(ev, k, { value: v, enumerable: true, configurable: true });
  }
}

class WheelEvent extends G.MouseEvent {
  constructor(type, init) {
    eventNeeds(arguments, 'WheelEvent');
    super(type, init);
    eventMembers(this, init, { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 });
    for (const k of ['deltaX', 'deltaY', 'deltaZ', 'deltaMode']) ObjDefine(this, k, { value: Number(this[k]) || 0, enumerable: true, configurable: true });
  }
}
constants(WheelEvent, { DOM_DELTA_PIXEL: 0, DOM_DELTA_LINE: 1, DOM_DELTA_PAGE: 2 });
tag(WheelEvent);

class CompositionEvent extends G.UIEvent {
  constructor(type, init) {
    eventNeeds(arguments, 'CompositionEvent');
    super(type, init);
    eventMembers(this, init, { data: '' });
  }
}
tag(CompositionEvent);

class StorageEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'StorageEvent');
    super(type, init);
    eventMembers(this, init, { key: null, oldValue: null, newValue: null, url: '', storageArea: null });
  }
}
tag(StorageEvent);

class PopStateEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'PopStateEvent');
    super(type, init);
    eventMembers(this, init, { state: null });
  }
}
tag(PopStateEvent);

class HashChangeEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'HashChangeEvent');
    super(type, init);
    eventMembers(this, init, { oldURL: '', newURL: '' });
  }
}
tag(HashChangeEvent);

class PageTransitionEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'PageTransitionEvent');
    super(type, init);
    eventMembers(this, init, { persisted: false });
  }
}
tag(PageTransitionEvent);

class BeforeUnloadEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'BeforeUnloadEvent');
    super(type, init);
    ObjDefine(this, 'returnValue', { value: '', writable: true, enumerable: true, configurable: true });
  }
}
tag(BeforeUnloadEvent);

class TouchEvent extends G.UIEvent {
  constructor(type, init) {
    eventNeeds(arguments, 'TouchEvent');
    super(type, init);
    eventMembers(this, init, { touches: [], targetTouches: [], changedTouches: [], altKey: false, ctrlKey: false, shiftKey: false, metaKey: false });
  }
}
tag(TouchEvent);

class AnimationEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'AnimationEvent');
    super(type, init);
    eventMembers(this, init, { animationName: '', elapsedTime: 0, pseudoElement: '' });
  }
}
tag(AnimationEvent);

class TransitionEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'TransitionEvent');
    super(type, init);
    eventMembers(this, init, { propertyName: '', elapsedTime: 0, pseudoElement: '' });
  }
}
tag(TransitionEvent);

class CloseEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'CloseEvent');
    super(type, init);
    eventMembers(this, init, { wasClean: false, code: 0, reason: '' });
  }
}
tag(CloseEvent);

// DataTransfer (what a drag or a paste carries): the items are kept in script, nothing reaches
// the system's clipboard or drag and drop.
class DataTransfer {
  constructor() {
    setSlots(this, { brand: 'DataTransfer', data: new Map(), dropEffect: 'none', effectAllowed: 'all' });
  }
  get dropEffect() {
    return slots(this, 'DataTransfer').dropEffect;
  }
  set dropEffect(v) {
    v = String(v);
    if (['none', 'copy', 'link', 'move'].indexOf(v) >= 0) slots(this, 'DataTransfer').dropEffect = v;
  }
  get effectAllowed() {
    return slots(this, 'DataTransfer').effectAllowed;
  }
  set effectAllowed(v) {
    slots(this, 'DataTransfer').effectAllowed = String(v);
  }
  get types() {
    return Array.from(slots(this, 'DataTransfer').data.keys());
  }
  get files() {
    return [];
  }
  get items() {
    return [];
  }
  getData(type) {
    const v = slots(this, 'DataTransfer').data.get(str(type).toLowerCase());
    return v === undefined ? '' : v;
  }
  setData(type, value) {
    slots(this, 'DataTransfer').data.set(str(type).toLowerCase(), str(value));
  }
  clearData(type) {
    const d = slots(this, 'DataTransfer').data;
    if (type === undefined) d.clear();
    else d.delete(str(type).toLowerCase());
  }
  setDragImage() {}
}
tag(DataTransfer);
enumerable(DataTransfer, ['dropEffect', 'effectAllowed', 'types', 'files', 'items', 'getData', 'setData', 'clearData', 'setDragImage']);

class ClipboardEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'ClipboardEvent');
    super(type, init);
    eventMembers(this, init, { clipboardData: null });
  }
}
tag(ClipboardEvent);

class DragEvent extends G.MouseEvent {
  constructor(type, init) {
    eventNeeds(arguments, 'DragEvent');
    super(type, init);
    eventMembers(this, init, { dataTransfer: null });
  }
}
tag(DragEvent);
