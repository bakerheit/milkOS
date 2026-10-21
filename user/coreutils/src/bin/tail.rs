//! tail [-n N | -N] [file...]
#![no_std]
#![no_main]
extern crate alloc;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use rt::io::{Lines, Out, each_input};
rt::main!(main);
fn count_arg(argv: &[&'static str]) -> (usize, Vec<&'static str>) {
    let mut n = 10;
    let mut files = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i];
        if a == "-n" {
            i += 1;
            n = argv.get(i).and_then(|v| v.parse().ok()).unwrap_or(10);
        } else if let Some(v) = a.strip_prefix("-n") {
            n = v.parse().unwrap_or(10);
        } else if a.len() > 1 && a.starts_with('-') && a[1..].parse::<usize>().is_ok() {
            n = a[1..].parse().unwrap();
        } else {
            files.push(a);
        }
        i += 1;
    }
    (n, files)
}
fn main(args: &rt::Args) -> i32 {
    let (n, files) = count_arg(&args.argv[1..]);
    let mut out = Out::stdout();
    let many = files.len() > 1;
    let ok = each_input("tail", &files, |fd, name| {
        if many {
            out.line(&alloc::format!("==> {name} <=="));
        }
        let mut keep: VecDeque<Vec<u8>> = VecDeque::new();
        let mut lines = Lines::new(fd);
        while let Some(l) = lines.next_bytes() {
            if keep.len() == n {
                keep.pop_front();
            }
            if n > 0 {
                keep.push_back(l);
            }
        }
        for l in keep {
            out.write(&l);
            out.write(b"\n");
        }
    });
    out.flush();
    if ok { 0 } else { 1 }
}
