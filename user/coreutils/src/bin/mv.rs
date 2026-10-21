//! mv SRC... DEST   (DEST a directory: move into it)
#![no_std]
#![no_main]
extern crate alloc;
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    if a.len() < 2 {
        rt::eprintln!("usage: mv SRC... DEST");
        return 1;
    }
    let (srcs, dest) = a.split_at(a.len() - 1);
    let dest = dest[0];
    let into = sys::stat_path(dest, true).is_ok_and(|s| s.is_dir());
    if srcs.len() > 1 && !into {
        rt::eprintln!("mv: {dest}: not a directory");
        return 1;
    }
    let mut rc = 0;
    for s in srcs {
        let target = if into {
            coreutils::join(dest, coreutils::basename(s))
        } else {
            alloc::string::String::from(dest)
        };
        if let Err(e) = sys::rename(s, &target) {
            rt::eprintln!("mv: {s} -> {target}: {e}");
            rc = 1;
        }
    }
    rc
}
