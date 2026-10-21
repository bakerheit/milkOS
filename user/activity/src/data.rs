//! Reading the system: one `Sampler::refresh` per tick turns /proc into the values the tabs draw.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rt::sys;
use sysview::audit::{self, Record};
use sysview::proc::{self, Proc};
use sysview::system::{self, IrqKind, MemInfo, Ring};

pub const HISTORY: usize = 60;

pub fn text(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Wall clock in nanoseconds (the RTC, local time).
pub fn wall_ns() -> u64 {
    sys::clock_ns(sys::CLOCK_REALTIME)
}

pub fn mono_ns() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC)
}

#[derive(Default)]
pub struct Sampler {
    pub procs: Vec<Proc>,
    /// Busy percent per core, newest last.
    pub cores: Vec<Ring<HISTORY>>,
    pub load: Option<[u32; 3]>,
    pub mem: MemInfo,
    pub disk_read: Ring<HISTORY>,
    pub disk_write: Ring<HISTORY>,
    /// Disk bytes per second right now: (read, write).
    pub disk_rate: (u64, u64),
    /// Interrupts per second: timer, network, input, sound.
    pub irq_rate: [u64; 4],
    /// The Milk volume: (total blocks, free blocks) of 4 KiB.
    pub milk_blocks: Option<(u64, u64)>,
    /// tmpfs (the root, in RAM) used, KB.
    pub ram_disk_kb: u64,
    pub audit: Vec<Record>,
    pub total_records: u64,
    /// uid -> account name, from /etc/passwd.
    pub users: Vec<(u32, String)>,
    prev_ns: u64,
    prev_disk: Option<(u64, u64)>,
    prev_irq: Option<[u64; 4]>,
    audit_seen: u64,
}

impl Sampler {
    pub fn refresh(&mut self) {
        let now = mono_ns();
        let dt = now.saturating_sub(self.prev_ns);
        let first = self.prev_ns == 0;
        // Processes.
        let mut procs: Vec<Proc> = Vec::new();
        for (n, _) in sys::read_dir("/proc").unwrap_or_default() {
            if n.parse::<u32>().is_err() {
                continue;
            }
            if let Some(p) = proc::parse_status(&text(&format!("/proc/{n}/status"))) {
                procs.push(p);
            }
        }
        if !first {
            proc::fill_cpu(&mut procs, &self.procs, dt);
        }
        self.procs = procs;
        // Cores.
        let load = system::parse_cpuload(&text("/proc/cpuload"));
        while self.cores.len() < load.len() {
            self.cores.push(Ring::default());
        }
        for (r, p) in self.cores.iter_mut().zip(load) {
            r.push(p);
        }
        self.load = system::parse_loadavg(&text("/proc/loadavg"));
        let mi = text("/proc/meminfo");
        self.mem = system::parse_meminfo(&mi);
        self.ram_disk_kb = mi
            .lines()
            .find_map(|l| l.strip_prefix("Shmem:"))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0);
        // Disk: the sum over every device, in 512-byte sectors.
        let (r, w) = system::parse_diskstats(&text("/proc/diskstats"))
            .iter()
            .fold((0u64, 0u64), |a, d| {
                (a.0 + d.sectors_read, a.1 + d.sectors_written)
            });
        if let Some((pr, pw)) = self.prev_disk {
            self.disk_rate = (
                system::per_sec(pr, r, dt) * 512,
                system::per_sec(pw, w, dt) * 512,
            );
        }
        self.prev_disk = Some((r, w));
        self.disk_read
            .push((self.disk_rate.0 / 1024).min(u32::MAX as u64) as u32);
        self.disk_write
            .push((self.disk_rate.1 / 1024).min(u32::MAX as u64) as u32);
        // Interrupts.
        let irqs = system::parse_interrupts(&text("/proc/interrupts"));
        let cur = [
            IrqKind::Timer,
            IrqKind::Network,
            IrqKind::Input,
            IrqKind::Sound,
        ]
        .map(|k| system::irq_total(&irqs, k));
        if let Some(p) = self.prev_irq {
            for i in 0..4 {
                self.irq_rate[i] = system::per_sec(p[i], cur[i], dt);
            }
        }
        self.prev_irq = Some(cur);
        // The Milk volume's free space.
        self.milk_blocks = text("/sys/fs/milkfs/info").lines().find_map(|l| {
            let rest = l.strip_prefix("blocks:")?;
            let mut it = rest.split_whitespace();
            let total: u64 = it.next()?.parse().ok()?;
            it.next()?;
            let free: u64 = it.next()?.parse().ok()?;
            Some((total, free))
        });
        // The audit log, re-read only when it has grown.
        let status = text("/proc/audit_status");
        let n: u64 = status
            .lines()
            .find_map(|l| l.strip_prefix("records:"))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0);
        if n != self.audit_seen || self.audit.is_empty() {
            self.audit_seen = n;
            self.total_records = n;
            self.audit = audit::parse_log(&text("/proc/audit"));
        }
        if self.users.is_empty() {
            self.users = text("/etc/passwd")
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split(':').collect();
                    Some((f.get(2)?.parse().ok()?, String::from(*f.first()?)))
                })
                .collect();
        }
        self.prev_ns = now;
    }

    pub fn user_name(&self, uid: u32) -> String {
        self.users
            .iter()
            .find(|(u, _)| *u == uid)
            .map_or_else(|| format!("uid {uid}"), |(_, n)| n.clone())
    }

    pub fn total_cpu_pct(&self) -> u32 {
        // 100% = one core, as the table says; the summary shows the machine-wide busy average.
        let last: Vec<u32> = self.cores.iter().map(|c| c.last()).collect();
        if last.is_empty() {
            0
        } else {
            last.iter().sum::<u32>() / last.len() as u32
        }
    }

    pub fn find(&self, pid: u32) -> Option<&Proc> {
        self.procs.iter().find(|p| p.pid == pid)
    }

    /// How many sandbox refusals this pid has in the log, and the newest one's sentence.
    pub fn refusals(&self, pid: u32) -> (usize, Option<String>) {
        let mine: Vec<&Record> = self
            .audit
            .iter()
            .filter(|r| r.pid == pid && r.event == "sandbox.deny")
            .collect();
        (mine.len(), mine.last().map(|r| audit::describe(r).sentence))
    }
}

/// Run a program and collect what it prints (for `audit verify`).
pub fn capture(path: &str, argv: &[&str]) -> String {
    let Ok((r, w)) = sys::pipe() else {
        return String::new();
    };
    match sys::fork() {
        Ok(0) => {
            let _ = sys::dup2(w, 1);
            let _ = sys::dup2(w, 2);
            let _ = sys::execve(path, argv, &["PATH=/bin"]);
            sys::exit(127);
        }
        Ok(pid) => {
            let _ = sys::close(w);
            let mut out = Vec::new();
            let mut buf = [0u8; 512];
            while let Ok(n) = sys::read(r, &mut buf) {
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            let _ = sys::close(r);
            let _ = sys::wait4(pid as i32, 0);
            String::from_utf8_lossy(&out).into_owned()
        }
        Err(_) => String::new(),
    }
}
