//! Per-CPU data. TPIDR_EL1 holds the CPU's index (0 = the boot CPU, set in boot.S;
//! secondaries get theirs in smp.S). Everything here is per CPU and lock free.

#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const MAX_CPUS: usize = 8;

pub struct PerCpu {
    pub online: AtomicBool,
    /// MPIDR_EL1 affinity (what PSCI CPU_ON and the DTB call it).
    pub mpidr: AtomicU64,
    /// The scheduler wants this CPU to pick again (set locally or by another CPU + IPI).
    pub need_resched: AtomicBool,
    /// Thread running here, and this CPU's idle thread.
    pub current: AtomicU32,
    pub idle: AtomicU32,
    /// Time spent in the idle thread / anything else, ns (for /proc/stat and CPU %).
    pub idle_ns: AtomicU64,
    pub busy_ns: AtomicU64,
    /// Timer ticks taken on this CPU.
    pub ticks: AtomicU64,
    /// Reschedule IPIs received.
    pub ipis: AtomicU64,
}

impl PerCpu {
    const fn new() -> Self {
        Self {
            online: AtomicBool::new(false),
            mpidr: AtomicU64::new(0),
            need_resched: AtomicBool::new(false),
            current: AtomicU32::new(0),
            idle: AtomicU32::new(0),
            idle_ns: AtomicU64::new(0),
            busy_ns: AtomicU64::new(0),
            ticks: AtomicU64::new(0),
            ipis: AtomicU64::new(0),
        }
    }
}

static CPUS: [PerCpu; MAX_CPUS] = [const { PerCpu::new() }; MAX_CPUS];

/// Index of the CPU we are running on.
#[inline]
pub fn id() -> usize {
    let v: u64;
    // SAFETY: register read.
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el1", out(reg) v, options(nomem, nostack, preserves_flags))
    };
    v as usize
}

#[inline]
pub fn this() -> &'static PerCpu {
    &CPUS[id()]
}

pub fn get(cpu: usize) -> &'static PerCpu {
    &CPUS[cpu]
}

/// Mark the boot CPU online. Call early (before secondaries).
pub fn init_boot() {
    let mpidr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack)) };
    CPUS[0]
        .mpidr
        .store(mpidr & 0xff_00ff_ffff, Ordering::Relaxed);
    CPUS[0].online.store(true, Ordering::Release);
}

/// CPUs that are up.
pub fn online() -> impl Iterator<Item = usize> {
    (0..MAX_CPUS).filter(|&c| CPUS[c].online.load(Ordering::Acquire))
}

pub fn count() -> usize {
    online().count()
}
