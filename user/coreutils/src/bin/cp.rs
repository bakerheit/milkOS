//! cp [-r] [--reflink[=always|auto]] SRC... DEST
//!
//! --reflink / --reflink=always: share the source's blocks copy-on-write (FICLONE; milkfs
//! only), fail if that isn't possible; --reflink=auto: fall back to copying.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use rt::io::flags;
use rt::sys;
rt::main!(main);

#[derive(Clone, Copy, PartialEq)]
enum Reflink {
    Never,
    Auto,
    Always,
}

fn copy_file(src: &str, dst: &str, reflink: Reflink) -> bool {
    let inf = match sys::open(src, sys::O_RDONLY | sys::O_CLOEXEC) {
        Ok(f) => f,
        Err(e) => {
            rt::eprintln!("cp: {src}: {e}");
            return false;
        }
    };
    let mode = sys::stat_fd(inf).map_or(0o644, |s| s.mode & 0o7777);
    let outf = match sys::open_mode(
        dst,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        mode,
    ) {
        Ok(f) => f,
        Err(e) => {
            rt::eprintln!("cp: {dst}: {e}");
            let _ = sys::close(inf);
            return false;
        }
    };
    if reflink != Reflink::Never {
        match sys::ficlone(inf, outf) {
            Ok(()) => {
                let _ = (sys::close(inf), sys::close(outf));
                return true;
            }
            Err(e) if reflink == Reflink::Always => {
                rt::eprintln!("cp: {dst}: can't reflink: {e}");
                let _ = (sys::close(inf), sys::close(outf));
                return false;
            }
            Err(_) => {} // auto: copy instead
        }
    }
    let mut buf = [0u8; 4096];
    let mut ok = true;
    loop {
        match sys::read(inf, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = sys::write_all(outf, &buf[..n]) {
                    rt::eprintln!("cp: {dst}: {e}");
                    ok = false;
                    break;
                }
            }
            Err(e) => {
                rt::eprintln!("cp: {src}: {e}");
                ok = false;
                break;
            }
        }
    }
    let _ = (sys::close(inf), sys::close(outf));
    ok
}
fn copy(src: &str, dst: &str, recursive: bool, reflink: Reflink) -> bool {
    match sys::stat_path(src, true) {
        Ok(st) if st.is_dir() => {
            if !recursive {
                rt::eprintln!("cp: {src}: is a directory (use -r)");
                return false;
            }
            let _ = sys::mkdir(dst, 0o755);
            let mut ok = true;
            for (n, _) in sys::read_dir(src).unwrap_or_default() {
                if n != "." && n != ".." {
                    ok &= copy(
                        &coreutils::join(src, &n),
                        &coreutils::join(dst, &n),
                        true,
                        reflink,
                    );
                }
            }
            ok
        }
        Ok(_) => copy_file(src, dst, reflink),
        Err(e) => {
            rt::eprintln!("cp: {src}: {e}");
            false
        }
    }
}
fn main(args: &rt::Args) -> i32 {
    // Long options first (the short-flag parser would read "--reflink" as letters).
    let mut reflink = Reflink::Never;
    let mut rest = alloc::vec::Vec::new();
    for &a in &args.argv[1..] {
        match a {
            "--reflink" | "--reflink=always" => reflink = Reflink::Always,
            "--reflink=auto" => reflink = Reflink::Auto,
            "--reflink=never" => reflink = Reflink::Never,
            _ => rest.push(a),
        }
    }
    let (f, a) = flags(&rest);
    let r = f.contains(&'r') || f.contains(&'R');
    if a.len() < 2 {
        rt::eprintln!("usage: cp [-r] [--reflink[=always|auto]] SRC... DEST");
        return 1;
    }
    let (srcs, dest) = a.split_at(a.len() - 1);
    let dest = dest[0];
    let into = sys::stat_path(dest, true).is_ok_and(|s| s.is_dir());
    let mut ok = true;
    for s in srcs {
        let t = if into {
            coreutils::join(dest, coreutils::basename(s))
        } else {
            String::from(dest)
        };
        ok &= copy(s, &t, r, reflink);
    }
    if ok { 0 } else { 1 }
}
