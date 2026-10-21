//! echo [-n] [-e] words...
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut words = &args.argv[1..];
    let (mut newline, mut escapes) = (true, false);
    while let Some(w) = words.first() {
        match *w {
            "-n" => newline = false,
            "-e" => escapes = true,
            "-ne" | "-en" => {
                newline = false;
                escapes = true
            }
            _ => break,
        }
        words = &words[1..];
    }
    let mut s = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        if escapes {
            let mut it = w.chars();
            while let Some(c) = it.next() {
                if c != '\\' {
                    s.push(c);
                    continue;
                }
                match it.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('e') => s.push('\x1b'),
                    Some('\\') => s.push('\\'),
                    Some('c') => {
                        newline = false;
                        break;
                    }
                    Some(o) => {
                        s.push('\\');
                        s.push(o)
                    }
                    None => s.push('\\'),
                }
            }
        } else {
            s.push_str(w);
        }
    }
    if newline {
        s.push('\n');
    }
    let _ = rt::sys::write_all(1, s.as_bytes());
    0
}
