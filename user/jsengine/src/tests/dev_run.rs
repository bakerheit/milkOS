//! A development helper, not a test: runs a script file with a `print` function and reports
//! what it threw. `JSENGINE_RUN=file cargo test -p jsengine --lib run_script -- --ignored
//! --nocapture`; with `JSENGINE_RUN_SPLIT=1` each line is its own script (a syntax error on
//! one line doesn't hide the others).
use crate::*;

#[test]
#[ignore]
fn run_script() {
    let path = std::env::var("JSENGINE_RUN").unwrap();
    let src = std::fs::read_to_string(&path).unwrap();
    let h = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || {
            let mut r = Realm::new(Options::default());
            r.set_stack_limit(6 << 20);
            let f = r.new_function("print", 1, |r, _t, a| {
                let mut parts = std::vec::Vec::new();
                for v in a {
                    parts.push(match v.kind() {
                        Kind::String => r.get_string(*v).unwrap(),
                        _ => r.inspect(*v),
                    });
                }
                std::println!("{}", parts.join(" "));
                Ok(Value::UNDEFINED)
            });
            let g = r.global();
            r.set(g, "print", f).unwrap();
            let split = std::env::var("JSENGINE_RUN_SPLIT").is_ok();
            if split {
                for line in src.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match r.eval(line, "run.js") {
                        Ok(_) => {}
                        Err(e) => std::println!(
                            "THROWN [{}]: {}",
                            &line[..line.len().min(40)],
                            r.report(&e).message
                        ),
                    }
                }
            } else {
                match r.eval(&src, "run.js") {
                    Ok(_) => {}
                    Err(e) => std::println!("THROWN: {}", r.report(&e)),
                }
            }
            if let Err(e) = r.run_jobs() {
                std::println!("JOB: {}", r.report(&e));
            }
            for v in r.take_unhandled_rejections() {
                std::println!("UNHANDLED: {}", r.report_value(v));
            }
        })
        .unwrap();
    h.join().unwrap();
}
