//! Exception vectors, trap frame, and the Rust side of exception handling.
//!
//! All 16 vector slots (see `exceptions.S`) save a [`TrapFrame`] on the kernel stack and
//! call [`exception_entry`]. From there:
//! - IRQ goes to [`crate::gic::handle_irq`].
//! - SVC goes to the syscall hook ([`set_syscall_handler`]); the default one only logs.
//! - A lower-EL fault goes to the user-fault hook ([`set_user_fault_handler`]) if one claims
//!   it, otherwise it is fatal.
//! - Everything else (kernel data/instruction aborts, undefined instructions, SError, ...)
//!   prints a crash dump and halts. The one exception is `BRK #BRK_SELFTEST`, which the boot
//!   self-test uses to prove traps are caught: it logs and resumes after the instruction.
//!
//! A scheduler can build a `TrapFrame` for a new task and resume it with [`restore_frame`].

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::arch::{asm, global_asm};
use core::mem::{offset_of, size_of};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::FnSlot;
use crate::{gic, kprintln};

/// Register state saved on every exception. `repr(C)` layout is relied on by the asm in
/// `exceptions.S` (offsets are generated from this struct, so they cannot drift).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    /// x0..x30 (x30 is the link register).
    pub x: [u64; 31],
    /// The EL0 stack pointer (SP_EL0). The EL1 stack pointer is not saved: it is the frame
    /// address plus `size_of::<TrapFrame>()` (see [`TrapFrame::kernel_sp`]).
    pub sp_el0: u64,
    /// Return address (ELR_EL1).
    pub elr: u64,
    /// Saved PSTATE (SPSR_EL1).
    pub spsr: u64,
    /// ESR_EL1 at the time of the exception (only meaningful for synchronous ones).
    pub esr: u64,
    /// FAR_EL1 (only meaningful for aborts, alignment and watchpoints).
    pub far: u64,
}

impl TrapFrame {
    /// An all-zero frame, handy as a starting point for building a new task's first frame.
    pub const fn zeroed() -> Self {
        Self {
            x: [0; 31],
            sp_el0: 0,
            elr: 0,
            spsr: 0,
            esr: 0,
            far: 0,
        }
    }

    /// The EL1 stack pointer the interrupted code had, when it was running on SP_EL1.
    pub fn kernel_sp(&self) -> u64 {
        self as *const Self as u64 + size_of::<Self>() as u64
    }

    /// True if the exception came from EL0.
    pub fn is_user(&self) -> bool {
        self.spsr & 0x1f == 0
    }
}

/// `BRK` immediate the boot self-test uses; a kernel BRK with this value is logged and skipped.
pub const BRK_SELFTEST: u16 = 0x5e1f;

global_asm!(
    include_str!("exceptions.S"),
    FRAME_SIZE = const size_of::<TrapFrame>(),
    OFF_X30 = const offset_of!(TrapFrame, x) + 30 * 8,
    OFF_SP_EL0 = const offset_of!(TrapFrame, sp_el0),
    OFF_ELR = const offset_of!(TrapFrame, elr),
    OFF_SPSR = const offset_of!(TrapFrame, spsr),
    OFF_ESR = const offset_of!(TrapFrame, esr),
    OFF_FAR = const offset_of!(TrapFrame, far),
    KSTACK_SHIFT = const crate::sched::kstack::KSTACK_SHIFT,
    HANDLER = sym exception_entry,
    OVERFLOW = sym kstack_overflow,
);

// ---- fault probes (the W^X self-test) -----------------------------------------------
//
// `probe_write` stores one byte and `probe_exec` calls an address, both from EL1; if that
// faults, `sync` sees the abort at a known pc, records ESR/FAR and resumes at the probe's
// "faulted" exit instead of crashing. Nothing else uses them.

global_asm!(
    ".section .text",
    ".global milk_probe_store",
    "milk_probe_store:",
    "    strb w1, [x0]",
    "    mov x0, #0",
    "    ret",
    ".global milk_probe_store_fault",
    "milk_probe_store_fault:",
    "    mov x0, #1",
    "    ret",
    ".global milk_probe_call",
    "milk_probe_call:",
    "    stp x29, x30, [sp, #-16]!",
    "    blr x0",
    "    mov x0, #0",
    "    b 1f",
    ".global milk_probe_call_fault",
    "milk_probe_call_fault:",
    "    mov x0, #1",
    "1:  ldp x29, x30, [sp], #16",
    "    ret",
);

unsafe extern "C" {
    fn milk_probe_store(addr: usize, v: u8) -> u64;
    fn milk_probe_store_fault();
    fn milk_probe_call(addr: usize) -> u64;
    fn milk_probe_call_fault();
}

/// Address `probe_exec` is calling (an instruction abort there is the probe's).
static PROBE_TARGET: AtomicUsize = AtomicUsize::new(0);
static PROBE_ESR: AtomicUsize = AtomicUsize::new(0);

/// Write `v` to `addr` from EL1. `Err(esr)` if it faulted (ESR_EL1 of the abort).
pub fn probe_write(addr: usize, v: u8) -> Result<(), u64> {
    // SAFETY: the stub only stores a byte; a fault is caught in `sync`.
    if unsafe { milk_probe_store(addr, v) } == 0 {
        Ok(())
    } else {
        Err(PROBE_ESR.load(Ordering::Relaxed) as u64)
    }
}

/// Call the code at `addr` (it must be a function that just returns, if it runs at all).
/// `Err(esr)` if fetching it faulted.
pub fn probe_exec(addr: usize) -> Result<(), u64> {
    PROBE_TARGET.store(addr, Ordering::Relaxed);
    // SAFETY: `addr` is a `ret` the caller wrote, or faults (caught in `sync`).
    let r = unsafe { milk_probe_call(addr) };
    PROBE_TARGET.store(0, Ordering::Relaxed);
    if r == 0 {
        Ok(())
    } else {
        Err(PROBE_ESR.load(Ordering::Relaxed) as u64)
    }
}

/// A probe's abort: resume at its "faulted" exit. False if this abort isn't a probe's.
fn probe_fault(frame: &mut TrapFrame, ec: u64) -> bool {
    let elr = frame.elr as usize;
    let resume = if ec == EC_DABT_SAME && elr == milk_probe_store as *const () as usize {
        milk_probe_store_fault as *const () as usize
    } else if ec == EC_IABT_SAME && elr != 0 && elr == PROBE_TARGET.load(Ordering::Relaxed) {
        milk_probe_call_fault as *const () as usize
    } else {
        return false;
    };
    PROBE_ESR.store(frame.esr as usize, Ordering::Relaxed);
    frame.elr = resume as u64;
    true
}

/// Stack for reporting a kernel stack overflow (the overflowing stack is unusable).
#[repr(C, align(16))]
struct EmergencyStack([u8; 16 * 1024]);
#[unsafe(no_mangle)]
static mut __emergency_stack: EmergencyStack = EmergencyStack([0; 16 * 1024]);
global_asm!(
    ".global __emergency_stack_top",
    ".set __emergency_stack_top, __emergency_stack + {size}",
    size = const size_of::<EmergencyStack>(),
);

/// Called on the emergency stack when an EL1 exception would push its frame below the
/// current kernel stack.
extern "C" fn kstack_overflow(sp: u64, elr: u64, far: u64, esr: u64) -> ! {
    CRASHING.store(crate::percpu::id() + 1, Ordering::Relaxed);
    #[cfg(feature = "raspi")]
    crate::rpi::logbuf::emergency();
    let (tid, name) = crate::sched::current_for_crash();
    kprintln!("\n*** KERNEL STACK OVERFLOW ***");
    kprintln!("  thread {tid} ({name}), sp would be {sp:#018x}");
    kprintln!(
        "  ELR_EL1 = {elr:#018x}  FAR_EL1 = {far:#018x}  ESR_EL1 = {esr:#010x} ({})",
        ec_name((esr >> 26) & 0x3f)
    );
    kprintln!("*** halted ***");
    if crate::platform::RASPI {
        // No serial cable on the Pi: the dump on the screen, the LED cycle.
        #[cfg(feature = "raspi")]
        crate::rpi::led::note_stack_overflow(elr, far);
        crate::crashscreen::show();
        #[cfg(feature = "raspi")]
        crate::rpi::led::blink_forever();
    }
    halt();
}

unsafe extern "C" {
    static exception_vectors: u8;
    /// Resume the context described by `frame`: switch the kernel stack to it and `eret`.
    /// The frame must live in a region of at least `size_of::<TrapFrame>()` bytes, 16-byte
    /// aligned; afterwards SP_EL1 points just past it.
    pub unsafe fn restore_frame(frame: *const TrapFrame) -> !;
}

/// Point VBAR_EL1 at our vector table. IRQs stay masked; call [`crate::sync::irq_enable`]
/// once the interrupt controller is set up.
pub fn init() {
    unsafe {
        let base = &raw const exception_vectors as u64;
        asm!("msr vbar_el1, {}", "isb", in(reg) base, options(nostack));
    }
}

// ---- hooks -------------------------------------------------------------------------

/// Syscall handler: called for `SVC` with the trapped frame and the SVC immediate. By the
/// usual convention the syscall number is in x8 and args in x0..x5; write the return value
/// into `frame.x[0]`. For SVC the saved ELR already points past the instruction.
pub type SyscallHandler = fn(frame: &mut TrapFrame, imm: u16);

/// Hook for faults taken from EL0. Return true if handled (e.g. the process was killed and
/// the frame redirected); false to treat it as fatal.
pub type UserFaultHandler = fn(frame: &mut TrapFrame) -> bool;

/// Hook run after every IRQ, once the interrupt has been acknowledged and ended (EOI), still
/// with IRQs masked and on the interrupted context's kernel stack. The scheduler uses it to
/// preempt: it may switch to another thread and come back here much later.
pub type IrqExitHook = fn(frame: &mut TrapFrame);

static SYSCALL: FnSlot = FnSlot::new();
static USER_FAULT: FnSlot = FnSlot::new();
static IRQ_EXIT: FnSlot = FnSlot::new();

pub fn set_irq_exit_hook(h: IrqExitHook) {
    IRQ_EXIT.set(h);
}

/// Hook run last, IRQs masked, whenever an exception (syscall, fault, IRQ) is about to
/// return to EL0. The process layer reschedules and delivers signals here; it may rewrite
/// the frame or never return (process killed).
pub type UserReturnHook = fn(frame: &mut TrapFrame);
static USER_RETURN: FnSlot = FnSlot::new();

pub fn set_user_return_hook(h: UserReturnHook) {
    USER_RETURN.set(h);
}

pub fn set_syscall_handler(h: SyscallHandler) {
    SYSCALL.set(h);
}

pub fn set_user_fault_handler(h: UserFaultHandler) {
    USER_FAULT.set(h);
}

fn default_syscall(frame: &mut TrapFrame, imm: u16) {
    kprintln!(
        "[svc] from {} imm={:#x} x8={:#x} x0={:#x} x1={:#x} x2={:#x} (no syscall handler yet)",
        if frame.is_user() { "EL0" } else { "EL1" },
        imm,
        frame.x[8],
        frame.x[0],
        frame.x[1],
        frame.x[2],
    );
    frame.x[0] = u64::MAX; // -1: not implemented
}

// ---- dispatch ----------------------------------------------------------------------

const EC_UNKNOWN: u64 = 0x00;
const EC_SVC64: u64 = 0x15;
const EC_IABT_LOWER: u64 = 0x20;
const EC_IABT_SAME: u64 = 0x21;
const EC_DABT_LOWER: u64 = 0x24;
const EC_DABT_SAME: u64 = 0x25;
const EC_BRK64: u64 = 0x3c;

#[unsafe(no_mangle)]
extern "C" fn exception_entry(frame: &mut TrapFrame, vector: usize) {
    match vector % 4 {
        0 => sync(frame, vector),
        1 => {
            gic::handle_irq(frame);
            if let Some(h) = IRQ_EXIT.get::<IrqExitHook>() {
                h(frame);
            }
        }
        2 => crash(frame, vector, "FIQ (unexpected: we only use IRQ)"),
        _ if crate::serror::absorb(crate::percpu::id(), vector, frame.esr) => {}
        _ => crash(frame, vector, "SError"),
    }
    if frame.is_user()
        && let Some(h) = USER_RETURN.get::<UserReturnHook>()
    {
        crate::sync::irq_disable();
        h(frame);
    }
}

fn sync(frame: &mut TrapFrame, vector: usize) {
    let ec = (frame.esr >> 26) & 0x3f;
    let iss = frame.esr & 0x01ff_ffff;
    // Vectors 12..15 are AArch32 lower-EL: nothing there is supported.
    if vector >= 12 {
        crash(frame, vector, "exception from AArch32 EL0 (unsupported)");
    }
    match ec {
        EC_SVC64 => {
            let imm = (iss & 0xffff) as u16;
            match SYSCALL.get::<SyscallHandler>() {
                Some(h) => h(frame, imm),
                None => default_syscall(frame, imm),
            }
        }
        EC_BRK64 if !frame.is_user() && (iss & 0xffff) as u16 == BRK_SELFTEST => {
            kprintln!(
                "[brk] caught BRK #{:#x} at {:#x}, resuming",
                BRK_SELFTEST,
                frame.elr
            );
            frame.elr += 4; // BRK does not advance ELR itself.
        }
        _ if (vector >= 8)
            && USER_FAULT
                .get::<UserFaultHandler>()
                .is_some_and(|h| h(frame)) => {}
        EC_DABT_SAME | EC_IABT_SAME if vector < 8 && probe_fault(frame, ec) => {}
        _ => crash(frame, vector, "synchronous exception"),
    }
}

// ---- crash dump --------------------------------------------------------------------

/// CPU number + 1 of the CPU printing a crash dump (0: none).
static CRASHING: AtomicUsize = AtomicUsize::new(0);

/// Read a system register by name.
macro_rules! read_sysreg {
    ($name:literal) => {{
        let v: u64;
        // SAFETY: a read of a register that exists at EL1.
        unsafe { asm!(concat!("mrs {}, ", $name), out(reg) v, options(nomem, nostack)) };
        v
    }};
}

fn halt() -> ! {
    unsafe { asm!("msr daifset, #0xf", options(nomem, nostack)) };
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// Print a crash dump for the trapped context and halt. Usable from any handler.
pub fn crash(frame: &TrapFrame, vector: usize, what: &str) -> ! {
    // The SD card log stops rate limiting: every line of the dump is kept.
    #[cfg(feature = "raspi")]
    crate::rpi::logbuf::emergency();
    let cpu = crate::percpu::id();
    // One full dump at a time. A fault while *this* CPU prints its dump is a real double
    // fault (bail out, don't recurse); another CPU crashing at the same moment (two CPUs
    // running a corrupted thread, say) gets a one-line summary and halts, so the first
    // dump stays readable.
    if let Err(owner) = CRASHING.compare_exchange(0, cpu + 1, Ordering::AcqRel, Ordering::Acquire) {
        if owner == cpu + 1 {
            kprintln!(
                "\n*** DOUBLE FAULT on cpu {cpu} while dumping a crash (vector {vector}) ***"
            );
            // The dump never got as far as the LED: show the fault that stopped it.
            #[cfg(feature = "raspi")]
            if crate::platform::RASPI {
                crate::rpi::led::note_exception(frame.esr, frame.elr, frame.far);
                crate::rpi::led::blink_forever();
            }
        } else {
            // Let the first dump finish (bounded: it may itself be stuck).
            let t0 = crate::timer::now_ns();
            while crate::timer::now_ns() - t0 < 300_000_000 {
                core::hint::spin_loop();
            }
            kprintln!(
                "*** cpu {cpu} also crashed (cpu {} is dumping): {what}, vector {vector}, ESR {:#x}, ELR {:#x}, FAR {:#x}, lr {:#x}",
                owner - 1,
                frame.esr,
                frame.elr,
                frame.far,
                frame.x[30]
            );
        }
        halt();
    }

    crate::serror::dying();
    let esr = frame.esr;
    let source = match vector / 4 {
        0 => "current EL, SP_EL0",
        1 => "current EL, SP_ELx",
        2 => "lower EL, AArch64",
        _ => "lower EL, AArch32",
    };

    kprintln!("\n*** KERNEL EXCEPTION: {what} ***");
    let (tid, name) = crate::sched::current_for_crash();
    kprintln!("  vector {vector} ({source}), cpu {cpu}, thread {tid} ({name})");
    let pfr0 = read_sysreg!("id_aa64pfr0_el1");
    let ras = rpikit::esr::pfr0_ras(pfr0) != 0;
    let ec = rpikit::esr::ec(esr);
    if vector % 4 == 0 || vector % 4 == 3 {
        kprintln!(
            "{}",
            rpikit::esr::Decode {
                esr,
                far: frame.far,
                ras
            }
        );
    } else {
        // IRQ / FIQ vector that should not be taken: ESR_EL1 is whatever the last synchronous exception left.
        kprintln!("  ESR_EL1 = {esr:#010x}  (stale: not an ESR-reporting vector)");
    }
    kprintln!(
        "  ELR_EL1 = {:#018x}  ({})",
        frame.elr,
        if vector % 4 == 3 {
            "where the CPU was, not the culprit"
        } else {
            "faulting pc"
        }
    );
    if vector % 4 != 0 || !rpikit::esr::decode_prints_far(ec) {
        kprintln!("  FAR_EL1 = {:#018x}", frame.far);
    }
    kprintln!(
        "  SPSR_EL1 = {:#010x}  {}",
        frame.spsr,
        spsr_desc(frame.spsr)
    );
    kprintln!(
        "  SCTLR_EL1 = {:#010x}  ISR_EL1 = {:#x} (pending now: {})  ID_AA64PFR0_EL1 = {:#x} (RAS: {})  MPIDR = {:#x}",
        read_sysreg!("sctlr_el1"),
        read_sysreg!("isr_el1"),
        rpikit::esr::isr_pending(read_sysreg!("isr_el1")),
        pfr0,
        rpikit::esr::ras_name(rpikit::esr::pfr0_ras(pfr0)),
        read_sysreg!("mpidr_el1")
    );
    crate::serror::print_ras(pfr0);
    crate::serror::report(cpu, vector);
    kprintln!("  SP_EL0  = {:#018x}", frame.sp_el0);
    kprintln!("  SP_EL1  = {:#018x}", frame.kernel_sp());
    for (i, pair) in frame.x[..30].chunks(2).enumerate() {
        kprintln!(
            "  x{:<2} = {:#018x}  x{:<2} = {:#018x}",
            i * 2,
            pair[0],
            i * 2 + 1,
            pair[1]
        );
    }
    kprintln!("  x30 = {:#018x}  (lr)", frame.x[30]);
    // The crash screen shows the end of the log: say the important things once more, last.
    kprintln!(
        "  summary: {what}, cpu {cpu}, thread {name}, ESR {esr:#x} (EC {ec:#04x} {}), ELR {:#x}, FAR {:#x}",
        rpikit::esr::ec_name(ec),
        frame.elr,
        frame.far
    );
    crate::serror::summary(cpu, vector);
    kprintln!("*** halted ***");
    if crate::platform::RASPI {
        // No serial cable on the Pi: the same dump on the screens, then the LED cycle (the code line
        // is logged first so the crash screens end with it).
        #[cfg(feature = "raspi")]
        {
            if vector % 4 == 0 || vector % 4 == 3 {
                crate::rpi::led::note_exception(frame.esr, frame.elr, frame.far);
            } else {
                // An IRQ or FIQ vector that should not be taken: no ESR to speak of.
                crate::rpi::led::note_exception(0x3d << 26, frame.elr, 0);
            }
            crate::crashscreen::show();
            crate::rpi::led::blink_forever();
        }
    }
    halt();
}

pub(crate) fn fault_status(fsc: u64) -> &'static str {
    rpikit::esr::fault_status(fsc)
}

pub(crate) fn ec_name(ec: u64) -> &'static str {
    rpikit::esr::ec_name(ec)
}

/// One-line decode of an SPSR value; the returned `Display` avoids needing an allocator.
fn spsr_desc(spsr: u64) -> impl core::fmt::Display {
    struct D(u64);
    impl core::fmt::Display for D {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let s = self.0;
            let mode = match s & 0x1f {
                0x00 => "EL0t",
                0x04 => "EL1t",
                0x05 => "EL1h",
                0x08 => "EL2t",
                0x09 => "EL2h",
                m if m & 0x10 != 0 => "AArch32",
                _ => "?",
            };
            let flag = |bit: u32, c: char| if s & (1 << bit) != 0 { c } else { '-' };
            write!(
                f,
                "{} NZCV={}{}{}{} DAIF={}{}{}{}",
                mode,
                flag(31, 'N'),
                flag(30, 'Z'),
                flag(29, 'C'),
                flag(28, 'V'),
                flag(9, 'D'),
                flag(8, 'A'),
                flag(7, 'I'),
                flag(6, 'F'),
            )
        }
    }
    D(spsr)
}
