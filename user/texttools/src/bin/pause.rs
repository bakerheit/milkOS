//! pause DURATION   (it replaced sleep): pause 5, pause 5s, pause 2m, pause 500ms, pause 0.5
//!
//! Exits 0 when the time is up, 130 if a signal cut it short. Lengths of time:
//! user/textkit/src/duration.rs.
#![no_std]
#![no_main]
extern crate alloc;

use textkit::help::PAUSE;

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let ns = match &args.argv[1..] {
        ["help" | "--help" | "-h"] => {
            let _ = rt::sys::write_all(1, PAUSE.as_bytes());
            return 0;
        }
        [d] => match textkit::duration::parse(d) {
            Ok(ns) => ns,
            Err(m) => {
                rt::eprintln!("pause: {m}");
                return 2;
            }
        },
        [] => {
            rt::eprintln!("pause: how long? pause 5, pause 5s, pause 2m or pause 500ms");
            return 2;
        }
        _ => {
            rt::eprintln!("pause: one length of time, like pause 5s (pause help)");
            return 2;
        }
    };
    match rt::sys::nanosleep(ns) {
        Ok(()) => 0,
        Err(_) => 130, // interrupted
    }
}
