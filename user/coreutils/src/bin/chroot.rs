//! chroot DIR [CMD [ARGS...]]   run CMD (default /Commands/msh) with DIR as /.
//! Used by `milk` to run a tried app as if its clone volume were the root.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::vec::Vec;
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let Some(dir) = args.get(1) else {
        rt::eprintln!("usage: chroot DIR [CMD [ARGS...]]");
        return 1;
    };
    if let Err(e) = sys::chroot(dir) {
        rt::eprintln!("chroot: {dir}: {e}");
        return 1;
    }
    let _ = sys::chdir("/");
    let argv: Vec<&str> = if args.argv.len() > 2 {
        args.argv[2..].to_vec()
    } else {
        alloc::vec!["/Commands/msh"]
    };
    let path = argv[0];
    let e = sys::execve(path, &argv, &args.envp);
    rt::eprintln!("chroot: {path}: {e}");
    127
}
