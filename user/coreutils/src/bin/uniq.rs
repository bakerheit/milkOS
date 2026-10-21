//! uniq [-c] [-d] [file]
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use core::fmt::Write;
use rt::io::{Lines, Out, each_input, flags};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, files) = flags(&args.argv[1..]);
    let (count, dups) = (f.contains(&'c'), f.contains(&'d'));
    let mut out = Out::stdout();
    let mut prev: Option<(String, usize)> = None;
    let emit = |out: &mut Out, p: &(String, usize)| {
        if dups && p.1 < 2 {
            return;
        }
        if count {
            let _ = writeln!(out, "{:>7} {}", p.1, p.0);
        } else {
            out.line(&p.0);
        }
    };
    let ok = each_input("uniq", &files[..files.len().min(1)], |fd, _| {
        for l in Lines::new(fd) {
            match &mut prev {
                Some((p, n)) if *p == l => *n += 1,
                _ => {
                    if let Some(p) = prev.take() {
                        emit(&mut out, &p);
                    }
                    prev = Some((l, 1));
                }
            }
        }
    });
    if let Some(p) = prev.take() {
        emit(&mut out, &p);
    }
    out.flush();
    if ok { 0 } else { 1 }
}
