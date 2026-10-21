//! sleep SECONDS (fractions ok: 0.5)
#![no_std]
#![no_main]
rt::main!(main);
fn parse_secs(s: &str) -> Option<u64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut ns = if int.is_empty() {
        0
    } else {
        int.parse::<u64>().ok()?
    }
    .checked_mul(1_000_000_000)?;
    let mut scale = 100_000_000;
    for c in frac.chars().take(9) {
        ns += c.to_digit(10)? as u64 * scale;
        scale /= 10;
    }
    Some(ns)
}
fn main(args: &rt::Args) -> i32 {
    match args.get(1).and_then(parse_secs) {
        Some(ns) => match rt::sys::nanosleep(ns) {
            Ok(()) => 0,
            Err(_) => 130, // interrupted
        },
        None => {
            rt::eprintln!("usage: sleep SECONDS");
            1
        }
    }
}
