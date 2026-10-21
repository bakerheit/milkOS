//! uname [-a] [-s] [-n] [-r] [-v] [-m]
#![no_std]
#![no_main]
extern crate alloc;
use alloc::vec::Vec;
use rt::io::flags;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, _) = flags(&args.argv[1..]);
    let Ok(u) = rt::sys::uname() else { return 1 };
    let all = f.contains(&'a');
    let mut parts: Vec<&str> = Vec::new();
    for (c, i) in [('s', 0), ('n', 1), ('r', 2), ('v', 3), ('m', 4)] {
        if all || f.contains(&c) {
            parts.push(&u[i]);
        }
    }
    if parts.is_empty() {
        parts.push(&u[0]);
    }
    rt::println!("{}", parts.join(" "));
    0
}
