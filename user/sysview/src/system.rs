//! The machine-wide files: `/proc/meminfo`, `loadavg`, `cpuload`, `diskstats`, `interrupts`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kb: u64,
    pub free_kb: u64,
    pub heap_kb: u64,
    pub heap_used_kb: u64,
    /// The page cache, if the kernel reports one.
    pub cache_kb: u64,
}

pub fn parse_meminfo(text: &str) -> MemInfo {
    let mut m = MemInfo::default();
    for l in text.lines() {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let n: u64 = v
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        match k {
            "MemTotal" => m.total_kb = n,
            "MemFree" => m.free_kb = n,
            "KernelHeap" => m.heap_kb = n,
            "KernelHeapUsed" => m.heap_used_kb = n,
            "PageCache" | "Cached" => m.cache_kb = n,
            _ => {}
        }
    }
    m
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pressure {
    Low,
    Medium,
    High,
}

impl MemInfo {
    /// Memory in use that the system cannot just hand back: total - free - page cache.
    pub fn used_kb(&self) -> u64 {
        self.total_kb
            .saturating_sub(self.free_kb)
            .saturating_sub(self.cache_kb)
    }

    /// Low under 60% used, Medium up to 85%, High above.
    pub fn pressure(&self) -> Pressure {
        if self.total_kb == 0 {
            return Pressure::Low;
        }
        let pct = self.used_kb() * 100 / self.total_kb;
        match pct {
            0..60 => Pressure::Low,
            60..=85 => Pressure::Medium,
            _ => Pressure::High,
        }
    }
}

/// The three load averages in hundredths ("0.02 0.02 0.00 2/19 15").
pub fn parse_loadavg(text: &str) -> Option<[u32; 3]> {
    let mut it = text.split_whitespace();
    let mut one = || -> Option<u32> {
        let w = it.next()?;
        let (i, f) = w.split_once('.').unwrap_or((w, "0"));
        let f2: String = f.chars().chain("00".chars()).take(2).collect();
        Some(i.parse::<u32>().ok()? * 100 + f2.parse::<u32>().ok()?)
    };
    Some([one()?, one()?, one()?])
}

/// Per-core busy percent from `/proc/cpuload` ("cpu0 12").
pub fn parse_cpuload(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|l| {
            let (name, v) = l.split_once(' ')?;
            name.strip_prefix("cpu")?.parse::<u32>().ok()?;
            v.trim().parse::<u32>().ok().map(|p| p.min(100))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    pub name: String,
    pub sectors_read: u64,
    pub sectors_written: u64,
}

pub fn parse_diskstats(text: &str) -> Vec<Disk> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 {
                return None;
            }
            Some(Disk {
                name: f[2].to_string(),
                sectors_read: f[5].parse().ok()?,
                sectors_written: f[9].parse().ok()?,
            })
        })
        .collect()
}

/// How much a counter grew per second between two looks `dt_ns` apart.
pub fn per_sec(prev: u64, cur: u64, dt_ns: u64) -> u64 {
    if dt_ns == 0 {
        return 0;
    }
    (cur.saturating_sub(prev) as u128 * 1_000_000_000 / dt_ns as u128) as u64
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IrqKind {
    Timer,
    Network,
    Input,
    Sound,
    Disk,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Irq {
    pub label: String,
    /// Total over all CPUs.
    pub count: u64,
    pub kind: IrqKind,
}

pub fn parse_interrupts(text: &str) -> Vec<Irq> {
    let mut out = Vec::new();
    for l in text.lines() {
        let mut it = l.split_whitespace().peekable();
        let Some(first) = it.next() else { continue };
        if !first.ends_with(':') || first.starts_with("ERR") {
            continue;
        }
        let mut count = 0u64;
        while let Some(n) = it.peek().and_then(|w| w.parse::<u64>().ok()) {
            count += n;
            it.next();
        }
        let label: Vec<&str> = it.collect();
        let label = label.join(" ");
        let l = label.to_lowercase();
        let kind = if l.contains("timer") {
            IrqKind::Timer
        } else if l.contains("net") {
            IrqKind::Network
        } else if l.contains("input") {
            IrqKind::Input
        } else if l.contains("sound") {
            IrqKind::Sound
        } else if l.contains("blk") {
            IrqKind::Disk
        } else {
            IrqKind::Other
        };
        out.push(Irq { label, count, kind });
    }
    out
}

/// Sum the counts of one kind.
pub fn irq_total(v: &[Irq], kind: IrqKind) -> u64 {
    v.iter().filter(|i| i.kind == kind).map(|i| i.count).sum()
}

/// The last `N` values, oldest first, for the charts.
#[derive(Clone, Debug)]
pub struct Ring<const N: usize> {
    v: alloc::collections::VecDeque<u32>,
}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Ring {
            v: alloc::collections::VecDeque::new(),
        }
    }
}

impl<const N: usize> Ring<N> {
    pub fn push(&mut self, x: u32) {
        if self.v.len() == N {
            self.v.pop_front();
        }
        self.v.push_back(x);
    }

    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.v.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.v.len()
    }

    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    pub fn last(&self) -> u32 {
        self.v.back().copied().unwrap_or(0)
    }

    pub fn max(&self) -> u32 {
        self.v.iter().copied().max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_and_pressure() {
        let m = parse_meminfo(
            "MemTotal:           524288 kB\nMemFree:            462396 kB\nKernelHeap:          10240 kB\nKernelHeapUsed:       2748 kB\nShmem: 9076 kB\n",
        );
        assert_eq!(
            (m.total_kb, m.free_kb, m.heap_kb, m.heap_used_kb, m.cache_kb),
            (524_288, 462_396, 10_240, 2_748, 0)
        );
        assert_eq!(m.used_kb(), 61_892);
        assert_eq!(m.pressure(), Pressure::Low);
        let mut m2 = m;
        m2.free_kb = 524_288 * 30 / 100; // 70% used
        assert_eq!(m2.pressure(), Pressure::Medium);
        m2.free_kb = 524_288 * 10 / 100; // 90% used
        assert_eq!(m2.pressure(), Pressure::High);
        m2.cache_kb = 524_288 * 40 / 100; // the cache is not "used"
        assert_eq!(m2.pressure(), Pressure::Low);
        assert_eq!(MemInfo::default().pressure(), Pressure::Low);
    }

    #[test]
    fn load_and_cpu() {
        assert_eq!(parse_loadavg("0.02 0.12 1.50 2/19 15"), Some([2, 12, 150]));
        assert_eq!(parse_loadavg("3 4 5"), Some([300, 400, 500]));
        assert_eq!(parse_loadavg("x"), None);
        assert_eq!(
            parse_cpuload("cpu0 0\ncpu1 12\ncpu2 100\ncpu3 250\nload 9\n"),
            [0, 12, 100, 100]
        );
    }

    #[test]
    fn disks_and_rates() {
        let d = parse_diskstats(" 254       0 vda 21 0 168 0 188 0 4384 0 0 0 0\nshort line\n");
        assert_eq!(d.len(), 1);
        assert_eq!(
            (d[0].name.as_str(), d[0].sectors_read, d[0].sectors_written),
            ("vda", 168, 4384)
        );
        assert_eq!(per_sec(100, 300, 2_000_000_000), 100);
        assert_eq!(per_sec(300, 100, 1_000_000_000), 0);
        assert_eq!(per_sec(0, 5, 0), 0);
    }

    #[test]
    fn interrupts_by_kind() {
        let t = "            CPU0       CPU1\n   0:       1481       1649  GICv2 SGI IPI rescheduling\n  27:      21066      20798  GICv2 PPI arch_timer (virtual)\n  33:        114          0  GICv2 SPI uart-pl011\n  76:         17        378  GICv2 SPI virtio-blk (mmio slot 28)\n  77:          0          5  GICv2 SPI virtio-input (mmio slot 29)\n  78:          3          0  GICv2 SPI virtio-net (mmio slot 30)\n ERR:          0  spurious\n";
        let v = parse_interrupts(t);
        assert_eq!(v.len(), 6);
        assert_eq!(irq_total(&v, IrqKind::Timer), 41_864);
        assert_eq!(irq_total(&v, IrqKind::Disk), 395);
        assert_eq!(irq_total(&v, IrqKind::Input), 5);
        assert_eq!(irq_total(&v, IrqKind::Network), 3);
        assert_eq!(irq_total(&v, IrqKind::Sound), 0);
        assert_eq!(v[1].label, "GICv2 PPI arch_timer (virtual)");
    }

    #[test]
    fn ring_keeps_the_newest() {
        let mut r: Ring<3> = Ring::default();
        assert!(r.is_empty());
        for x in [1, 5, 2, 9] {
            r.push(x);
        }
        assert_eq!(r.iter().collect::<Vec<_>>(), [5, 2, 9]);
        assert_eq!((r.len(), r.last(), r.max()), (3, 9, 9));
    }
}
