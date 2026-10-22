//! Says hello and pokes a few syscalls: argv/env, uname, clocks, brk heap, mmap.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::{println, sys};

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    println!(
        "hello, world! from pid {} (parent {})",
        sys::getpid(),
        sys::getppid()
    );
    println!("  argv = {:?}", args.argv);
    println!("  HOME = {}", args.env("HOME").unwrap_or("?"));
    if let Ok(u) = sys::uname() {
        println!("  uname: {} {} {} {}", u[0], u[1], u[2], u[4]);
    }
    let mono = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let real = sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000;
    println!(
        "  uptime {}.{:03} s, unix time {}",
        mono / 1_000_000_000,
        mono / 1_000_000 % 1000,
        real
    );

    // Heap via brk.
    let v: Vec<u64> = (0..100_000).collect();
    let sum: u64 = v.iter().sum();
    let h = rt::heap::stats();
    println!(
        "  heap: summed {} u64s = {sum}, brk heap at {:#x}..{:#x}, {} KiB free",
        v.len(),
        h.start,
        h.end,
        h.free / 1024
    );

    // Anonymous mmap.
    let len = 256 << 10;
    match sys::mmap_anon(len) {
        Ok(p) => {
            // SAFETY: fresh private mapping of `len` bytes.
            let s = unsafe { core::slice::from_raw_parts_mut(p, len) };
            s.iter_mut().enumerate().for_each(|(i, b)| *b = i as u8);
            let ok = s.iter().enumerate().all(|(i, &b)| b == i as u8);
            println!(
                "  mmap: 256 KiB at {:#x}, pattern {}",
                p as usize,
                if ok { "ok" } else { "BAD" }
            );
            let _ = sys::munmap(p, len);
        }
        Err(e) => println!("  mmap failed: {e:?}"),
    }
    0
}
