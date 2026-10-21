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
use core::sync::atomic::{AtomicBool, Ordering};

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
    CRASHING.store(true, Ordering::Relaxed);
    let (tid, name) = crate::sched::current_for_crash();
    kprintln!("\n*** KERNEL STACK OVERFLOW ***");
    kprintln!("  thread {tid} ({name}), sp would be {sp:#018x}");
    kprintln!(
        "  ELR_EL1 = {elr:#018x}  FAR_EL1 = {far:#018x}  ESR_EL1 = {esr:#010x} ({})",
        ec_name((esr >> 26) & 0x3f)
    );
    kprintln!("*** halted ***");
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
        _ => crash(frame, vector, "synchronous exception"),
    }
}

// ---- crash dump --------------------------------------------------------------------

static CRASHING: AtomicBool = AtomicBool::new(false);

fn halt() -> ! {
    unsafe { asm!("msr daifset, #0xf", options(nomem, nostack)) };
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// Print a crash dump for the trapped context and halt. Usable from any handler.
pub fn crash(frame: &TrapFrame, vector: usize, what: &str) -> ! {
    // A fault while printing the dump would recurse forever; bail out on the second entry.
    if CRASHING.load(Ordering::Relaxed) {
        kprintln!("\n*** DOUBLE FAULT while dumping a crash (vector {vector}) ***");
        halt();
    }
    CRASHING.store(true, Ordering::Relaxed);

    let esr = frame.esr;
    let ec = (esr >> 26) & 0x3f;
    let il = (esr >> 25) & 1;
    let iss = esr & 0x01ff_ffff;
    let source = match vector / 4 {
        0 => "current EL, SP_EL0",
        1 => "current EL, SP_ELx",
        2 => "lower EL, AArch64",
        _ => "lower EL, AArch32",
    };

    kprintln!("\n*** KERNEL EXCEPTION: {what} ***");
    kprintln!("  vector {vector} ({source})");
    if vector % 4 == 0 {
        kprintln!(
            "  ESR_EL1 = {:#010x}  EC={:#04x} ({}) IL={} ISS={:#09x}",
            esr,
            ec,
            ec_name(ec),
            il,
            iss
        );
        match ec {
            EC_DABT_LOWER | EC_DABT_SAME => dump_data_abort(frame, iss),
            EC_IABT_LOWER | EC_IABT_SAME => dump_inst_abort(frame, iss),
            EC_SVC64 | EC_BRK64 => kprintln!("  immediate = {:#x}", iss & 0xffff),
            0x22 | 0x26 | 0x34 | 0x35 => kprintln!("  FAR_EL1 = {:#018x}", frame.far),
            _ => {}
        }
    }
    kprintln!("  ELR_EL1 = {:#018x}  (faulting pc)", frame.elr);
    kprintln!(
        "  SPSR    = {:#010x}  {}",
        frame.spsr,
        spsr_desc(frame.spsr)
    );
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
    kprintln!("*** halted ***");
    halt();
}

fn dump_data_abort(frame: &TrapFrame, iss: u64) {
    let fsc = iss & 0x3f;
    let write = iss & (1 << 6) != 0;
    let s1ptw = iss & (1 << 7) != 0;
    let cache_maint = iss & (1 << 8) != 0;
    let fnv = iss & (1 << 10) != 0;
    kprintln!(
        "  data abort: {} on {}{}{}",
        fault_status(fsc),
        if cache_maint {
            "cache maintenance"
        } else if write {
            "write"
        } else {
            "read"
        },
        if s1ptw { " (stage 1 table walk)" } else { "" },
        if iss & (1 << 24) != 0 {
            // ISV: syndrome valid, SAS gives the access size.
            match (iss >> 22) & 3 {
                0 => ", 1 byte",
                1 => ", 2 bytes",
                2 => ", 4 bytes",
                _ => ", 8 bytes",
            }
        } else {
            ""
        }
    );
    if fnv {
        kprintln!("  FAR_EL1 = not valid");
    } else {
        kprintln!("  FAR_EL1 = {:#018x}  (address accessed)", frame.far);
    }
}

fn dump_inst_abort(frame: &TrapFrame, iss: u64) {
    kprintln!("  instruction abort: {}", fault_status(iss & 0x3f));
    if iss & (1 << 10) != 0 {
        kprintln!("  FAR_EL1 = not valid");
    } else {
        kprintln!("  FAR_EL1 = {:#018x}  (address fetched)", frame.far);
    }
}

pub(crate) fn fault_status(fsc: u64) -> &'static str {
    match fsc {
        0x00 => "address size fault, level 0",
        0x01 => "address size fault, level 1",
        0x02 => "address size fault, level 2",
        0x03 => "address size fault, level 3",
        0x04 => "translation fault, level 0",
        0x05 => "translation fault, level 1",
        0x06 => "translation fault, level 2",
        0x07 => "translation fault, level 3",
        0x09 => "access flag fault, level 1",
        0x0a => "access flag fault, level 2",
        0x0b => "access flag fault, level 3",
        0x0d => "permission fault, level 1",
        0x0e => "permission fault, level 2",
        0x0f => "permission fault, level 3",
        0x10 => "synchronous external abort",
        0x11 => "synchronous tag check fault",
        0x13..=0x17 => "synchronous external abort on table walk",
        0x18 => "synchronous parity/ECC error",
        0x1b..=0x1f => "synchronous parity/ECC error on table walk",
        0x21 => "alignment fault",
        0x30 => "TLB conflict abort",
        0x31 => "unsupported atomic hardware update fault",
        0x35 => "unsupported exclusive or atomic access",
        _ => "other/unknown fault status",
    }
}

pub(crate) fn ec_name(ec: u64) -> &'static str {
    match ec {
        EC_UNKNOWN => "unknown reason",
        0x01 => "trapped WFI/WFE",
        0x07 => "trapped FP/SIMD access",
        0x0e => "illegal execution state",
        EC_SVC64 => "SVC (AArch64)",
        0x16 => "HVC (AArch64)",
        0x17 => "SMC (AArch64)",
        0x18 => "trapped system register / MSR / MRS",
        0x19 => "trapped SVE access",
        EC_IABT_LOWER => "instruction abort, lower EL",
        EC_IABT_SAME => "instruction abort, same EL",
        0x22 => "PC alignment fault",
        EC_DABT_LOWER => "data abort, lower EL",
        EC_DABT_SAME => "data abort, same EL",
        0x26 => "SP alignment fault",
        0x2c => "trapped FP exception",
        0x2f => "SError",
        0x30 => "breakpoint, lower EL",
        0x31 => "breakpoint, same EL",
        0x32 => "software step, lower EL",
        0x33 => "software step, same EL",
        0x34 => "watchpoint, lower EL",
        0x35 => "watchpoint, same EL",
        EC_BRK64 => "BRK (AArch64)",
        _ => "other",
    }
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
