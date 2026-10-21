//! grep [-i] [-v] [-n] [-c] [-l] PATTERN [file...]   (plain substring match)
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use core::fmt::Write;
use rt::io::{Lines, Out, each_input, flags};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, rest) = flags(&args.argv[1..]);
    let Some((&pat, files)) = rest.split_first() else {
        rt::eprintln!("usage: grep [-ivncl] PATTERN [FILE...]");
        return 2;
    };
    let (icase, invert, number, count, list) = (
        f.contains(&'i'),
        f.contains(&'v'),
        f.contains(&'n'),
        f.contains(&'c'),
        f.contains(&'l'),
    );
    let needle = if icase {
        pat.to_lowercase()
    } else {
        String::from(pat)
    };
    let mut out = Out::stdout();
    let many = files.len() > 1;
    let mut any = false;
    let ok = each_input("grep", files, |fd, name| {
        let mut hits = 0;
        for (i, line) in Lines::new(fd).enumerate() {
            let hay = if icase {
                line.to_lowercase()
            } else {
                line.clone()
            };
            if hay.contains(needle.as_str()) == invert {
                continue;
            }
            hits += 1;
            any = true;
            if count || list {
                continue;
            }
            let mut s = String::new();
            if many {
                let _ = write!(s, "{name}:");
            }
            if number {
                let _ = write!(s, "{}:", i + 1);
            }
            s.push_str(&line);
            out.line(&s);
        }
        if list && hits > 0 {
            out.line(name);
        } else if count {
            if many {
                let _ = writeln!(out, "{name}:{hits}");
            } else {
                let _ = writeln!(out, "{hits}");
            }
        }
    });
    out.flush();
    if !ok {
        2
    } else if any {
        0
    } else {
        1
    }
}
