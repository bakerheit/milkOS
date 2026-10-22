//! `system memory`: /proc/meminfo in the groups Activity Monitor's Memory tab shows
//! (sysview::system, docs/memory.md "Where the memory goes").

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use sysview::system::{MemInfo, MemPart, parse_meminfo};

use crate::fmt::kb;

/// One group: its label, kilobytes, and tenths of a percent of all memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub label: &'static str,
    pub kb: u64,
    pub pct10: u64,
}

/// The groups, in Activity Monitor's order, adding up to all memory. `apps_rss_kb` stands in
/// for the apps on a kernel without the breakdown.
pub fn groups(m: &MemInfo, apps_rss_kb: u64) -> Vec<Group> {
    let total = m.total_kb.max(1);
    m.parts(apps_rss_kb)
        .into_iter()
        .map(|(p, k)| Group {
            label: p.label(),
            kb: k,
            pct10: k * 1000 / total,
        })
        .collect()
}

fn pct(t: u64) -> String {
    format!("{}.{}%", t / 10, t % 10)
}

/// The whole report as lines: a headline, the groups, what "Other kernel" is made of.
pub fn report(meminfo: &str, apps_rss_kb: u64) -> Vec<String> {
    let m = parse_meminfo(meminfo);
    let mut out = Vec::new();
    let pressure = match m.pressure() {
        sysview::system::Pressure::Low => "low",
        sysview::system::Pressure::Medium => "medium",
        sysview::system::Pressure::High => "high",
    };
    out.push(format!(
        "{} of memory: {} in use, {} free (pressure {pressure})",
        kb(m.total_kb),
        kb(m.used_kb()),
        kb(m.free_kb)
    ));
    out.push(String::new());
    let g = groups(&m, apps_rss_kb);
    let w = g.iter().map(|x| x.label.len()).max().unwrap_or(0);
    for x in &g {
        out.push(format!(
            "  {:<w$}  {:>9}  {:>6}",
            x.label,
            kb(x.kb),
            pct(x.pct10)
        ));
    }
    if m.kernel.present {
        let parts: Vec<String> = m
            .other_kernel_detail()
            .into_iter()
            .filter(|p| p.1 > 0)
            .map(|(n, k)| format!("{n} {}", kb(k)))
            .collect();
        if !parts.is_empty() {
            out.push(String::new());
            out.push(format!(
                "{} is {}.",
                MemPart::OtherKernel.label(),
                parts.join(", ")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW: &str = "\
MemTotal:           524288 kB
MemFree:            300000 kB
MemReserved:         20000 kB
KernelHeap:          16384 kB
KernelHeapUsed:      12000 kB
KernelHeapFree:       4384 kB
HeapLargestFree:      2000 kB
Shmem:                1000 kB
KernelImage:         14312 kB
BuiltinFiles:        11671 kB
BootReserved:          512 kB
PageTables:           2048 kB
KernelStacks:         1024 kB
AppPages:            40000 kB
RootFiles:            3000 kB
PageCache:           50000 kB
Display:              6144 kB
Wallpaper:            6144 kB
WindowBuffers:        2192 kB
PanelCaches:          1000 kB
Devices:               800 kB
Other:                 100 kB
";

    #[test]
    fn activity_monitor_groups() {
        let m = parse_meminfo(NEW);
        let g = groups(&m, 0);
        let labels: Vec<&str> = g.iter().map(|x| x.label).collect();
        assert_eq!(
            labels,
            [
                "Apps",
                "Kernel heap",
                "Kernel image",
                "Files in RAM",
                "Screen",
                "Windows",
                "Other kernel",
                "Page cache",
                "Free"
            ]
        );
        assert_eq!(g.iter().map(|x| x.kb).sum::<u64>(), 524_288);
        assert_eq!(g[2].kb, 14_312 + 11_671);
        assert_eq!(g[4].kb, 6144 + 6144 + 1000);
        assert_eq!(g[8].pct10, 300_000 * 1000 / 524_288);
    }

    #[test]
    fn report_lines() {
        let r = report(NEW, 0);
        assert_eq!(
            r[0],
            "512 MB of memory: 170 MB in use, 293 MB free (pressure low)"
        );
        assert!(
            r.iter()
                .any(|l| l.starts_with("  Apps ") && l.contains("39 MB"))
        );
        assert!(
            r.iter()
                .any(|l| l.starts_with("  Page cache") && l.contains("49 MB"))
        );
        assert!(
            r.last()
                .unwrap()
                .starts_with("Other kernel is page tables 2.0 MB, kernel stacks 1.0 MB")
        );
        // An old kernel without the breakdown: apps from the processes, no detail line.
        let old =
            "MemTotal: 1024 kB\nMemFree: 512 kB\nKernelHeap: 256 kB\nKernelHeapUsed: 128 kB\n";
        let r = report(old, 100);
        assert!(
            r.iter()
                .any(|l| l.starts_with("  Apps") && l.contains("100 KB"))
        );
        assert!(!r.iter().any(|l| l.starts_with("Other kernel is")));
    }
}
