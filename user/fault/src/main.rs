//! Misbehaves on purpose, to show the kernel kills only the process.
//!   fault          null pointer read  (SIGSEGV)
//!   fault write    write to read-only code (SIGSEGV)
//!   fault stack N  recurse using ~N KiB of stack (grows on demand; past 8 MiB: SIGSEGV)
//!   fault ill      undefined instruction (SIGILL)

#![no_std]
#![no_main]

use rt::println;

rt::main!(main);

#[inline(never)]
fn recurse(depth: usize) -> usize {
    let buf = [depth as u8; 1024];
    if depth == 0 {
        return buf[0] as usize;
    }
    core::hint::black_box(&buf);
    recurse(depth - 1) + core::hint::black_box(buf[7]) as usize
}

fn main(args: &rt::Args) -> i32 {
    match args.get(1).unwrap_or("null") {
        "write" => {
            println!(
                "fault: writing to my own code at {:#x}...",
                main as *const () as usize
            );
            // SAFETY: deliberately not.
            unsafe { core::ptr::write_volatile(main as *const () as usize as *mut u32, 0) };
        }
        "stack" => {
            let kib: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1024);
            println!("fault: recursing through ~{kib} KiB of stack...");
            let r = recurse(kib);
            println!("fault: survived, stack grew on demand (result {r})");
            return 0;
        }
        "ill" => {
            println!("fault: executing an undefined instruction...");
            // SAFETY: deliberately not.
            unsafe { core::arch::asm!("udf #0") };
        }
        _ => {
            println!("fault: reading through a null pointer...");
            // SAFETY: deliberately not.
            let v = unsafe { core::ptr::read_volatile(core::ptr::null::<u64>()) };
            println!("fault: read {v}?!");
        }
    }
    println!("fault: still alive?!");
    1
}
