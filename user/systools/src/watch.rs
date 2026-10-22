//! `processes watch`: a full-screen system monitor (raw tty + ANSI escapes): per-CPU bars,
//! memory, network and disk rates, processes sorted by CPU.
//!   processes watch     interactive: q quit, c sort by CPU, m sort by memory, p by pid,
//!                       + / - refresh faster / slower
//!   processes watch --plain [--frames N] [--every SECS]   N plain-text frames (default 1),
//!                       no escapes, no raw mode (for logs and tests)
//! Reads /proc/cpuload, /proc/stat, /proc/meminfo, /proc/loadavg, /proc/net/dev,
//! /proc/diskstats and /proc/<pid>/{stat,status,cmdline}. Works on the UART console and in
//! the desktop Terminal (both are ttys; the size comes from TIOCGWINSZ).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use rt::net::{POLLIN, PollFd};
use rt::{print, sys};

const TCGETS: usize = 0x5401;
const TCSETS: usize = 0x5402;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ICRNL: u32 = 0o400;
const HZ: u64 = 100;

fn ioctl(cmd: usize, buf: &mut [u8]) -> isize {
    // SAFETY: buf is valid for the termios struct the ioctl reads or writes.
    unsafe { sys::syscall(29, [0, cmd, buf.as_mut_ptr() as usize, 0, 0, 0]) }
}

fn raw_mode() -> Option<[u8; 60]> {
    let mut t = [0u8; 60];
    if ioctl(TCGETS, &mut t) < 0 {
        return None;
    }
    let old = t;
    let w = |t: &[u8], k: usize| u32::from_le_bytes([t[k], t[k + 1], t[k + 2], t[k + 3]]);
    let (i, l) = (w(&t, 0), w(&t, 12));
    t[0..4].copy_from_slice(&(i & !ICRNL).to_le_bytes());
    t[12..16].copy_from_slice(&(l & !(ICANON | ECHO | ISIG)).to_le_bytes());
    (ioctl(TCSETS, &mut t) >= 0).then_some(old)
}

fn text(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

#[derive(Clone)]
struct Proc {
    pid: u32,
    ppid: u32,
    state: char,
    ticks: u64,
    rss_kb: u64,
    cpu: String,
    name: String,
    cmd: String,
}

fn procs() -> Vec<Proc> {
    let mut v = Vec::new();
    for (n, _) in sys::read_dir("/proc").unwrap_or_default() {
        let Ok(pid) = n.parse::<u32>() else { continue };
        let st = text(&alloc::format!("/proc/{pid}/stat"));
        // pid (comm) state ppid ... utime(14) ... rss(24)
        let (Some(l), Some(r)) = (st.find('('), st.rfind(')')) else {
            continue;
        };
        let name = String::from(&st[l + 1..r]);
        let f: Vec<&str> = st[r + 1..].split_whitespace().collect();
        if f.len() < 22 {
            continue;
        }
        let num = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        let status = text(&alloc::format!("/proc/{pid}/status"));
        let cpu = status
            .lines()
            .find_map(|l| l.strip_prefix("Cpu:"))
            .map(|c| String::from(c.trim()))
            .unwrap_or_else(|| String::from("-"));
        let cmd: String = text(&alloc::format!("/proc/{pid}/cmdline"))
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        v.push(Proc {
            pid,
            ppid: num(1) as u32,
            state: f[0].chars().next().unwrap_or('?'),
            ticks: num(11),
            rss_kb: num(21) * 4,
            cpu,
            name,
            cmd,
        });
    }
    v
}

/// (rx bytes, tx bytes) of every interface but lo.
fn net() -> (u64, u64) {
    let mut t = (0, 0);
    for l in text("/proc/net/dev").lines().skip(2) {
        let Some((name, rest)) = l.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let f: Vec<u64> = rest
            .split_whitespace()
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        if f.len() >= 9 {
            t.0 += f[0];
            t.1 += f[8];
        }
    }
    t
}

/// (sectors read, sectors written) over all disks.
fn disk() -> (u64, u64) {
    let mut t = (0, 0);
    for l in text("/proc/diskstats").lines() {
        let f: Vec<u64> = l
            .split_whitespace()
            .enumerate()
            .filter(|(i, _)| *i != 2)
            .map(|(_, x)| x.parse().unwrap_or(0))
            .collect();
        if f.len() >= 9 {
            t.0 += f[4];
            t.1 += f[8];
        }
    }
    t
}

fn meminfo() -> BTreeMap<String, u64> {
    text("/proc/meminfo")
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((String::from(k), v.split_whitespace().next()?.parse().ok()?))
        })
        .collect()
}

fn bar(out: &mut String, frac: u64, width: usize, ansi: bool) {
    let filled = (frac.min(100) as usize * width).div_ceil(100).min(width);
    if ansi {
        let colour = if frac >= 85 {
            "31"
        } else if frac >= 50 {
            "33"
        } else {
            "32"
        };
        let _ = write!(out, "\x1b[{colour}m");
    }
    for i in 0..width {
        out.push(if i < filled { '|' } else { ' ' });
    }
    if ansi {
        out.push_str("\x1b[0m");
    }
}

fn rate(bytes: u64, secs_x100: u64) -> String {
    let per_s = bytes * 100 / secs_x100.max(1);
    if per_s >= 1 << 20 {
        alloc::format!("{}.{} MiB/s", per_s >> 20, ((per_s & 0xfffff) * 10) >> 20)
    } else if per_s >= 1024 {
        alloc::format!("{}.{} KiB/s", per_s >> 10, ((per_s & 0x3ff) * 10) >> 10)
    } else {
        alloc::format!("{per_s} B/s")
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Cpu,
    Mem,
    Pid,
}

struct Sample {
    at_ns: u64,
    ticks: BTreeMap<u32, u64>,
    net: (u64, u64),
    disk: (u64, u64),
}

fn sample(ps: &[Proc]) -> Sample {
    Sample {
        at_ns: sys::clock_ns(sys::CLOCK_MONOTONIC),
        ticks: ps.iter().map(|p| (p.pid, p.ticks)).collect(),
        net: net(),
        disk: disk(),
    }
}

/// Clip `line` to `cols` visible characters (ANSI escape sequences don't count).
fn fit(line: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut vis = 0;
    let mut esc = false;
    for c in line.chars() {
        if esc {
            out.push(c);
            if c.is_ascii_alphabetic() {
                esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            esc = true;
            out.push(c);
            continue;
        }
        if vis >= cols {
            continue;
        }
        out.push(c);
        vis += 1;
    }
    out
}

/// One frame. `ansi`: escapes (home, clear to end of line, colours); else plain text.
/// Every line is clipped to `cols`, and no more than `rows` lines are drawn (no newline
/// after the last one), so nothing wraps or scrolls even in a small Terminal window.
fn frame(
    prev: &Sample,
    ps: &mut [Proc],
    sort: Sort,
    rows: usize,
    cols: usize,
    ansi: bool,
) -> (String, Sample) {
    let now = sample(ps);
    let dt_x100 = ((now.at_ns - prev.at_ns) / 10_000_000).max(1); // in 1/100 s
    let mut lines: Vec<String> = Vec::new();
    // Header: time, uptime, load.
    let up: u64 = text("/proc/uptime")
        .split('.')
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let wall = sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000;
    let load = text("/proc/loadavg");
    let la: Vec<&str> = load.split_whitespace().take(3).collect();
    lines.push(alloc::format!(
        "processes watch - {:02}:{:02}:{:02} up {}:{:02}:{:02}, {} processes, load {}",
        wall / 3600 % 24,
        wall / 60 % 60,
        wall % 60,
        up / 3600,
        up / 60 % 60,
        up % 60,
        ps.len(),
        if la.len() == 3 {
            la.join(" ")
        } else {
            String::from("n/a")
        }
    ));
    // CPUs and memory: bars sized to the terminal ("cpu0 [....] 100%").
    let width = cols.saturating_sub(13).clamp(8, 50);
    for l in text("/proc/cpuload").lines() {
        let mut w = l.split_whitespace();
        let (Some(name), Some(pct)) = (w.next(), w.next().and_then(|p| p.parse::<u64>().ok()))
        else {
            continue;
        };
        let mut b = alloc::format!("{name:<5}[");
        bar(&mut b, pct, width, ansi);
        let _ = write!(b, "] {pct:>3}%");
        lines.push(b);
    }
    let m = meminfo();
    let total = m.get("MemTotal").copied().unwrap_or(1).max(1);
    let used = total - m.get("MemFree").copied().unwrap_or(0).min(total);
    let mut b = String::from("Mem  [");
    // "] 44/512M" is 3 characters longer than "] 100%".
    bar(
        &mut b,
        used * 100 / total,
        width.saturating_sub(3).max(5),
        ansi,
    );
    let _ = write!(b, "] {}/{}M", used / 1024, total / 1024);
    lines.push(b);
    lines.push(alloc::format!(
        "Net  rx {} tx {}",
        rate(now.net.0.saturating_sub(prev.net.0), dt_x100),
        rate(now.net.1.saturating_sub(prev.net.1), dt_x100),
    ));
    lines.push(alloc::format!(
        "Disk rd {} wr {}",
        rate(now.disk.0.saturating_sub(prev.disk.0) * 512, dt_x100),
        rate(now.disk.1.saturating_sub(prev.disk.1) * 512, dt_x100),
    ));
    // Processes. Tenths of a percent of one CPU: ticks and dt are both in 1/100 s.
    let pct = |p: &Proc| -> u64 {
        let before = prev.ticks.get(&p.pid).copied().unwrap_or(p.ticks);
        p.ticks.saturating_sub(before) * 1000 / dt_x100
    };
    let mut rows_v: Vec<(u64, &Proc)> = ps.iter().map(|p| (pct(p), p)).collect();
    match sort {
        Sort::Cpu => rows_v.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.ticks.cmp(&a.1.ticks))),
        Sort::Mem => rows_v.sort_by_key(|r| core::cmp::Reverse(r.1.rss_kb)),
        Sort::Pid => rows_v.sort_by_key(|r| r.1.pid),
    }
    let head = alloc::format!(
        "{:>5} {:>5} S CPU  %CPU     TIME  RSS KiB COMMAND{:pad$}",
        "PID",
        "PPID",
        "",
        pad = cols
    );
    lines.push(if ansi {
        alloc::format!("\x1b[7m{}\x1b[0m", fit(&head, cols))
    } else {
        String::from(head.trim_end())
    });
    for (c, p) in &rows_v {
        let t = p.ticks;
        lines.push(alloc::format!(
            "{:>5} {:>5} {} {:>3} {:>4}.{} {:>3}:{:02}.{:02} {:>8} {}",
            p.pid,
            p.ppid,
            p.state,
            p.cpu,
            c / 10,
            c % 10,
            t / HZ / 60,
            t / HZ % 60,
            t % HZ,
            p.rss_kb,
            if p.cmd.is_empty() { &p.name } else { &p.cmd }
        ));
    }
    let mut out = String::new();
    if ansi {
        out.push_str("\x1b[H");
        let n = lines.len().min(rows.max(1));
        for (i, l) in lines.iter().take(n).enumerate() {
            out.push_str(&fit(l, cols.saturating_sub(1)));
            out.push_str("\x1b[K");
            if i + 1 < n {
                out.push_str("\r\n");
            }
        }
        out.push_str("\x1b[J");
    } else {
        for l in &lines {
            out.push_str(l);
            out.push('\n');
        }
    }
    (out, now)
}

/// Run the view: `plain` prints `frames` plain-text frames `every_secs` apart; otherwise the
/// interactive full-screen view until q.
pub fn run(plain: bool, frames: u64, every_secs: u64) -> i32 {
    let mut delay_ms = every_secs.clamp(1, 60) * 1000;
    let (rows, cols) =
        sys::winsize(1).map_or((24, 80), |(r, c)| (r.max(10) as usize, c.max(40) as usize));
    let mut ps = procs();
    let mut prev = sample(&ps);
    if plain {
        let n = frames.max(1);
        for i in 0..n {
            sys::sleep_ms(if i == 0 { 500 } else { delay_ms });
            ps = procs();
            let (out, now) = frame(
                &prev,
                &mut ps,
                Sort::Cpu,
                rows.max(40),
                cols.max(100),
                false,
            );
            print!("{out}");
            if i + 1 < n {
                print!("\n");
            }
            prev = now;
        }
        return 0;
    }
    let Some(old) = raw_mode() else {
        rt::eprintln!(
            "processes watch: this isn't a terminal (processes watch --plain prints plain text)"
        );
        return 1;
    };
    print!("\x1b[?25l\x1b[2J");
    let mut sort = Sort::Cpu;
    let mut first = true;
    'outer: loop {
        let wait = if first { 300 } else { delay_ms };
        first = false;
        let deadline = sys::clock_ns(sys::CLOCK_MONOTONIC) + wait * 1_000_000;
        loop {
            let now = sys::clock_ns(sys::CLOCK_MONOTONIC);
            if now >= deadline {
                break;
            }
            let mut fds = [PollFd {
                fd: 0,
                events: POLLIN,
                revents: 0,
            }];
            if rt::net::poll(&mut fds, Some((deadline - now) / 1_000_000 + 1)).unwrap_or(0) == 0 {
                continue;
            }
            let mut b = [0u8; 8];
            let n = sys::read(0, &mut b).unwrap_or(0);
            for &c in &b[..n] {
                match c {
                    b'q' | 3 => break 'outer,
                    b'c' => sort = Sort::Cpu,
                    b'm' => sort = Sort::Mem,
                    b'p' => sort = Sort::Pid,
                    b'+' => delay_ms = (delay_ms / 2).max(250),
                    b'-' => delay_ms = (delay_ms * 2).min(10_000),
                    _ => {}
                }
            }
            break; // redraw now (sort changed, or a key)
        }
        let (rows, cols) = sys::winsize(1).map_or((rows, cols), |(r, c)| (r as usize, c as usize));
        ps = procs();
        let (out, now) = frame(&prev, &mut ps, sort, rows, cols, true);
        print!("{out}");
        prev = now;
    }
    let mut t = old;
    ioctl(TCSETS, &mut t);
    print!("\x1b[0m\x1b[?25h\x1b[2J\x1b[H");
    0
}
