//! notify <title> [subtitle...]: show a toast on the desktop.

#![no_std]
#![no_main]

extern crate alloc;

use rt::println;

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let Some(title) = args.get(1) else {
        println!("usage: notify <title> [subtitle...]");
        return 1;
    };
    let sub = args.argv[2..].join(" ");
    desk::wm(&alloc::format!("notify\t{title}\t{sub}"))
}
