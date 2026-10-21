//! uptime: how long since boot, and how many processes.
#![no_std]
#![no_main]
extern crate alloc;
rt::main!(main);
fn main(_: &rt::Args) -> i32 {
    let up = rt::sys::read_file("/proc/uptime").unwrap_or_default();
    let secs: f64 = core::str::from_utf8(&up)
        .unwrap_or("0")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let s = secs as u64;
    let procs = rt::sys::read_dir("/proc").map_or(0, |d| {
        d.iter().filter(|e| e.0.parse::<u32>().is_ok()).count()
    });
    let now = rt::sys::clock_ns(rt::sys::CLOCK_REALTIME) / 1_000_000_000;
    let t = coreutils::fmt_time(now as i64);
    rt::println!(
        " {} up {}:{:02}:{:02}, {} processes",
        &t[7..],
        s / 3600,
        s / 60 % 60,
        s % 60,
        procs
    );
    0
}
