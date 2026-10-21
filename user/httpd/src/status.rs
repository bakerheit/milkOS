//! `/status`: a live dashboard from /proc, in the HTML subset the desktop Browser renders
//! (h1-h3, p, a, ul/li, pre, b, br, title: no tables, no CSS), so bars are text.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use rt::sys;

fn text(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// HTML-escape.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// %XX decoding of audit fields.
fn unescape(s: &str) -> String {
    if s == "-" {
        return String::new();
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        if b[k] == b'%'
            && k + 3 <= b.len()
            && let Some(v) = core::str::from_utf8(&b[k + 1..k + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            k += 3;
            continue;
        }
        out.push(b[k]);
        k += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn bar(pct: u64, width: usize) -> String {
    let filled = (pct.min(100) as usize * width).div_ceil(100).min(width);
    let mut s = String::from("[");
    for i in 0..width {
        s.push(if i < filled { '|' } else { ' ' });
    }
    s.push(']');
    s
}

fn hms(secs: u64) -> String {
    alloc::format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

pub fn page() -> String {
    let mut h = String::new();
    let up: u64 = text("/proc/uptime")
        .split('.')
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let load = text("/proc/loadavg");
    let la: Vec<&str> = load.split_whitespace().take(3).collect();
    let procs = sys::read_dir("/proc")
        .unwrap_or_default()
        .iter()
        .filter(|(n, _)| n.parse::<u32>().is_ok())
        .count();
    let version = text("/proc/version");
    let _ = write!(
        h,
        "<html><head><title>milkOS status</title></head><body>\n<h1>milkOS status</h1>\n\
         <p>Up <b>{}</b> &middot; load <b>{}</b> &middot; <b>{procs}</b> processes &middot; {}</p>\n",
        hms(up),
        if la.len() == 3 {
            la.join(" ")
        } else {
            String::from("n/a")
        },
        esc(version.trim())
    );
    // CPUs.
    h.push_str("<h2>CPU</h2>\n<pre>");
    for l in text("/proc/cpuload").lines() {
        let mut w = l.split_whitespace();
        if let (Some(name), Some(p)) = (w.next(), w.next().and_then(|x| x.parse::<u64>().ok())) {
            let _ = writeln!(h, "{name:<5} {} {p:>3}%", bar(p, 24));
        }
    }
    h.push_str("</pre>\n");
    // Memory.
    let mut total = 1u64;
    let mut free = 0u64;
    let mut heap = 0u64;
    for l in text("/proc/meminfo").lines() {
        let mut w = l.split_whitespace();
        let (Some(k), Some(v)) = (w.next(), w.next().and_then(|x| x.parse::<u64>().ok())) else {
            continue;
        };
        match k {
            "MemTotal:" => total = v.max(1),
            "MemFree:" => free = v,
            "KernelHeapUsed:" => heap = v,
            _ => {}
        }
    }
    let used = total.saturating_sub(free);
    let _ = write!(
        h,
        "<h2>Memory</h2>\n<pre>mem   {} {:>3}%\n{} / {} MiB used, kernel heap {} KiB</pre>\n",
        bar(used * 100 / total, 24),
        used * 100 / total,
        used / 1024,
        total / 1024,
        heap
    );
    // Network totals.
    h.push_str("<h2>Network</h2>\n<pre>");
    for l in text("/proc/net/dev").lines().skip(2) {
        let Some((name, rest)) = l.split_once(':') else {
            continue;
        };
        let f: Vec<u64> = rest
            .split_whitespace()
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        if f.len() >= 10 {
            let _ = writeln!(
                h,
                "{:<5} rx {:>9} B {:>6} pkts  tx {:>9} B {:>6} pkts",
                name.trim(),
                f[0],
                f[1],
                f[8],
                f[9]
            );
        }
    }
    h.push_str("</pre>\n");
    // Recent audit events (newest first).
    h.push_str("<h2>Recent audit events</h2>\n<ul>\n");
    let audit = text("/proc/audit");
    let lines: Vec<&str> = audit.lines().filter(|l| !l.is_empty()).collect();
    for l in lines.iter().rev().take(15) {
        let f: Vec<&str> = l.split(' ').collect();
        if f.len() < 8 {
            continue;
        }
        let secs = f[1].parse::<u64>().unwrap_or(0) / 1_000_000_000;
        let _ = writeln!(
            h,
            "<li>{} <b>{}</b> {} (pid {}) {}</li>",
            {
                let t = secs % 86_400;
                alloc::format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
            },
            esc(&unescape(f[4])),
            esc(&unescape(f[3])),
            f[2],
            esc(&unescape(f[5]))
        );
    }
    if lines.is_empty() {
        h.push_str("<li>no audit records</li>\n");
    }
    let _ = write!(
        h,
        "</ul>\n<p>{} records in RAM. <a href=\"/status\">Refresh</a> &middot; <a href=\"/\">Home</a></p>\n</body></html>\n",
        lines.len()
    );
    h
}
