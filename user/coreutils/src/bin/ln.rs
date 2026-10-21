//! ln [-s] TARGET LINK
#![no_std]
#![no_main]
use rt::io::flags;
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, a) = flags(&args.argv[1..]);
    if a.len() != 2 {
        rt::eprintln!("usage: ln [-s] TARGET LINK");
        return 1;
    }
    let r = if f.contains(&'s') {
        sys::symlink(a[0], a[1])
    } else {
        sys::link(a[0], a[1])
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            rt::eprintln!("ln: {}: {e}", a[1]);
            1
        }
    }
}
