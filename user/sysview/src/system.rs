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
    /// Where the rest goes, if the kernel says (docs/memory.md "Where the memory goes").
    pub kernel: KernelMem,
}

/// The kernel's own breakdown of the memory in use (the `/proc/meminfo` lines after
/// `Shmem`). All zero on a kernel that doesn't report them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelMem {
    /// The kernel image without the built-in files.
    pub image_kb: u64,
    /// The built-in files (initramfs archive), inside the kernel image.
    pub builtin_files_kb: u64,
    pub boot_kb: u64,
    pub page_tables_kb: u64,
    pub stacks_kb: u64,
    /// Every program's private pages, each counted once.
    pub app_pages_kb: u64,
    /// File data in RAM (the root file system, /tmp).
    pub root_files_kb: u64,
    pub display_kb: u64,
    pub wallpaper_kb: u64,
    pub windows_kb: u64,
    pub panels_kb: u64,
    pub devices_kb: u64,
    /// Not owned up to by anything above.
    pub other_kb: u64,
    /// The kernel sent the breakdown.
    pub present: bool,
}

/// One slice of the Memory tab's bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemPart {
    Apps,
    KernelHeap,
    KernelImage,
    Files,
    Screen,
    Windows,
    OtherKernel,
    PageCache,
    Free,
}

impl MemPart {
    pub fn label(self) -> &'static str {
        match self {
            MemPart::Apps => "Apps",
            MemPart::KernelHeap => "Kernel heap",
            MemPart::KernelImage => "Kernel image",
            MemPart::Files => "Files in RAM",
            MemPart::Screen => "Screen",
            MemPart::Windows => "Windows",
            MemPart::OtherKernel => "Other kernel",
            MemPart::PageCache => "Page cache",
            MemPart::Free => "Free",
        }
    }
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
            _ => {
                let k2 = &mut m.kernel;
                let slot = match k {
                    "KernelImage" => &mut k2.image_kb,
                    "BuiltinFiles" => &mut k2.builtin_files_kb,
                    "BootReserved" => &mut k2.boot_kb,
                    "PageTables" => &mut k2.page_tables_kb,
                    "KernelStacks" => &mut k2.stacks_kb,
                    "AppPages" => &mut k2.app_pages_kb,
                    "RootFiles" => &mut k2.root_files_kb,
                    "Display" => &mut k2.display_kb,
                    "Wallpaper" => &mut k2.wallpaper_kb,
                    "WindowBuffers" => &mut k2.windows_kb,
                    "PanelCaches" => &mut k2.panels_kb,
                    "Devices" => &mut k2.devices_kb,
                    "Other" => &mut k2.other_kb,
                    _ => continue,
                };
                *slot = n;
                k2.present = true;
            }
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

    /// The Memory tab's bar, in order, adding up to `total_kb`. `apps_rss_kb` (the
    /// processes' VmRSS summed) stands in for the apps on a kernel without the breakdown,
    /// where everything not otherwise known is "Other kernel" and the heap counts only
    /// what is in use.
    pub fn parts(&self, apps_rss_kb: u64) -> Vec<(MemPart, u64)> {
        let k = &self.kernel;
        let mut v = if k.present {
            alloc::vec![
                (MemPart::Apps, k.app_pages_kb),
                (MemPart::KernelHeap, self.heap_kb),
                (MemPart::KernelImage, k.image_kb + k.builtin_files_kb),
                (MemPart::Files, k.root_files_kb),
                (MemPart::Screen, k.display_kb + k.wallpaper_kb + k.panels_kb),
                (MemPart::Windows, k.windows_kb),
            ]
        } else {
            alloc::vec![
                (MemPart::Apps, apps_rss_kb),
                (MemPart::KernelHeap, self.heap_used_kb),
            ]
        };
        v.push((MemPart::PageCache, self.cache_kb));
        let named: u64 = v.iter().map(|&(_, kb)| kb).sum();
        let other = self.total_kb.saturating_sub(named + self.free_kb);
        v.insert(v.len() - 1, (MemPart::OtherKernel, other));
        v.push((MemPart::Free, self.free_kb));
        v
    }

    /// "Other kernel" in words, biggest first: page tables, stacks, devices, boot, the rest.
    pub fn other_kernel_detail(&self) -> Vec<(&'static str, u64)> {
        let k = &self.kernel;
        let mut v = alloc::vec![
            ("kernel stacks", k.stacks_kb),
            ("page tables", k.page_tables_kb),
            ("devices", k.devices_kb),
            ("boot", k.boot_kb),
            ("unaccounted", k.other_kb),
        ];
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
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
        // An old kernel: no breakdown, the apps from VmRSS, the rest is "Other kernel".
        assert!(!m.kernel.present);
        let p = m.parts(1_000);
        let kb = |part| p.iter().find(|(q, _)| *q == part).map(|&(_, k)| k);
        assert_eq!(kb(MemPart::Apps), Some(1_000));
        assert_eq!(kb(MemPart::KernelHeap), Some(2_748));
        assert_eq!(kb(MemPart::OtherKernel), Some(61_892 - 1_000 - 2_748));
        assert_eq!(kb(MemPart::Screen), None);
        assert_eq!(p.iter().map(|&(_, k)| k).sum::<u64>(), 524_288);
    }

    #[test]
    fn meminfo_breakdown() {
        let text = "MemTotal:           524288 kB
MemFree:            448884 kB
MemReserved:         26640 kB
KernelHeap:          12288 kB
KernelHeapUsed:       3154 kB
KernelHeapFree:       9133 kB
HeapLargestFree:      6144 kB
Shmem:               12096 kB
KernelImage:         14312 kB
BuiltinFiles:        11671 kB
BootReserved:         1680 kB
PageTables:            264 kB
KernelStacks:         1472 kB
AppPages:             1644 kB
RootFiles:           12096 kB
PageCache:            1796 kB
Display:              6144 kB
Wallpaper:            9216 kB
WindowBuffers:        2192 kB
PanelCaches:           272 kB
Devices:               356 kB
Other:                   0 kB
";
        let m = parse_meminfo(text);
        assert!(m.kernel.present);
        assert_eq!(m.cache_kb, 1_796);
        assert_eq!(m.kernel.app_pages_kb, 1_644);
        assert_eq!(m.kernel.windows_kb, 2_192);
        let p = m.parts(99_999); // VmRSS is ignored when the kernel counts app pages
        let kb = |part| p.iter().find(|(q, _)| *q == part).map(|&(_, k)| k).unwrap();
        assert_eq!(kb(MemPart::Apps), 1_644);
        assert_eq!(kb(MemPart::KernelHeap), 12_288);
        assert_eq!(kb(MemPart::KernelImage), 14_312 + 11_671);
        assert_eq!(kb(MemPart::Files), 12_096);
        assert_eq!(kb(MemPart::Screen), 6_144 + 9_216 + 272);
        assert_eq!(kb(MemPart::Windows), 2_192);
        assert_eq!(kb(MemPart::Free), 448_884);
        // The lines add up: the small ones (stacks, page tables, devices, boot) are the rest.
        // (plus 1 kB: each line is rounded down on its own)
        assert_eq!(kb(MemPart::OtherKernel), 1_680 + 264 + 1_472 + 356 + 1);
        assert_eq!(p.iter().map(|&(_, k)| k).sum::<u64>(), 524_288);
        assert_eq!(p.last().map(|&(q, _)| q), Some(MemPart::Free));
        let d = m.other_kernel_detail();
        assert_eq!(d[0], ("boot", 1_680));
        assert_eq!(d[1], ("kernel stacks", 1_472));
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
