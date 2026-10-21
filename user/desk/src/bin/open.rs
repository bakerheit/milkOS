//! open <app|path>...: open apps (files, terminal, notes, about, paint, today, spaces),
//! folders (in Files), text files (in Notes) or /bin programs on the desktop.

#![no_std]
#![no_main]

use rt::println;

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    if args.argv.len() < 2 {
        println!("usage: open <app|path>...   apps: files terminal notes about paint today spaces");
        return 1;
    }
    let mut code = 0;
    for a in &args.argv[1..] {
        code |= desk::wm(&alloc::format!("open\t{}", desk::absolute(a)));
    }
    code
}

extern crate alloc;
