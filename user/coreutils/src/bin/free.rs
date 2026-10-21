//! free [-h] [-m]: memory from /proc/meminfo.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use rt::io::flags;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, _) = flags(&args.argv[1..]);
    let m = rt::sys::read_file("/proc/meminfo").unwrap_or_default();
    let m = String::from_utf8_lossy(&m);
    let kb = |k: &str| -> u64 {
        m.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(':')))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    };
    let (total, free, shmem) = (kb("MemTotal"), kb("MemFree"), kb("Shmem"));
    let (heap_t, heap_u) = (kb("KernelHeap"), kb("KernelHeapUsed"));
    let used = total - free;
    let show = |v: u64| -> String {
        if f.contains(&'h') {
            coreutils::human(v * 1024)
        } else if f.contains(&'m') {
            alloc::format!("{}", v / 1024)
        } else {
            alloc::format!("{v}")
        }
    };
    rt::println!(
        "{:>8} {:>10} {:>10} {:>10} {:>10}",
        "",
        "total",
        "used",
        "free",
        "shared"
    );
    rt::println!(
        "{:>8} {:>10} {:>10} {:>10} {:>10}",
        "Mem:",
        show(total),
        show(used),
        show(free),
        show(shmem)
    );
    rt::println!(
        "{:>8} {:>10} {:>10} {:>10}",
        "KHeap:",
        show(heap_t),
        show(heap_u),
        show(heap_t - heap_u)
    );
    0
}
