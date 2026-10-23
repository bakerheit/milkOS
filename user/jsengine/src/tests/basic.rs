//! First end-to-end checks: scripts in, values out.

use crate::*;
use alloc::string::String;

pub(crate) fn run(src: &str) -> String {
    let mut r = Realm::new(Options::default());
    match r.eval(src, "test.js") {
        Ok(v) => {
            if let Err(e) = r.run_jobs() {
                return alloc::format!("job error: {}", r.report(&e));
            }
            r.inspect(v)
        }
        Err(e) => alloc::format!("error: {}", r.report(&e)),
    }
}

#[test]
fn smoke() {
    assert_eq!(run("1 + 2"), "3");
    assert_eq!(run("var x = 10; x * 2"), "20");
    assert_eq!(run("let s = 'a'; s + 'b'"), "ab");
    assert_eq!(run("function f(a, b) { return a + b } f(2, 3)"), "5");
    assert_eq!(run("[1,2,3].map(x => x * 2).join(',')"), "2,4,6");
}
