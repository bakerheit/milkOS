//! touch FILE...   (create, or set the times to now)
#![no_std]
#![no_main]
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut rc = 0;
    for f in &args.argv[1..] {
        match sys::open_mode(f, sys::O_WRONLY | sys::O_CREAT | sys::O_CLOEXEC, 0o644) {
            Ok(fd) => {
                let _ = sys::close(fd);
                let _ = sys::touch_now(f);
            }
            Err(e) => {
                rt::eprintln!("touch: {f}: {e}");
                rc = 1;
            }
        }
    }
    rc
}
