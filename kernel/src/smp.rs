//! Secondary CPU bring-up via PSCI CPU_ON.
//!
//! The CPU list and the PSCI conduit (`hvc` or `smc`) come from the device tree
//! (`/cpus/cpu@N/reg` = MPIDR, `/psci/method`). Each secondary:
//! 1. starts at `secondary_entry` (smp.S) at EL1 with the MMU off, x0 = its `BootBlock`;
//! 2. loads the boot CPU's MAIR/TCR/SCTLR, the boot identity tables (TTBR0) and the
//!    kernel's tables (TTBR1) from that block (cleaned to the point of coherency by the boot
//!    CPU, since the secondary reads it uncached), turns the MMU and caches on, branches up
//!    to the higher half, drops the identity map (empty TTBR0), then sets sp (its own
//!    64 KiB kernel stack, with guard) and TPIDR_EL1;
//! 3. runs `secondary_main`: vectors, FP, its GIC CPU interface and banked interrupts, its
//!    virtual timer, then joins the scheduler as that CPU's idle thread.

use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::percpu::{self, MAX_CPUS};
use crate::sched::kstack::KStack;
use crate::{gic, kprintln, mem, mmu, sched, timer};

/// What a secondary needs before it can run Rust. Layout shared with smp.S.
#[repr(C, align(64))]
pub struct BootBlock {
    stack_top: u64,
    mair: u64,
    tcr: u64,
    ttbr0: u64,
    sctlr: u64,
    cpu: u64,
    ttbr1: u64,
    ttbr0_final: u64,
}

static mut BOOT: [BootBlock; MAX_CPUS] = [const {
    BootBlock {
        stack_top: 0,
        mair: 0,
        tcr: 0,
        ttbr0: 0,
        sctlr: 0,
        cpu: 0,
        ttbr1: 0,
        ttbr0_final: 0,
    }
}; MAX_CPUS];

/// Boot stacks handed to secondaries (they become those CPUs' idle threads' stacks).
static mut STACKS: [Option<KStack>; MAX_CPUS] = [const { None }; MAX_CPUS];

global_asm!(include_str!("el.S"), include_str!("smp.S"), MAIN = sym secondary_main);

unsafe extern "C" {
    fn secondary_entry();
    fn secondary_spin_entry();
}

const PSCI_CPU_ON: u64 = 0xc400_0003;
const PSCI_VERSION: u64 = 0x8400_0000;

static USE_SMC: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicUsize = AtomicUsize::new(1);

fn psci(fid: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let r: i64;
    // SAFETY: firmware call; clobbers per SMCCC (x0-x17 may change).
    unsafe {
        if USE_SMC.load(Ordering::Relaxed) {
            core::arch::asm!("smc #0", inout("x0") fid as i64 => r, inout("x1") a1 => _,
                inout("x2") a2 => _, inout("x3") a3 => _, out("x4") _, out("x5") _, out("x6") _,
                out("x7") _, out("x8") _, out("x9") _, out("x10") _, out("x11") _, out("x12") _,
                out("x13") _, out("x14") _, out("x15") _, out("x16") _, out("x17") _,
                options(nostack));
        } else {
            core::arch::asm!("hvc #0", inout("x0") fid as i64 => r, inout("x1") a1 => _,
                inout("x2") a2 => _, inout("x3") a3 => _, out("x4") _, out("x5") _, out("x6") _,
                out("x7") _, out("x8") _, out("x9") _, out("x10") _, out("x11") _, out("x12") _,
                out("x13") _, out("x14") _, out("x15") _, out("x16") _, out("x17") _,
                options(nostack));
        }
    }
    r
}

/// Clean + invalidate `[addr, addr+len)` to the point of coherency (for a CPU running
/// with its MMU and caches off).
fn clean_to_poc(addr: usize, len: usize) {
    let ctr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4usize << ((ctr >> 16) & 0xf);
    let mut a = addr & !(line - 1);
    while a < addr + len {
        // SAFETY: cache maintenance on our own memory.
        unsafe { core::arch::asm!("dc civac, {}", in(reg) a, options(nostack)) };
        a += line;
    }
    // SAFETY: barrier.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

fn sysreg_state() -> (u64, u64, u64) {
    let (mair, tcr, sctlr): (u64, u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("mrs {}, mair_el1", out(reg) mair, options(nomem, nostack));
        core::arch::asm!("mrs {}, tcr_el1", out(reg) tcr, options(nomem, nostack));
        core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack));
    }
    (mair, tcr, sctlr)
}

/// How the firmware starts a CPU.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
    /// A PSCI CPU_ON call (hvc or smc).
    Psci,
    /// The spin table: write the entry point at this physical address and send an event.
    Spin(u64),
}

/// The CPUs the firmware knows (MPIDR affinity, how to start each) and the PSCI conduit.
fn discover() -> (heapless_list::List, bool) {
    let mut cpus = heapless_list::List::new();
    let mut smc = crate::platform::CURRENT.psci_default_smc;
    // SAFETY: the bootloader's DTB pointer (or 0).
    if let Some(d) = unsafe { crate::dtb::Dtb::from_addr(mem::boot_dtb_ptr()) } {
        let mut found = [crate::dtb::Cpu {
            reg: 0,
            method: crate::dtb::EnableMethod::Unknown,
            release_addr: 0,
        }; crate::dtb::MAX_CPUS];
        let n = d.cpus(&mut found);
        let conduit = d.psci_conduit();
        if let Some(c) = conduit {
            smc = c == crate::dtb::Conduit::Smc;
        }
        for c in &found[..n] {
            let start = match c.method {
                crate::dtb::EnableMethod::SpinTable if c.release_addr != 0 => {
                    Start::Spin(c.release_addr)
                }
                crate::dtb::EnableMethod::SpinTable => continue, // nowhere to write: skip
                // `psci`, or no enable-method at all with a /psci node (qemu virt).
                _ if conduit.is_some() || c.method == crate::dtb::EnableMethod::Psci => Start::Psci,
                // No PSCI at all and no spin table: nothing we can do for this CPU.
                _ => continue,
            };
            cpus.push(c.reg, start);
        }
    } else if crate::platform::RASPI {
        // No device tree (qemu raspi4b without -dtb): the Pi firmware's spin table, the
        // addresses every Pi 4 tree names.
        for i in 0..4u64 {
            cpus.push(i, Start::Spin(0xd8 + 8 * i));
        }
    }
    (cpus, smc)
}

unsafe extern "C" {
    /// Physical addresses of the secondaries' boot blocks, indexed by MPIDR Aff0 (smp.S
    /// `spin_blocks`, read by `secondary_spin_entry` with the MMU off: a CPU released from the
    /// firmware's spin table arrives with no argument, so it finds its own block by its number).
    static mut spin_blocks: [u64; MAX_CPUS];
}

/// Release a CPU parked in the firmware's spin table: record its boot block, store the entry
/// point where it is polling (`release`, a physical address in low RAM), make both visible to
/// a core that reads with its caches off, and wake it. False if `release` is not usable.
fn release_spin(mpidr: u64, release: u64, blk_pa: u64) -> bool {
    let aff0 = (mpidr & 0xff) as usize;
    if aff0 >= MAX_CPUS || release == 0 || release % 8 != 0 {
        return false;
    }
    let va = mmu::phys_to_virt(release as usize);
    if mmu::with_kernel_space(|s| s.translate(va)) != Some(release as usize) {
        return false;
    }
    let entry = mmu::virt_to_phys(secondary_spin_entry as *const () as usize) as u64;
    // SAFETY: the table is ours; the release word is the firmware's mailbox for this CPU.
    unsafe {
        let slot = (&raw mut spin_blocks).cast::<u64>().add(aff0);
        slot.write_volatile(blk_pa);
        clean_to_poc(slot as usize, 8);
        (va as *mut u64).write_volatile(entry);
        clean_to_poc(va, 8);
        core::arch::asm!("sev", options(nostack, preserves_flags));
    }
    true
}

/// Start every other CPU in the device tree. After `sched::init`. Returns how many CPUs
/// are online afterwards.
pub fn init() -> usize {
    let (cpus, smc) = discover();
    USE_SMC.store(smc, Ordering::Relaxed);
    let boot_mpidr = percpu::get(0).mpidr.load(Ordering::Relaxed);
    let (mair, tcr, sctlr) = sysreg_state();
    let ttbr0 = mmu::boot_ttbr0();
    // Only ask PSCI if some CPU is started that way (a Pi's firmware has none: a call would
    // trap to nowhere).
    let uses_psci = cpus.as_slice().iter().any(|&(_, s)| s == Start::Psci);
    let ver = if uses_psci {
        psci(PSCI_VERSION, 0, 0, 0)
    } else {
        0
    };
    let mut next = 1usize;
    for &(mpidr, start) in cpus.as_slice() {
        if mpidr == boot_mpidr || next >= MAX_CPUS {
            continue;
        }
        let cpu = next;
        next += 1;
        let Some(stack) = KStack::new() else {
            kprintln!("[smp] no memory for cpu {cpu}'s stack");
            break;
        };
        percpu::get(cpu).mpidr.store(mpidr, Ordering::Relaxed);
        // SAFETY: only the boot CPU writes these, before starting `cpu`.
        let blk = unsafe {
            let b = &mut *(&raw mut BOOT).cast::<BootBlock>().add(cpu);
            b.stack_top = stack.top() as u64;
            b.mair = mair;
            b.tcr = tcr;
            b.ttbr0 = ttbr0;
            // Without WXN: the MMU comes on through the boot identity block (writable +
            // executable); smp.S sets WXN once it runs on the kernel's W^X tables.
            b.sctlr = sctlr & !mmu::SCTLR_WXN_BIT;
            b.cpu = cpu as u64;
            b.ttbr1 = mmu::kernel_ttbr1();
            b.ttbr0_final = mmu::kernel_ttbr0();
            *(&raw mut STACKS).cast::<Option<KStack>>().add(cpu) = Some(stack);
            b as *const BootBlock as usize
        };
        clean_to_poc(blk, core::mem::size_of::<BootBlock>());
        let blk_pa = mmu::virt_to_phys(blk) as u64;
        match start {
            Start::Psci => {
                let entry = mmu::virt_to_phys(secondary_entry as *const () as usize) as u64;
                let r = psci(PSCI_CPU_ON, mpidr, entry, blk_pa);
                if r != 0 {
                    kprintln!("[smp] PSCI CPU_ON for mpidr {mpidr:#x} failed: {r}");
                    continue;
                }
            }
            Start::Spin(release) => {
                if !release_spin(mpidr, release, blk_pa) {
                    kprintln!(
                        "[smp] spin-table release address {release:#x} for mpidr {mpidr:#x} is not in RAM"
                    );
                    continue;
                }
            }
        }
        // Wait (up to 1 s) for it to report in.
        let t0 = timer::now_ns();
        while !percpu::get(cpu).online.load(Ordering::Acquire)
            && timer::now_ns() - t0 < 1_000_000_000
        {
            core::hint::spin_loop();
        }
        if !percpu::get(cpu).online.load(Ordering::Acquire) {
            kprintln!("[smp] cpu {cpu} (mpidr {mpidr:#x}) did not come up");
            continue;
        }
        // And for it to join the scheduler (its idle thread exists), so whatever runs next
        // (the scheduler self-test, drivers) starts with every CPU fully in.
        let t0 = timer::now_ns();
        while percpu::get(cpu).idle.load(Ordering::Acquire) == 0
            && timer::now_ns() - t0 < 1_000_000_000
        {
            core::hint::spin_loop();
        }
        if percpu::get(cpu).idle.load(Ordering::Acquire) == 0 {
            kprintln!("[smp] cpu {cpu} came online but did not join the scheduler");
        }
    }
    let n = percpu::count();
    if uses_psci {
        kprintln!(
            "[smp] {n} of {} CPUs online (PSCI {}.{} via {})",
            cpus.as_slice().len().max(1),
            (ver >> 16) & 0xffff,
            ver & 0xffff,
            if smc { "smc" } else { "hvc" }
        );
    } else {
        kprintln!(
            "[smp] {n} of {} CPUs online (spin table)",
            cpus.as_slice().len().max(1)
        );
    }
    STARTED.store(n, Ordering::Release);
    if n > 1 {
        gic::spread_spis(true);
        kprintln!("[smp] device IRQs spread over {n} CPUs (GICD_ITARGETSR, round robin)");
    }
    n
}

/// First Rust code on a secondary CPU (MMU and caches already on, stack set).
extern "C" fn secondary_main(cpu: usize) -> ! {
    crate::exceptions::init();
    percpu::record_midr();
    sched::enable_fp();
    gic::init_cpu();
    timer::init_cpu();
    // SAFETY: set by the boot CPU before CPU_ON; this CPU is its only user from now on.
    let stack = unsafe { (*(&raw mut STACKS).cast::<Option<KStack>>().add(cpu)).take() }
        .expect("secondary stack");
    percpu::get(cpu).online.store(true, Ordering::Release);
    kprintln!(
        "[smp] cpu {cpu} up (mpidr {:#x})",
        percpu::get(cpu).mpidr.load(Ordering::Relaxed)
    );
    // IRQs stay masked until the scheduler knows this CPU (run_secondary unmasks them):
    // other CPUs may already route IRQs, IPIs and work here.
    #[cfg(feature = "smp-join-delay")]
    timer::delay_ms(200); // regression knob: the old IRQs-on window, made wide
    sched::run_secondary(cpu, stack)
}

/// Tiny fixed-capacity list (no heap needed for the DTB walk).
mod heapless_list {
    use super::Start;
    pub struct List {
        v: [(u64, Start); 16],
        n: usize,
    }
    impl List {
        pub fn new() -> Self {
            Self {
                v: [(0, Start::Psci); 16],
                n: 0,
            }
        }
        pub fn push(&mut self, x: u64, s: Start) {
            if self.n < 16 {
                self.v[self.n] = (x, s);
                self.n += 1;
            }
        }
        pub fn as_slice(&self) -> &[(u64, Start)] {
            &self.v[..self.n]
        }
    }
}
