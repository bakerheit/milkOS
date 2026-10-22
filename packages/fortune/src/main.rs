//! fortune: a milk-themed quote.
//!   fortune            a random one
//!   fortune -a         all of them
//!   fortune -n N       quote number N
//! Reads quotes.txt next to the program (so it also works when run from a try-before-install
//! clone), else /Apps/fortune/quotes.txt, else the built-in copy.
//! Quotes are separated by lines holding a single `%`, like the classic fortune files.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, println, sys};

rt::main!(main);

const BUILTIN: &str = include_str!("../quotes.txt");

fn random() -> u64 {
    let mut b = [0u8; 8];
    // SAFETY: getrandom(buf, len, 0) writes at most 8 bytes into `b`.
    let r = unsafe { sys::syscall(278, [b.as_mut_ptr() as usize, 8, 0, 0, 0, 0]) };
    if r == 8 {
        u64::from_le_bytes(b)
    } else {
        sys::clock_ns(sys::CLOCK_MONOTONIC)
    }
}

fn quotes_text(argv0: &str) -> String {
    let mut tries = Vec::new();
    if let Some((dir, _)) = argv0.rsplit_once('/')
        && !dir.is_empty()
        && !dir.ends_with("/Commands")
    {
        tries.push(alloc::format!("{dir}/quotes.txt"));
    }
    tries.push(String::from("/Apps/fortune/quotes.txt"));
    for p in tries {
        if let Ok(b) = sys::read_file(&p) {
            return String::from_utf8_lossy(&b).into_owned();
        }
    }
    String::from(BUILTIN)
}

fn main(args: &rt::Args) -> i32 {
    let text = quotes_text(args.get(0).unwrap_or(""));
    let quotes: Vec<&str> = text
        .split("\n%\n")
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .collect();
    if quotes.is_empty() {
        eprintln!("fortune: no quotes");
        return 1;
    }
    match (args.get(1), args.get(2)) {
        (Some("-a"), _) => {
            for (i, q) in quotes.iter().enumerate() {
                println!("{:>2}. {q}", i + 1);
            }
        }
        (Some("-n"), Some(n)) => match n.parse::<usize>() {
            Ok(n) if (1..=quotes.len()).contains(&n) => println!("{}", quotes[n - 1]),
            _ => {
                eprintln!("fortune: -n 1..{}", quotes.len());
                return 2;
            }
        },
        (None, _) => {
            let q = quotes[(random() % quotes.len() as u64) as usize];
            println!("{q}");
            println!("    -- milkOS fortune ({} quotes)", quotes.len());
        }
        _ => {
            eprintln!("usage: fortune [-a | -n N]");
            return 2;
        }
    }
    0
}
