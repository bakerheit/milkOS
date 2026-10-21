//! sort [-r] [-n] [-u] [-k FIELD] [file...]   (fields are whitespace separated, from 1)
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::io::{Lines, Out, each_input, flags};
rt::main!(main);
fn num(s: &str) -> f64 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && c == '-')))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse().unwrap_or(0.0)
}
fn key(l: &str, k: usize) -> &str {
    if k == 0 {
        return l;
    }
    // From field k to the end of the line, like `sort -k k`.
    let mut it = l.char_indices().peekable();
    let mut field = 0;
    let mut in_ws = true;
    while let Some(&(i, c)) = it.peek() {
        if !c.is_whitespace() && in_ws {
            field += 1;
            if field == k {
                return &l[i..];
            }
        }
        in_ws = c.is_whitespace();
        it.next();
    }
    ""
}
fn main(args: &rt::Args) -> i32 {
    let mut argv: Vec<&'static str> = args.argv[1..].to_vec();
    let mut k = 0;
    if let Some(i) = argv.iter().position(|a| *a == "-k") {
        k = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0);
        argv.drain(i..(i + 2).min(argv.len()));
    }
    let (f, files) = flags(&argv);
    let mut lines: Vec<String> = Vec::new();
    let ok = each_input("sort", &files, |fd, _| lines.extend(Lines::new(fd)));
    if f.contains(&'n') {
        lines.sort_by(|a, b| {
            num(key(a, k))
                .partial_cmp(&num(key(b, k)))
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
    } else {
        lines.sort_by(|a, b| key(a, k).cmp(key(b, k)).then(a.cmp(b)));
    }
    if f.contains(&'r') {
        lines.reverse();
    }
    if f.contains(&'u') {
        lines.dedup();
    }
    let mut out = Out::stdout();
    for l in &lines {
        out.line(l);
    }
    out.flush();
    if ok { 0 } else { 2 }
}
