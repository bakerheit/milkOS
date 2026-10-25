//! ESR_EL1 decoded: what an exception dump says about the syndrome (docs/raspberry-pi.md, "Reading a
//! crash"). Pure text from numbers, so the table of known values is a host test and the kernel's dump
//! (`kernel/src/exceptions.rs`) only prints [`Decode`].
//!
//! ESR_EL1 is `EC[31:26] IL[25] ISS[24:0]`. What the ISS means depends on the class:
//!
//! * data aborts: `ISV[24] SAS[23:22] SSE[21] SRT[20:16] SF[15] AR[14] FnV[10] EA[9] CM[8] S1PTW[7] WnR[6] DFSC[5:0]`;
//! * instruction aborts: `FnV[10] EA[9] S1PTW[7] IFSC[5:0]`;
//! * SError (EC 0x2f): `IDS[24]` then, if IDS is 0, `IESB[13] AET[12:10] EA[9] DFSC[5:0]` (the RAS
//!   extension); with IDS = 1 the low 24 bits are the implementation's own. A Cortex-A72 (the Pi 4)
//!   has no RAS extension: expect IDS = 1 or an all-zero syndrome, and the dump says which;
//! * SVC / HVC / SMC / BRK: the 16-bit immediate; trapped system register access: the register
//!   encoding and the direction.
//!
//! An SError is *asynchronous*: ELR_EL1 is where the CPU happened to be, not the instruction that
//! caused it (the dump's checkpoint lines are what attribute it).

use core::fmt;

pub const EC_UNKNOWN: u64 = 0x00;
pub const EC_SVC64: u64 = 0x15;
pub const EC_SYSREG: u64 = 0x18;
pub const EC_IABT_LOWER: u64 = 0x20;
pub const EC_IABT_SAME: u64 = 0x21;
pub const EC_PC_ALIGN: u64 = 0x22;
pub const EC_DABT_LOWER: u64 = 0x24;
pub const EC_DABT_SAME: u64 = 0x25;
pub const EC_SP_ALIGN: u64 = 0x26;
pub const EC_SERROR: u64 = 0x2f;
pub const EC_WATCH_LOWER: u64 = 0x34;
pub const EC_WATCH_SAME: u64 = 0x35;
pub const EC_BRK64: u64 = 0x3c;

/// The exception class of an ESR value.
pub const fn ec(esr: u64) -> u64 {
    (esr >> 26) & 0x3f
}

/// The 25-bit syndrome of an ESR value.
pub const fn iss(esr: u64) -> u64 {
    esr & 0x01ff_ffff
}

/// The exception class by name.
pub const fn ec_name(ec: u64) -> &'static str {
    match ec {
        EC_UNKNOWN => "unknown reason",
        0x01 => "trapped WFI/WFE",
        0x03 => "trapped MCR/MRC (AArch32, cp15)",
        0x04 => "trapped MCRR/MRRC (AArch32, cp15)",
        0x05 => "trapped MCR/MRC (AArch32, cp14)",
        0x06 => "trapped LDC/STC (AArch32)",
        0x07 => "trapped FP/SIMD access",
        0x08 => "trapped VMRS (AArch32)",
        0x09 => "trapped pointer authentication",
        0x0a => "trapped LD64B/ST64B",
        0x0c => "trapped MRRC (AArch32, cp14)",
        0x0d => "branch target exception",
        0x0e => "illegal execution state",
        0x11 => "SVC (AArch32)",
        0x12 => "HVC (AArch32)",
        0x13 => "SMC (AArch32)",
        EC_SVC64 => "SVC (AArch64)",
        0x16 => "HVC (AArch64)",
        0x17 => "SMC (AArch64)",
        EC_SYSREG => "trapped system register / MSR / MRS",
        0x19 => "trapped SVE access",
        0x1a => "trapped ERET",
        0x1c => "pointer authentication failure",
        0x1f => "implementation defined (EL3)",
        EC_IABT_LOWER => "instruction abort, lower EL",
        EC_IABT_SAME => "instruction abort, same EL",
        EC_PC_ALIGN => "PC alignment fault",
        EC_DABT_LOWER => "data abort, lower EL",
        EC_DABT_SAME => "data abort, same EL",
        EC_SP_ALIGN => "SP alignment fault",
        0x28 => "trapped FP exception (AArch32)",
        0x2c => "trapped FP exception",
        EC_SERROR => "SError",
        0x30 => "breakpoint, lower EL",
        0x31 => "breakpoint, same EL",
        0x32 => "software step, lower EL",
        0x33 => "software step, same EL",
        EC_WATCH_LOWER => "watchpoint, lower EL",
        EC_WATCH_SAME => "watchpoint, same EL",
        0x38 => "BKPT (AArch32)",
        0x3a => "vector catch (AArch32)",
        EC_BRK64 => "BRK (AArch64)",
        _ => "other",
    }
}

/// A data or instruction abort's fault status code (DFSC / IFSC), by name.
pub const fn fault_status(fsc: u64) -> &'static str {
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

/// An SError's DFSC field (RAS syndrome, IDS = 0).
pub const fn serror_dfsc(dfsc: u64) -> &'static str {
    match dfsc {
        0x00 => "uncategorized",
        0x11 => "asynchronous SError interrupt",
        _ => "reserved",
    }
}

/// An SError's AET field (RAS syndrome): how bad the error is for the software.
pub const fn serror_aet(aet: u64) -> &'static str {
    match aet {
        0 => "uncontainable",
        1 => "unrecoverable state",
        2 => "restartable state",
        3 => "recoverable state",
        6 => "corrected",
        _ => "reserved",
    }
}

/// How many bytes a data abort with a valid syndrome (ISV = 1) accessed: the SAS field.
pub const fn access_size(sas: u64) -> u32 {
    1 << (sas & 3)
}

/// The ESR of a given `ras`-less SError, for tests and the deliberate one: IL = 1, IDS clear.
pub const fn serror_esr(iss: u64) -> u64 {
    EC_SERROR << 26 | 1 << 25 | (iss & 0x01ff_ffff)
}

/// An ESR value (and the fault address register that goes with it) ready to print: a few short lines,
/// each indented two spaces, no trailing newline. `ras` is whether ID_AA64PFR0_EL1.RAS is non-zero (it
/// changes how an SError's syndrome reads).
#[derive(Clone, Copy, Debug)]
pub struct Decode {
    pub esr: u64,
    pub far: u64,
    pub ras: bool,
}

impl fmt::Display for Decode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let esr = self.esr;
        let ec = ec(esr);
        let il = (esr >> 25) & 1;
        let iss = iss(esr);
        write!(
            f,
            "  ESR_EL1 = {esr:#010x}  EC={ec:#04x} ({}) IL={il} ISS={iss:#09x}",
            ec_name(ec)
        )?;
        match ec {
            EC_DABT_LOWER | EC_DABT_SAME => self.data_abort(f, iss),
            EC_IABT_LOWER | EC_IABT_SAME => self.inst_abort(f, iss),
            EC_SERROR => self.serror(f, iss),
            EC_SVC64 | 0x16 | 0x17 | EC_BRK64 => write!(f, "\n  immediate = {:#x}", iss & 0xffff),
            EC_SYSREG => self.sysreg(f, iss),
            EC_PC_ALIGN | EC_SP_ALIGN | EC_WATCH_LOWER | EC_WATCH_SAME => {
                write!(f, "\n  FAR_EL1 = {:#018x}", self.far)
            }
            _ => Ok(()),
        }
    }
}

impl Decode {
    fn data_abort(&self, f: &mut fmt::Formatter<'_>, iss: u64) -> fmt::Result {
        let fsc = iss & 0x3f;
        let write = iss & (1 << 6) != 0;
        let s1ptw = iss & (1 << 7) != 0;
        let cm = iss & (1 << 8) != 0;
        let ea = iss & (1 << 9) != 0;
        let fnv = iss & (1 << 10) != 0;
        let what = if cm {
            "cache maintenance"
        } else if write {
            "write"
        } else {
            "read"
        };
        write!(
            f,
            "\n  data abort: {} on {what}{}",
            fault_status(fsc),
            if s1ptw { " (stage 1 table walk)" } else { "" }
        )?;
        write!(
            f,
            "\n  DFSC={fsc:#04x} WnR={} CM={} S1PTW={} EA={} FnV={}",
            write as u8, cm as u8, s1ptw as u8, ea as u8, fnv as u8
        )?;
        if iss & (1 << 24) != 0 {
            // ISV: the syndrome describes the access.
            write!(
                f,
                "\n  ISV=1: {} byte(s), register {}{}, {}-bit, acquire/release={}",
                access_size(iss >> 22),
                (iss >> 16) & 0x1f,
                if iss & (1 << 21) != 0 { " (sign-extended)" } else { "" },
                if iss & (1 << 15) != 0 { 64 } else { 32 },
                (iss >> 14) & 1
            )?;
        }
        if ea && fsc == 0x10 {
            write!(f, "\n  the external bus answered with an error (EA=1)")?;
        }
        if fnv {
            write!(f, "\n  FAR_EL1 = not valid")
        } else {
            write!(f, "\n  FAR_EL1 = {:#018x}  (address accessed)", self.far)
        }
    }

    fn inst_abort(&self, f: &mut fmt::Formatter<'_>, iss: u64) -> fmt::Result {
        let fsc = iss & 0x3f;
        write!(f, "\n  instruction abort: {}", fault_status(fsc))?;
        write!(
            f,
            "\n  IFSC={fsc:#04x} S1PTW={} EA={} FnV={}",
            (iss >> 7) & 1,
            (iss >> 9) & 1,
            (iss >> 10) & 1
        )?;
        if iss & (1 << 10) != 0 {
            write!(f, "\n  FAR_EL1 = not valid")
        } else {
            write!(f, "\n  FAR_EL1 = {:#018x}  (address fetched)", self.far)
        }
    }

    fn serror(&self, f: &mut fmt::Formatter<'_>, iss: u64) -> fmt::Result {
        let ids = iss & (1 << 24) != 0;
        write!(
            f,
            "\n  SError: asynchronous external abort; ELR_EL1 is where the CPU was, not the culprit"
        )?;
        if ids {
            write!(
                f,
                "\n  IDS=1: ISS[23:0] = {:#08x} is implementation defined (not RAS)",
                iss & 0xff_ffff
            )
        } else {
            let iesb = (iss >> 13) & 1;
            let aet = (iss >> 10) & 7;
            let ea = (iss >> 9) & 1;
            let dfsc = iss & 0x3f;
            write!(
                f,
                "\n  IDS=0 IESB={iesb} AET={aet} ({}) EA={ea} DFSC={dfsc:#04x} ({})",
                serror_aet(aet),
                serror_dfsc(dfsc)
            )?;
            if !self.ras {
                write!(
                    f,
                    "\n  this CPU has no RAS extension: the fields above are not architected here"
                )?;
            }
            if iss == 0 {
                write!(f, "\n  empty syndrome: the CPU says nothing about the cause")?;
            }
            Ok(())
        }
    }

    fn sysreg(&self, f: &mut fmt::Formatter<'_>, iss: u64) -> fmt::Result {
        write!(
            f,
            "\n  {} S{}_{}_C{}_C{}_{} x{}",
            if iss & 1 != 0 { "MRS" } else { "MSR" },
            (iss >> 20) & 3,
            (iss >> 14) & 7,
            (iss >> 10) & 0xf,
            (iss >> 1) & 0xf,
            (iss >> 17) & 7,
            (iss >> 5) & 0x1f
        )
    }
}

/// True if [`Decode`] already prints FAR_EL1 for this exception class (the aborts, alignment faults and
/// watchpoints), so the dump does not repeat it.
pub const fn decode_prints_far(ec: u64) -> bool {
    matches!(
        ec,
        EC_IABT_LOWER
            | EC_IABT_SAME
            | EC_DABT_LOWER
            | EC_DABT_SAME
            | EC_PC_ALIGN
            | EC_SP_ALIGN
            | EC_WATCH_LOWER
            | EC_WATCH_SAME
    )
}

/// The RAS field of ID_AA64PFR0_EL1 (bits 31:28): 0 = no RAS extension.
pub const fn pfr0_ras(pfr0: u64) -> u64 {
    (pfr0 >> 28) & 0xf
}

/// A short name for a RAS level.
pub const fn ras_name(level: u64) -> &'static str {
    match level {
        0 => "none",
        1 => "RAS",
        2 => "RAS + RASv1p1",
        _ => "RAS (newer)",
    }
}

/// The pending-exception bits of ISR_EL1 (bit 8 = SError, 7 = IRQ, 6 = FIQ), as words.
pub fn isr_pending(isr: u64) -> IsrPending {
    IsrPending(isr)
}

/// `Display` for [`isr_pending`]: "SError IRQ" or "none".
pub struct IsrPending(u64);

impl fmt::Display for IsrPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        for (bit, name) in [(8, "SError"), (7, "IRQ"), (6, "FIQ")] {
            if self.0 & (1 << bit) != 0 {
                write!(f, "{}{name}", if any { " " } else { "" })?;
                any = true;
            }
        }
        if !any {
            write!(f, "none")?;
        }
        Ok(())
    }
}
