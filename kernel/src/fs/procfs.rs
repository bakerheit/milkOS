//! procfs at `/proc`: read-only text files generated on every read.
//!
//! `meminfo`, `uptime`, `version`, `interrupts`, `mounts`. Per-process directories
//! (`/proc/<pid>`, `/proc/self`) belong to the process subsystem: it can add them with
//! [`root`] + `SynthDir::add_dir` / `add`, or replace `self` with a custom inode.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::fmt::Write;

use super::errno::Result;
use super::synth::{SynthDir, SynthFs};
use super::{path, tmpfs};
use crate::sync::SpinLock;
use crate::{gic, heap, mmu, pmm, timer};

static PROCFS: SpinLock<Option<Arc<SynthFs>>> = SpinLock::new(None);

/// Root directory of procfs, for other subsystems to hang entries on.
pub fn root() -> Option<Arc<SynthDir>> {
    PROCFS.lock().as_ref().map(|f| f.root_dir().clone())
}

pub const VERSION: &str = concat!(
    "milkOS ",
    env!("CARGO_PKG_VERSION"),
    " (aarch64, Rust no_std kernel) #1\n"
);

fn meminfo(root_fs: &tmpfs::TmpFs) -> String {
    let p = pmm::stats();
    let h = heap::stats();
    let kb = |bytes: usize| bytes / 1024;
    let mut s = String::new();
    let mut line = |k: &str, v: usize| {
        let _ = writeln!(s, "{:<16}{:>10} kB", format!("{k}:"), v);
    };
    line("MemTotal", kb(p.total_frames * mmu::PAGE_SIZE));
    line("MemFree", kb(p.free_frames * mmu::PAGE_SIZE));
    line("MemReserved", kb(p.reserved_frames * mmu::PAGE_SIZE));
    line("KernelHeap", kb(h.total));
    line("KernelHeapUsed", kb(h.used));
    line("KernelHeapFree", kb(h.free));
    line("HeapLargestFree", kb(h.largest_free));
    line("Shmem", kb(root_fs.used_bytes() as usize)); // tmpfs data, as on Linux
    s
}

fn uptime() -> String {
    let ns = timer::now_ns();
    // Second field is idle time; no idle accounting yet.
    format!(
        "{}.{:02} 0.00\n",
        ns / 1_000_000_000,
        ns % 1_000_000_000 / 10_000_000
    )
}

fn irq_name(intid: u32) -> String {
    use crate::virtio::mmio::{MMIO_SLOTS, MMIO_SPI_BASE, Transport, device_id};
    let virtio0 = gic::spi(MMIO_SPI_BASE);
    match intid {
        0 => String::from("IPI rescheduling"),
        27 => String::from("arch_timer (virtual)"),
        33 => String::from("uart-pl011"),
        i if (virtio0..virtio0 + MMIO_SLOTS as u32).contains(&i) => {
            let slot = (i - virtio0) as usize;
            let what = match Transport::probe(slot).map(|t| t.device_id()) {
                Ok(device_id::BLOCK) => "virtio-blk",
                Ok(device_id::INPUT) => "virtio-input",
                Ok(_) => "virtio",
                Err(_) => "virtio (empty)",
            };
            format!("{what} (mmio slot {slot})")
        }
        _ => String::from("-"),
    }
}

fn interrupts() -> String {
    let cpus: alloc::vec::Vec<usize> = crate::percpu::online().collect();
    let mut s = String::from("     ");
    for c in &cpus {
        let _ = write!(s, "{:>11}", format!("CPU{c}"));
    }
    s.push('\n');
    for irq in 0..gic::num_irqs() {
        let n = gic::irq_count(irq);
        if n == 0 && !gic::is_registered(irq) {
            continue;
        }
        let kind = match irq {
            0..16 => "SGI",
            16..32 => "PPI",
            _ => "SPI",
        };
        let _ = write!(s, "{irq:>4}:");
        for &c in &cpus {
            let _ = write!(s, " {:>10}", gic::irq_count_cpu(irq, c));
        }
        let _ = writeln!(s, "  GICv2 {kind} {}", irq_name(irq));
    }
    let _ = writeln!(s, " ERR: {:>10}  spurious", gic::spurious_count());
    s
}

fn mounts() -> String {
    let mut s = String::new();
    for m in path::mounts() {
        let _ = writeln!(s, "{0} {1} {0} rw 0 0", m.fs_name, m.path);
    }
    s
}

/// Build procfs and mount it on `/proc`. `root_fs` is the root tmpfs (for Shmem).
pub fn init(root_fs: Arc<tmpfs::TmpFs>) -> Result<()> {
    let fs = SynthFs::new("proc");
    let r = fs.root_dir();
    r.add_file("meminfo", move || meminfo(&root_fs));
    r.add_file("uptime", uptime);
    r.add_file("version", || String::from(VERSION));
    r.add_file("interrupts", interrupts);
    r.add_file("mounts", mounts);
    super::mount("/proc", fs.clone())?;
    *PROCFS.lock() = Some(fs);
    Ok(())
}
