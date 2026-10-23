//! The embedding API over the stub: objects, host functions and classes, hooks, pins, errors.

use crate::*;
use alloc::string::String;
use alloc::vec::Vec;

#[test]
fn objects_and_functions() {
    let mut r = Realm::new(Options::default());
    let o = r.new_object();
    let s = r.string("hello");
    r.set(o, "greeting", s).unwrap();
    let back = r.get(o, "greeting").unwrap();
    assert_eq!(r.get_string(back).as_deref(), Some("hello"));
    let f = r.new_function("add", 2, |r, _this, args| {
        let a = r.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))?;
        let b = r.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
        Ok(Value::number(a + b))
    });
    let v = r
        .call(f, Value::UNDEFINED, &[Value::int(2), Value::number(0.5)])
        .unwrap();
    assert_eq!(v.as_number(), Some(2.5));
    assert_eq!(r.type_of(f), "function");
    let a = r.new_array(&[Value::int(1), s]);
    assert!(r.is_array(a));
    assert_eq!(r.get(a, "length").unwrap().as_number(), Some(2.0));
    assert_eq!(r.inspect(a), "[1, 'hello']");
    let keys = r.own_key_strings(o).unwrap();
    assert_eq!(keys, alloc::vec![String::from("greeting")]);
}

struct List;
impl HostClass for List {
    fn name(&self) -> &str {
        "List"
    }
    fn hooks(&self) -> u8 {
        hooks::INDEXED
    }
    fn get(&self, _r: &mut Realm, _obj: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        Ok(match key {
            Key::Index(i) if (i as u64) < data => Some(Value::int(i as i32 * 10)),
            _ => None,
        })
    }
    fn keys(&self, _r: &mut Realm, _obj: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        Ok((0..data as u32).map(OwnedKey::Index).collect())
    }
}

#[test]
fn host_objects() {
    let mut r = Realm::new(Options::default());
    let class = r.register_class(List);
    let proto = r.new_object();
    let ctor = r.new_class("List", 0, proto, None, None);
    let l = r.new_host_object(class, 3, proto);
    assert_eq!(r.get(l, 1u32).unwrap().as_number(), Some(10.0));
    assert!(r.get(l, 5u32).unwrap().is_undefined());
    assert_eq!(r.host_data(l, class), Some(3));
    assert!(r.instance_of(l, ctor).unwrap());
    let e = r.construct(ctor, &[]).unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.name, "TypeError");
    assert_eq!(rep.message, "Illegal constructor");
    let ks = r.own_key_strings(l).unwrap();
    assert_eq!(ks.len(), 3);
}

#[test]
fn pins_and_gc() {
    let mut r = Realm::new(Options::default());
    let o = r.new_object();
    let p = r.pin(o);
    r.release_temps();
    r.gc();
    let o2 = r.pinned(&p);
    assert!(o2.is_object());
    let lost = r.new_object();
    r.release_temps();
    r.gc();
    assert!(r.get(lost, "x").is_err());
    // A stale handle reads as undefined.
    assert!(r.type_of(lost) == "undefined");
    r.unpin(p);
}

#[test]
fn errors() {
    let mut r = Realm::new(Options::default());
    let f = r.new_function("boom", 0, |r, _, _| {
        Err(r.throw_error(ErrorKind::RangeError, "too big"))
    });
    let e = r.call(f, Value::UNDEFINED, &[]).unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.name, "RangeError");
    assert_eq!(rep.message, "too big");
    let v = e.value().unwrap();
    assert!(r.is_error(v));
}
