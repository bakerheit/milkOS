//! rm [-r] [-f] PATH...
#![no_std]
#![no_main]
extern crate alloc;
use rt::io::flags;
use rt::sys;
rt::main!(main);
fn remove(p: &str, recursive: bool, force: bool) -> bool {
    let st = match sys::stat_path(p, false) {
        Ok(s) => s,
        Err(e) => {
            if !force {
                rt::eprintln!("rm: {p}: {e}");
            }
            return force;
        }
    };
    if st.is_dir() {
        if !recursive {
            rt::eprintln!("rm: {p}: is a directory (use -r)");
            return false;
        }
        let mut ok = true;
        for (name, _) in sys::read_dir(p).unwrap_or_default() {
            if name != "." && name != ".." {
                ok &= remove(&coreutils::join(p, &name), recursive, force);
            }
        }
        if let Err(e) = sys::rmdir(p) {
            rt::eprintln!("rm: {p}: {e}");
            return false;
        }
        return ok;
    }
    match sys::unlink(p) {
        Ok(()) => true,
        Err(e) => {
            rt::eprintln!("rm: {p}: {e}");
            false
        }
    }
}
fn main(args: &rt::Args) -> i32 {
    let (f, paths) = flags(&args.argv[1..]);
    let (r, force) = (f.contains(&'r') || f.contains(&'R'), f.contains(&'f'));
    if paths.is_empty() && !force {
        rt::eprintln!("usage: rm [-rf] PATH...");
        return 1;
    }
    let mut ok = true;
    for p in paths {
        ok &= remove(p, r, force);
    }
    if ok { 0 } else { 1 }
}
