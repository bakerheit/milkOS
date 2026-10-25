//! The system report: a section of the SD card log with the state of the machine, written when the boot has finished and
//! every five minutes after (rpi/logfile.rs calls [`emit`] from its thread). Cheap text dumps, all of them lines of the
//! log tagged `[report]`:
//!
//! * `/proc`: uptime, load, memory (and memory pressure), CPU times and load, the scheduler, the threads (thread and CPU
//!   load), the processes, the interrupts, the mounts and the disks' counters;
//! * the device list (`/dev`), the network interfaces with their counters, routes and neighbours, the hostname;
//! * the hardware found at boot (PCI, USB, Ethernet, SD card, board and CPU facts: the discovery lines the drivers print
//!   once, kept by `logbuf`);
//! * the SD card controller's counters of the log writer's own path, and where the log stands (`logfile::status`);
//! * the SoC temperature, the firmware's throttling and under-voltage flags and the clocks, asked from the VideoCore
//!   mailbox (each answer or its failure is a line: a board whose mailbox does not work says so).
//!
//! About 6 to 12 KiB a time, which is 70 to 140 KiB an hour of the ring.

use alloc::format;
use alloc::string::String;

use super::{logbuf, logfile, mailbox, sdhci};

/// Largest part of one /proc file kept.
const FILE_MAX: usize = 3500;

fn put(text: &str) {
    logbuf::exempt_lines("[report]", text);
}

fn file(path: &str, max_lines: usize) {
    match crate::fs::read_to_string(path) {
        Ok(t) => {
            let mut out = String::new();
            out.push_str(&format!("-- {path}\n"));
            let mut bytes = 0;
            for (i, l) in t.lines().enumerate() {
                if i >= max_lines || bytes > FILE_MAX {
                    out.push_str("   (cut)\n");
                    break;
                }
                bytes += l.len() + 1;
                out.push_str(l);
                out.push('\n');
            }
            put(&out);
        }
        Err(e) => put(&format!("-- {path}: {e:?}\n")),
    }
}

fn processes() {
    let mut out = String::from("-- processes (pid ppid state name)\n");
    let mut all = crate::proc::all();
    all.sort_by_key(|p| p.pid);
    for p in all.iter().take(60) {
        let stat = crate::fs::read_to_string(&format!("/proc/{}/stat", p.pid)).unwrap_or_default();
        out.push_str(stat.lines().next().unwrap_or("?"));
        out.push('\n');
    }
    if all.len() > 60 {
        out.push_str(&format!("   ({} more)\n", all.len() - 60));
    }
    put(&out);
}

fn devices() {
    let mut out = String::from("-- /dev\n");
    match crate::fs::readdir("/dev") {
        Ok(list) => {
            let names: alloc::vec::Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
            out.push_str(&names.join(" "));
            out.push('\n');
        }
        Err(e) => out.push_str(&format!("{e:?}\n")),
    }
    put(&out);
    let mut hw = String::from("-- hardware found at boot (PCI, USB, Ethernet, SD card, board, CPU)\n");
    logbuf::facts(|b| hw.push_str(&String::from_utf8_lossy(b)));
    put(&hw);
}

fn firmware() {
    use rpikit::mailbox::{clock, tag};
    let mut out = String::from("-- firmware (VideoCore mailbox)\n");
    let q = |t: u32, req: &[u32], words: usize| mailbox::query(t, req, words);
    match q(tag::GET_TEMPERATURE, &[0], 2) {
        Ok(v) => out.push_str(&format!("temperature: {}.{:03} C\n", v[1] / 1000, v[1] % 1000)),
        Err(e) => out.push_str(&format!("temperature: {e:?}\n")),
    }
    match q(tag::GET_THROTTLED, &[0], 1) {
        Ok(v) => {
            let f = v[0];
            out.push_str(&format!(
                "throttled flags {f:#x}: under-voltage {} (since boot {}), frequency capped {} (since boot {}), throttled {} (since boot {}), soft temp limit {} (since boot {})\n",
                f & 1,
                f >> 16 & 1,
                f >> 1 & 1,
                f >> 17 & 1,
                f >> 2 & 1,
                f >> 18 & 1,
                f >> 3 & 1,
                f >> 19 & 1
            ));
        }
        Err(e) => out.push_str(&format!("throttled flags: {e:?}\n")),
    }
    for (id, name) in [
        (clock::ARM, "arm"),
        (clock::CORE, "core"),
        (clock::EMMC2, "emmc2"),
        (clock::UART, "uart"),
    ] {
        match q(tag::GET_CLOCK_RATE, &[id], 2) {
            Ok(v) => out.push_str(&format!("clock {name}: {} Hz\n", v[1])),
            Err(e) => out.push_str(&format!("clock {name}: {e:?}\n")),
        }
    }
    put(&out);
}

fn card() {
    let (reads, writes, fails) = sdhci::raw_counters();
    put(&format!(
        "-- SD card controller, the log writer's own path: {reads} read commands, {write} write commands, {fails} failures (the block device's counters are in /proc/diskstats)\n",
        write = writes
    ));
}

/// Write report number `n` of boot `boot` into the log.
pub fn emit(n: u64, boot: u32) {
    let up = logbuf::uptime_ms();
    put(&format!(
        "==== system report {n}, boot {boot}, uptime {}.{:03} s ====\n",
        up / 1000,
        up % 1000
    ));
    file("/proc/uptime", 2);
    file("/proc/loadavg", 2);
    file("/proc/meminfo", 40);
    file("/proc/mempressure", 8);
    file("/proc/stat", 12);
    file("/proc/cpuload", 12);
    file("/proc/sched", 12);
    file("/proc/cpuinfo", 12);
    file("/proc/threads", 80);
    processes();
    file("/proc/interrupts", 40);
    file("/proc/mounts", 16);
    file("/proc/diskstats", 16);
    devices();
    file("/proc/net/dev", 12);
    file("/proc/net/route", 12);
    file("/proc/net/arp", 12);
    file("/proc/net/hostname", 2);
    card();
    // The mailbox can take a second to give up on a firmware that does not answer: save what is waiting first.
    logfile::tick();
    firmware();
    put(&format!("watchdog: {}\n", super::watchdog::status()));
    put(&logfile::status());
    put(&format!("==== end of system report {n} ====\n"));
}
