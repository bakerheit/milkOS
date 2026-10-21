//! wc [-l] [-w] [-c] [file...]
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use core::fmt::Write;
use rt::io::{Out, each_input, flags};
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, files) = flags(&args.argv[1..]);
    let (mut l, mut w, mut c) = (f.contains(&'l'), f.contains(&'w'), f.contains(&'c'));
    if !l && !w && !c {
        (l, w, c) = (true, true, true);
    }
    let mut out = Out::stdout();
    let mut total = (0u64, 0u64, 0u64);
    let mut count = 0;
    let show = |out: &mut Out, t: (u64, u64, u64), name: &str| {
        let mut s = String::new();
        if l {
            let _ = write!(s, "{:>8}", t.0);
        }
        if w {
            let _ = write!(s, "{:>8}", t.1);
        }
        if c {
            let _ = write!(s, "{:>8}", t.2);
        }
        if name != "-" {
            let _ = write!(s, " {name}");
        }
        out.line(&s);
    };
    let ok = each_input("wc", &files, |fd, name| {
        let mut t = (0u64, 0u64, 0u64);
        let mut in_word = false;
        let mut buf = [0u8; 4096];
        while let Ok(n) = sys::read(fd, &mut buf) {
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                t.2 += 1;
                if b == b'\n' {
                    t.0 += 1;
                }
                let space = b.is_ascii_whitespace();
                if !space && !in_word {
                    t.1 += 1;
                }
                in_word = !space;
            }
        }
        show(&mut out, t, name);
        total = (total.0 + t.0, total.1 + t.1, total.2 + t.2);
        count += 1;
    });
    if count > 1 {
        show(&mut out, total, "total");
    }
    out.flush();
    if ok { 0 } else { 1 }
}
