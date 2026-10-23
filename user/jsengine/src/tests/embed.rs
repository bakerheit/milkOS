//! The embedding API the way the DOM bindings use it: host classes with prototypes of
//! accessors and methods, live collections through hooks, listeners kept in pins and called
//! later, finalizers, event handler attributes, error positions, promises from the host.

use crate::*;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

/// A toy document: nodes with a tag, an id, children and a dataset.
#[derive(Default)]
struct Doc {
    tags: Vec<String>,
    ids: Vec<String>,
    children: Vec<Vec<u64>>,
    data: Vec<Vec<(String, String)>>,
}

impl Doc {
    fn add(&mut self, tag: &str) -> u64 {
        self.tags.push(String::from(tag));
        self.ids.push(String::new());
        self.children.push(Vec::new());
        self.data.push(Vec::new());
        self.tags.len() as u64 - 1
    }
}

struct ElementClass;
impl HostClass for ElementClass {
    fn name(&self) -> &str {
        "HTMLElement"
    }
}

/// A live NodeList: data = the parent node; indexed hooks read the document.
struct NodeListClass(Rc<RefCell<Doc>>, Rc<RefCell<Vec<Option<Pinned>>>>, ClassId);
impl HostClass for NodeListClass {
    fn name(&self) -> &str {
        "NodeList"
    }
    fn hooks(&self) -> u8 {
        hooks::INDEXED
    }
    fn get(
        &self,
        realm: &mut Realm,
        _obj: Value,
        data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<Value>> {
        let Key::Index(i) = key else { return Ok(None) };
        let child = self.0.borrow().children[data as usize]
            .get(i as usize)
            .copied();
        Ok(child.map(|c| wrap(realm, &self.1, self.2, c)))
    }
    fn keys(&self, _realm: &mut Realm, _obj: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let n = self.0.borrow().children[data as usize].len();
        Ok((0..n as u32).map(OwnedKey::Index).collect())
    }
}

/// element.dataset: named hooks over the node's data attributes.
struct DatasetClass(Rc<RefCell<Doc>>);
impl HostClass for DatasetClass {
    fn hooks(&self) -> u8 {
        hooks::NAMED
    }
    fn get(
        &self,
        realm: &mut Realm,
        _obj: Value,
        data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<Value>> {
        let Key::Str(k) = key else { return Ok(None) };
        let v = self.0.borrow().data[data as usize]
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone());
        Ok(v.map(|v| realm.string(&v)))
    }
    fn set(
        &self,
        realm: &mut Realm,
        _obj: Value,
        data: u64,
        key: Key<'_>,
        value: Value,
    ) -> JsResult<bool> {
        let Key::Str(k) = key else { return Ok(false) };
        let v = realm.to_string(value)?;
        let mut d = self.0.borrow_mut();
        let list = &mut d.data[data as usize];
        match list.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v,
            None => list.push((String::from(k), v)),
        }
        Ok(true)
    }
    fn delete(
        &self,
        _realm: &mut Realm,
        _obj: Value,
        data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<bool>> {
        let Key::Str(k) = key else { return Ok(None) };
        self.0.borrow_mut().data[data as usize].retain(|(n, _)| n != k);
        Ok(Some(true))
    }
    fn keys(&self, _realm: &mut Realm, _obj: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        Ok(self.0.borrow().data[data as usize]
            .iter()
            .map(|(n, _)| OwnedKey::Str(n.clone()))
            .collect())
    }
}

/// One wrapper per node (identity matters: `a.firstChild === a.firstChild`).
fn wrap(
    realm: &mut Realm,
    cache: &Rc<RefCell<Vec<Option<Pinned>>>>,
    class: ClassId,
    node: u64,
) -> Value {
    if let Some(Some(p)) = cache.borrow().get(node as usize) {
        return realm.pinned(p);
    }
    let proto = realm
        .get(realm.global(), "__ElementProto")
        .unwrap_or(Value::NULL);
    let w = realm.new_host_object(class, node, proto);
    let pin = realm.pin(w);
    let mut c = cache.borrow_mut();
    if c.len() <= node as usize {
        c.resize_with(node as usize + 1, || None);
    }
    c[node as usize] = Some(pin);
    w
}

fn setup() -> (Realm, Rc<RefCell<Doc>>) {
    let mut r = Realm::new(Options::default());
    let doc = Rc::new(RefCell::new(Doc::default()));
    let cache: Rc<RefCell<Vec<Option<Pinned>>>> = Rc::new(RefCell::new(Vec::new()));
    let el = r.register_class(ElementClass);
    let nl = r.register_class(NodeListClass(doc.clone(), cache.clone(), el));
    let ds = r.register_class(DatasetClass(doc.clone()));
    let g = r.global();
    let proto = r.new_object();
    r.define(g, "__ElementProto", proto, Attrs::METHOD).unwrap();
    let ctor = r.new_class("HTMLElement", 0, proto, None, None);
    r.define(g, "HTMLElement", ctor, Attrs::METHOD).unwrap();
    let nl_proto = r.new_object();
    let values = r.intrinsic(Intrinsic::ArrayValues);
    let iter = r.well_known_symbol(WellKnown::Iterator);
    r.define(nl_proto, iter, values, Attrs::METHOD).unwrap();
    let for_each = r.intrinsic(Intrinsic::ArrayForEach);
    r.define(nl_proto, "forEach", for_each, Attrs::METHOD)
        .unwrap();
    {
        let doc = doc.clone();
        r.define_getter(
            nl_proto,
            "length",
            move |r, this, _| {
                let Some((_, data)) = r.host_info(this) else {
                    return Err(r.throw_type_error("Illegal invocation"));
                };
                Ok(Value::from(
                    doc.borrow().children[data as usize].len() as u32
                ))
            },
            None,
        )
        .unwrap();
    }
    // tagName, id (get/set), children, dataset, appendChild
    {
        let doc = doc.clone();
        r.define_getter(
            proto,
            "tagName",
            move |r, this, _| {
                let Some(n) = r.host_data(this, el) else {
                    return Err(r.throw_type_error("Illegal invocation"));
                };
                let t = doc.borrow().tags[n as usize].to_uppercase();
                Ok(r.string(&t))
            },
            None,
        )
        .unwrap();
    }
    {
        let doc_g = doc.clone();
        let doc_s = doc.clone();
        let setter: Box<NativeFn> = Box::new(move |r: &mut Realm, this: Value, args: &[Value]| {
            let Some(n) = r.host_data(this, el) else {
                return Err(r.throw_type_error("Illegal invocation"));
            };
            let v = r.to_string(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            doc_s.borrow_mut().ids[n as usize] = v;
            Ok(Value::UNDEFINED)
        });
        r.define_getter(
            proto,
            "id",
            move |r, this, _| {
                let Some(n) = r.host_data(this, el) else {
                    return Err(r.throw_type_error("Illegal invocation"));
                };
                let id = doc_g.borrow().ids[n as usize].clone();
                Ok(r.string(&id))
            },
            Some(setter),
        )
        .unwrap();
    }
    r.define_getter(
        proto,
        "children",
        move |r, this, _| {
            let Some(n) = r.host_data(this, el) else {
                return Err(r.throw_type_error("Illegal invocation"));
            };
            let p = r.get(r.global(), "__NodeListProto")?;
            Ok(r.new_host_object(nl, n, p))
        },
        None,
    )
    .unwrap();
    r.define(g, "__NodeListProto", nl_proto, Attrs::METHOD)
        .unwrap();
    r.define_getter(
        proto,
        "dataset",
        move |r, this, _| {
            let Some(n) = r.host_data(this, el) else {
                return Err(r.throw_type_error("Illegal invocation"));
            };
            let op = r.intrinsic(Intrinsic::ObjectPrototype);
            Ok(r.new_host_object(ds, n, op))
        },
        None,
    )
    .unwrap();
    {
        let doc = doc.clone();
        r.define_method(proto, "appendChild", 1, move |r, this, args| {
            let Some(p) = r.host_data(this, el) else {
                return Err(r.throw_type_error("Illegal invocation"));
            };
            let child = args.first().copied().unwrap_or(Value::UNDEFINED);
            let Some(c) = r.host_data(child, el) else {
                return Err(r.throw_type_error(
                    "Failed to execute 'appendChild' on 'Node': parameter 1 is not of type 'Node'.",
                ));
            };
            doc.borrow_mut().children[p as usize].push(c);
            Ok(child)
        })
        .unwrap();
    }
    {
        let doc = doc.clone();
        let cache = cache.clone();
        let document = r.new_object();
        r.define(g, "document", document, Attrs::METHOD).unwrap();
        r.define_method(document, "createElement", 1, move |r, _this, args| {
            let tag = r.to_string(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let n = doc.borrow_mut().add(&tag);
            Ok(wrap(r, &cache, el, n))
        })
        .unwrap();
    }
    (r, doc)
}

fn eval_str(r: &mut Realm, src: &str) -> String {
    match r.eval(src, "page.js") {
        Ok(v) => r.inspect(v),
        Err(e) => {
            let rep = r.report(&e);
            alloc::format!("error: {rep}")
        }
    }
}

#[test]
fn dom_like_bindings() {
    let (mut r, doc) = setup();
    let out = eval_str(
        &mut r,
        r#"
        var ul = document.createElement('ul');
        for (var i = 0; i < 3; i++) { var li = document.createElement('li'); li.id = 'item' + i; ul.appendChild(li); }
        var ids = [];
        for (var c of ul.children) ids.push(c.id);
        ids.push(ul.children.length, ul.children[1].tagName, ul.children[1] === ul.children[1]);
        ids.push(Array.from(ul.children).map(e => e.tagName).join());
        ids.push([...ul.children].length, Object.keys(ul.children).join());
        ul.children.forEach(function (e, i) { e.dataset.index = i; });
        ids.push(ul.children[2].dataset.index, 'index' in ul.children[2].dataset, ul instanceof HTMLElement);
        delete ul.children[2].dataset.index;
        ids.push('index' in ul.children[2].dataset);
        ul.expando = 'kept';
        ids.join('|');
        "#,
    );
    assert_eq!(
        out,
        "item0|item1|item2|3|LI|true|LI,LI,LI|3|0,1,2|2|true|true|false"
    );
    assert_eq!(doc.borrow().data[1].len(), 1);
    // Errors from natives surface as catchable exceptions.
    let out = eval_str(
        &mut r,
        r#"
        var msgs = [];
        try { ul.appendChild({}) } catch (e) { msgs.push(e instanceof TypeError, e.message) }
        try { Object.getOwnPropertyDescriptor(__ElementProto, 'id').get.call({}) } catch (e) { msgs.push(e.message) }
        try { new HTMLElement() } catch (e) { msgs.push(e.message) }
        msgs.join('|')
        "#,
    );
    assert_eq!(
        out,
        "true|Failed to execute 'appendChild' on 'Node': parameter 1 is not of type 'Node'.|Illegal invocation|Illegal constructor"
    );
    // Expandos survive collections (the wrapper is pinned in the cache).
    r.release_temps();
    r.gc();
    assert_eq!(
        eval_str(&mut r, "ul.expando + document.createElement('p').tagName"),
        "keptP"
    );
}

#[test]
fn listeners_called_later() {
    let mut r = Realm::new(Options::default());
    let listeners: Rc<RefCell<Vec<Pinned>>> = Rc::new(RefCell::new(Vec::new()));
    let l2 = listeners.clone();
    let add = r.new_function("addEventListener", 2, move |r, _this, args| {
        let f = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        if !r.is_callable(f) {
            return Err(r.throw_type_error("listener is not a function"));
        }
        l2.borrow_mut().push(r.pin(f));
        Ok(Value::UNDEFINED)
    });
    let g = r.global();
    r.set(g, "addEventListener", add).unwrap();
    r.eval("var clicks = 0; addEventListener('click', function (e) { clicks += e.detail; this.handled = true; });", "page.js").unwrap();
    r.release_temps();
    // Lots of garbage, collections, then dispatch.
    r.eval(
        "for (var i = 0; i < 50000; i++) ({ junk: [i] });",
        "junk.js",
    )
    .unwrap();
    r.release_temps();
    r.gc();
    for d in 1..=3 {
        let ev = r.new_object();
        r.set(ev, "detail", Value::int(d)).unwrap();
        let fs: Vec<Value> = listeners.borrow().iter().map(|p| r.pinned(p)).collect();
        for f in fs {
            r.call(f, ev, &[ev]).unwrap();
        }
        assert_eq!(r.get(ev, "handled").unwrap().as_bool(), Some(true));
        r.run_jobs().unwrap();
        r.release_temps();
    }
    let v = r.eval("clicks", "x.js").unwrap();
    assert_eq!(v.as_number(), Some(6.0));
    for p in listeners.borrow_mut().drain(..) {
        r.unpin(p);
    }
}

struct Counted(Rc<Cell<u32>>);
impl HostClass for Counted {
    fn finalize(&self, _data: u64) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn finalizers_run_after_collection() {
    let mut r = Realm::new(Options::default());
    let n = Rc::new(Cell::new(0));
    let class = r.register_class(Counted(n.clone()));
    let make = r.new_function("make", 0, move |r, _, _| {
        let p = r.intrinsic(Intrinsic::ObjectPrototype);
        Ok(r.new_host_object(class, 7, p))
    });
    let g = r.global();
    r.set(g, "make", make).unwrap();
    r.eval(
        "var keep = make(); for (var i = 0; i < 100; i++) make();",
        "f.js",
    )
    .unwrap();
    r.release_temps();
    r.gc();
    // A later entry runs the finalizers of what the collection freed.
    r.eval("1", "x.js").unwrap();
    assert_eq!(n.get(), 100);
}

#[test]
fn handler_attributes_and_positions() {
    let mut r = Realm::new(Options::default());
    let f = r
        .compile_function(
            &["event"],
            "return this.name + ':' + event.type",
            "page.html",
        )
        .unwrap();
    let this = r.new_object();
    let name = r.string("button");
    r.set(this, "name", name).unwrap();
    let ev = r.new_object();
    let ty = r.string("click");
    r.set(ev, "type", ty).unwrap();
    let out = r.call(f, this, &[ev]).unwrap();
    assert_eq!(r.get_string(out).as_deref(), Some("button:click"));
    // A syntax error in a handler is reported (and can't smuggle code past the wrapper).
    assert!(
        r.compile_function(&[], "}); globalThis.pwned = 1; (function(){", "page.html")
            .is_err()
    );
    let pw = r.eval("typeof pwned", "x.js").unwrap();
    assert_eq!(r.get_string(pw).as_deref(), Some("undefined"));
    // Inline scripts report the page's own line numbers.
    let e = r
        .eval_with(
            "var ok = 1;\nnull.boom;",
            &ScriptOptions {
                file: "index.html",
                first_line: 40,
                first_column: 9,
            },
        )
        .unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.name, "TypeError");
    assert_eq!((rep.file.as_str(), rep.line), ("index.html", 41));
    let e = r
        .eval_with(
            "  var = 1",
            &ScriptOptions {
                file: "index.html",
                first_line: 12,
                first_column: 5,
            },
        )
        .unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.name, "SyntaxError");
    assert_eq!((rep.line, rep.column), (12, 11));
    // A thrown non-error still has a position.
    let e = r.eval("\n\nthrow 'plain'", "t.js").unwrap_err();
    let rep = r.report(&e);
    assert_eq!(
        (rep.name.as_str(), rep.message.as_str(), rep.line),
        ("Uncaught", "plain", 3)
    );
    let e = r
        .eval(
            "function outer() { inner() }\nfunction inner() { throw new Error('deep') }\nouter()",
            "s.js",
        )
        .unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.line, 2);
    assert!(rep.stack.contains("at inner (s.js:2:"), "{}", rep.stack);
    assert!(rep.stack.contains("at outer (s.js:1:"), "{}", rep.stack);
    assert!(alloc::format!("{rep}").starts_with("Error: deep\n    at inner"));
}

#[test]
fn promises_from_the_host() {
    let mut r = Realm::new(Options::default());
    let (p, resolve, _reject) = r.new_promise();
    let g = r.global();
    r.set(g, "hostPromise", p).unwrap();
    let keep = r.pin(resolve);
    r.eval("var got; hostPromise.then(v => { got = v * 2 });", "p.js")
        .unwrap();
    r.run_jobs().unwrap();
    assert!(r.eval("got", "x.js").unwrap().is_undefined());
    let res = r.pinned(&keep);
    r.call(res, Value::UNDEFINED, &[Value::int(21)]).unwrap();
    r.run_jobs().unwrap();
    assert_eq!(r.eval("got", "x.js").unwrap().as_number(), Some(42.0));
    r.unpin(keep);
    // Unhandled rejections are reported once, handled ones not at all.
    r.eval("Promise.reject(new Error('lost')); Promise.reject(1).catch(() => {}); (async () => { throw 'async lost' })()", "u.js")
        .unwrap();
    r.run_jobs().unwrap();
    let un = r.take_unhandled_rejections();
    assert_eq!(un.len(), 2);
    let rep = r.report_value(un[0]);
    assert_eq!(rep.message, "lost");
    assert_eq!(r.inspect(un[1]), "async lost");
    assert!(r.take_unhandled_rejections().is_empty());
    // queueMicrotask-style jobs from the host.
    let f = r
        .eval("(function (a, b) { globalThis.sum = a + b })", "m.js")
        .unwrap();
    r.enqueue_job(f, &[Value::int(2), Value::int(3)]);
    assert!(r.has_jobs());
    r.run_jobs().unwrap();
    assert_eq!(r.eval("sum", "x.js").unwrap().as_number(), Some(5.0));
}

struct Tab {
    title: String,
}

#[test]
fn host_state_and_conversions() {
    let mut r = Realm::new(Options::default());
    r.set_host(Box::new(Tab {
        title: String::from("Home"),
    }));
    let get_title = r.new_function("title", 0, |r, _, _| {
        let t = r.host::<Tab>().map(|t| t.title.clone()).unwrap_or_default();
        Ok(r.string(&t))
    });
    let set_title = r.new_function("setTitle", 1, |r, _, args| {
        let v = r.to_string(args.first().copied().unwrap_or(Value::UNDEFINED))?;
        if let Some(t) = r.host::<Tab>() {
            t.title = v;
        }
        Ok(Value::UNDEFINED)
    });
    let g = r.global();
    r.set(g, "title", get_title).unwrap();
    r.set(g, "setTitle", set_title).unwrap();
    let v = r
        .eval("setTitle(title() + ' page'); title()", "t.js")
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("Home page"));
    assert_eq!(r.host::<Tab>().map(|t| t.title.as_str()), Some("Home page"));
    // Conversions run script (toString / valueOf) and propagate its exceptions.
    let o = r
        .eval(
            "({ toString() { return 'T' }, valueOf() { return 3 } })",
            "c.js",
        )
        .unwrap();
    assert_eq!(r.to_string(o).unwrap(), "T");
    assert_eq!(r.to_number(o).unwrap(), 3.0);
    let bad = r
        .eval("({ toString() { throw new Error('no') } })", "c.js")
        .unwrap();
    assert!(r.to_string(bad).is_err());
    assert_eq!(r.type_of(o), "object");
    let json = r.json_parse("{\"a\":[1,2]}").unwrap();
    assert_eq!(
        r.json_stringify(json).unwrap().as_deref(),
        Some("{\"a\":[1,2]}")
    );
    assert_eq!(r.inspect(json), "{a: [1, 2]}");
    let arr = r.new_array(&[]);
    assert_eq!(r.array_push(arr, Value::int(1)).unwrap(), 1);
    let fun = r.eval("(function named(a) {})", "f.js").unwrap();
    assert_eq!(r.inspect(fun), "ƒ named()");
    let m = r.eval("new Map([[1, 'a']])", "m.js").unwrap();
    assert_eq!(r.inspect(m), "Map(1) {1 => 'a'}");
    let e = r.eval("new TypeError('x')", "e.js").unwrap();
    assert!(r.inspect(e).starts_with("TypeError: x"));
    assert!(r.is_error(e));
}

#[test]
fn wall_clock_interrupt() {
    let mut r = Realm::new(Options::default());
    let start = std::time::Instant::now();
    r.set_interrupt(Some(Box::new(move || start.elapsed().as_millis() < 200)));
    let e = r.eval("for (;;) {}", "spin.js").unwrap_err();
    assert_eq!(e.termination(), Some(Termination::Interrupted));
    assert!(start.elapsed().as_millis() < 2000);
    let rep = r.report(&e);
    assert!(rep.uncatchable);
}

struct WindowClass;
impl HostClass for WindowClass {
    fn name(&self) -> &str {
        "Window"
    }
    fn hooks(&self) -> u8 {
        hooks::NAMED
    }
    fn get(
        &self,
        realm: &mut Realm,
        _obj: Value,
        _data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<Value>> {
        // Elements by id: only names that look like ids we know.
        match key {
            Key::Str("byId") => Ok(Some(realm.string("element"))),
            _ => Ok(None),
        }
    }
}

#[test]
fn weak_handles_global_host_and_locations() {
    let mut r = Realm::new(Options::default());
    let o = r.new_object();
    let w = r.weak(o);
    assert!(r.upgrade(&w).is_some());
    r.release_temps();
    r.gc();
    assert!(r.upgrade(&w).is_none());
    let class = r.register_class(WindowClass);
    r.make_global_host(class, 0);
    let v = r.eval("var x = 1; [typeof byId, byId, x, typeof Math, globalThis === this, 'byId' in globalThis]", "w.js").unwrap();
    assert_eq!(
        r.inspect(v),
        "['string', 'element', 1, 'object', true, true]"
    );
    let where_ = Rc::new(RefCell::new(String::new()));
    let w2 = where_.clone();
    let log = r.new_function("log", 1, move |r, _, _| {
        if let Some((f, l, c)) = r.current_location() {
            *w2.borrow_mut() = alloc::format!("{f}:{l}:{c}");
        }
        let st = r.stack_trace();
        assert!(st.contains("at caller (c.js:2:"), "{st}");
        Ok(Value::UNDEFINED)
    });
    let g = r.global();
    r.set(g, "log", log).unwrap();
    r.eval("function caller() {\n  log('hi')\n}\ncaller()", "c.js")
        .unwrap();
    assert_eq!(where_.borrow().as_str(), "c.js:2:3");
}

/// Bytes in and out of the engine: the embedding API a `fetch` needs.
#[test]
fn host_bytes_in_and_out() {
    let mut r = Realm::new(Options::default());
    // Host bytes into script, copied and moved.
    let body: Vec<u8> = (0..=255u8).collect();
    let u8a = r.new_uint8_array(&body).unwrap();
    assert!(r.is_typed_array(u8a));
    assert!(!r.is_array_buffer(u8a));
    let ab = r.new_array_buffer(b"hello").unwrap();
    assert!(r.is_array_buffer(ab));
    assert!(r.is_array_buffer_view(u8a));
    let moved = r.new_array_buffer_from_vec(alloc::vec![1, 2, 3]).unwrap();
    let g = r.global();
    r.set(g, "u8", u8a).unwrap();
    r.set(g, "ab", ab).unwrap();
    r.set(g, "moved", moved).unwrap();
    let v = r
        .eval(
            "var s = 0; for (var i = 0; i < u8.length; i++) s += u8[i];\
             [s, ab.byteLength, new Uint8Array(ab)[1], new Uint8Array(moved).join('-')].join()",
            "b.js",
        )
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("32640,5,101,1-2-3"));
    // Borrow, copy and mutate from the host.
    assert_eq!(r.buffer_bytes(ab), Some(&b"hello"[..]));
    assert_eq!(r.copy_bytes(u8a).map(|b| b.len()), Some(256));
    r.buffer_bytes_mut(ab).unwrap()[0] = b'j';
    let v = r
        .eval("String.fromCharCode(...new Uint8Array(ab))", "b.js")
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("jello"));
    // A view borrows only what it views.
    let v = r
        .eval("new Uint16Array(new ArrayBuffer(16), 4, 3)", "b.js")
        .unwrap();
    assert_eq!(r.buffer_bytes(v).map(|b| b.len()), Some(6));
    let v = r
        .eval("new DataView(new ArrayBuffer(10), 2, 5)", "b.js")
        .unwrap();
    assert_eq!(r.buffer_bytes(v).map(|b| b.len()), Some(5));
    // Not bytes.
    let o = r.new_object();
    assert!(r.buffer_bytes(o).is_none());
    assert!(r.buffer_bytes(Value::int(3)).is_none());
    // Taking the bytes detaches the buffer: script sees a zero-length buffer.
    let bytes = r.take_buffer_bytes(ab).unwrap();
    assert_eq!(bytes, b"jello");
    assert!(r.buffer_bytes(ab).is_none());
    let v = r.eval("ab.byteLength + ',' + ab.detached", "b.js").unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("0,true"));
    assert!(r.take_buffer_bytes(ab).is_none());
    // The cap.
    r.set_max_buffer_bytes(1000);
    assert!(r.new_array_buffer(&[0u8; 1001]).is_err());
    assert!(r.new_uint8_array_from_vec(alloc::vec![0u8; 1000]).is_ok());
    let v = r
        .eval(
            "try { new ArrayBuffer(1001); 'no' } catch (e) { e.name }",
            "b.js",
        )
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("RangeError"));
    let v = r
        .eval(
            "try { new Uint8Array(1001); 'no' } catch (e) { e.name }",
            "b.js",
        )
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("RangeError"));
    // Buffers count against the heap budget.
    let mut r = Realm::new(Options {
        limits: Limits {
            max_heap_bytes: 4 << 20,
            ..Limits::default()
        },
        ..Options::default()
    });
    r.set_max_buffer_bytes(64 << 20);
    let v = r
        .eval(
            "var keep = []; try { for (var i = 0; i < 100; i++) keep.push(new ArrayBuffer(1 << 20)); 'no' } catch (e) { e.name }",
            "b.js",
        );
    // Either a RangeError from one allocation or a termination: never a panic, never 100 MB.
    match v {
        Ok(v) => assert_eq!(r.get_string(v).as_deref(), Some("RangeError")),
        Err(e) => assert!(e.is_uncatchable()),
    }
}

/// `unhandledrejection` / `rejectionhandled` and `window.onerror` style hooks.
#[test]
fn rejection_and_error_hooks() {
    let mut r = Realm::new(Options::default());
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let l2 = log.clone();
    r.set_rejection_hook(Some(Box::new(
        move |r: &mut Realm, ev: PromiseRejection| {
            match ev {
                PromiseRejection::Unhandled { promise, reason } => {
                    let s = r.inspect(reason);
                    l2.borrow_mut()
                        .push(alloc::format!("unhandled {s} {}", promise.is_object()));
                    // Keep the promise to handle it later from script.
                    let g = r.global();
                    let _ = r.set(g, "lastRejected", promise);
                }
                PromiseRejection::Handled { promise } => {
                    let g = r.global();
                    let same = r.get(g, "lastRejected").map(|p| p.is_same_handle(&promise));
                    l2.borrow_mut()
                        .push(alloc::format!("handled {:?}", same.ok()));
                }
            }
        },
    )));
    // Rejected, never handled: reported once, after the checkpoint.
    r.eval("Promise.reject(new Error('a')); Promise.resolve(1).then(() => { Promise.reject('late'); })", "h.js")
        .unwrap();
    assert!(log.borrow().is_empty());
    r.run_jobs().unwrap();
    assert_eq!(log.borrow().len(), 2);
    assert!(log.borrow()[0].starts_with("unhandled Error: a"));
    assert!(log.borrow()[1].starts_with("unhandled late"));
    // Handled within the same checkpoint: never reported.
    log.borrow_mut().clear();
    r.eval(
        "var p = Promise.reject(7); Promise.resolve().then(() => p.catch(() => {}))",
        "h.js",
    )
    .unwrap();
    r.run_jobs().unwrap();
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    // Handled in a later checkpoint: reported as unhandled, then as handled.
    r.eval("var q = Promise.reject(8)", "h.js").unwrap();
    r.run_jobs().unwrap();
    assert_eq!(log.borrow().len(), 1);
    r.eval("lastRejected = q; q.catch(() => {})", "h.js")
        .unwrap();
    r.run_jobs().unwrap();
    assert_eq!(log.borrow().len(), 2);
    assert_eq!(log.borrow()[1], "handled Some(true)");
    // While a hook is installed the old polling API sees nothing.
    assert!(r.take_unhandled_rejections().is_empty());
    // A listener that rejects during the hook is reported in the same run_jobs.
    log.borrow_mut().clear();
    r.set_rejection_hook(None);
    r.eval("Promise.reject(1)", "h.js").unwrap();
    r.run_jobs().unwrap();
    assert_eq!(r.take_unhandled_rejections().len(), 1);

    // The error hook: a throwing job doesn't stop the others.
    let errs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let e2 = errs.clone();
    r.set_error_hook(Some(Box::new(move |r: &mut Realm, e: &Exception| {
        let rep = r.report(e);
        e2.borrow_mut()
            .push(alloc::format!("{}:{}:{}", rep.message, rep.file, rep.line));
    })));
    let f = r.eval("(function (n) { globalThis.ran = (globalThis.ran || 0) + 1; if (n % 2) throw new TypeError('odd ' + n); })", "j.js").unwrap();
    for n in 0..4 {
        r.enqueue_job(f, &[Value::int(n)]);
    }
    r.run_jobs().unwrap();
    let ran = r.eval("ran", "j.js").unwrap();
    assert_eq!(ran.as_number(), Some(4.0));
    assert_eq!(errs.borrow().as_slice(), ["odd 1:j.js:1", "odd 3:j.js:1"]);
    // Terminations still come back to the caller.
    let mut limits = r.limits();
    limits.max_steps = 100_000;
    r.set_limits(limits);
    let spin = r.eval("(function () { while (true) {} })", "j.js").unwrap();
    r.enqueue_job(spin, &[]);
    let e = r.run_jobs().unwrap_err();
    assert!(e.is_uncatchable());
    assert_eq!(errs.borrow().len(), 2);
}
