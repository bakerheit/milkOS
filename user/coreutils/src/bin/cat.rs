//! cat [-n] [file...]  ("-" or nothing: stdin)
#![no_std]
#![no_main]
extern crate alloc;
use core::fmt::Write;
use rt::io::{Lines, Out, each_input, flags};
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, files) = flags(&args.argv[1..]);
    let number = f.contains(&'n');
    let mut out = Out::stdout();
    let mut n = 0;
    let ok = each_input("cat", &files, |fd, _| {
        if number {
            for l in Lines::new(fd) {
                n += 1;
                let _ = writeln!(out, "{n:>6}\t{l}");
            }
            return;
        }
        let mut buf = [0u8; 4096];
        loop {
            match sys::read(fd, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(k) => {
                    out.flush();
                    if sys::write_all(1, &buf[..k]).is_err() {
                        break;
                    }
                }
            }
        }
    });
    out.flush();
    if ok { 0 } else { 1 }
}
