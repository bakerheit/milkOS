//! The ACT LED after a crash (docs/raspberry-pi.md, "Reading the ACT LED"): a repeating cycle that says
//! where the kernel was and why it stopped, readable by counting flashes with nothing else attached.
//! Pure data like [`crate::ledcode`]: [`CrashCycle`] yields the on/off intervals, [`decode`] reads a list of
//! intervals back (the same rules as `scripts/led-decode.py`), and the kernel
//! (`kernel/src/rpi/led.rs`) only switches the pin and waits.
//!
//! One cycle, then it repeats:
//!
//! 1. **Alarm**: 10 fast flashes (100 ms on, 100 ms off, 2 s in all) then dark 2.5 s. Nothing else in
//!    milkOS looks like it, so "it crashed" is clear at a glance.
//! 2. **Stage** (1 digit, 0 to 8): the boot stage reached, as in the boot codes: 0 = before any stage,
//!    1 to 7 the stages, 8 = the desktop was up.
//! 3. **Kind** (2 octal digits, 6 bits): the exception class (ESR_EL1 EC; 0o45 = data abort at EL1,
//!    0o44 = from EL0, 0o41 / 0o40 instruction aborts, 0o25 SVC, 0o74 BRK...), or a marker:
//!    [`EC_PANIC`] 0o77 a Rust panic, [`EC_STACK_OVERFLOW`] 0o76, [`EC_OTHER`] 0o75 (an interrupt vector that
//!    should never be taken, or no information).
//! 4. **PC** (12 octal digits = low 36 bits): ELR_EL1, the faulting address. A kernel address is
//!    [`KERNEL_BASE`] + this. For a panic it is instead the source line of the panic.
//! 5. **FAR** (8 octal digits = low 24 bits): the faulting data address (FAR_EL1). For a panic it is the
//!    [`file_hash24`] of the source file name.
//!
//! A digit `d` is `d` short pulses (150 ms on, 250 ms dark between them); a digit 0 is one long pulse
//! (600 ms). Dark after a digit: 700 ms; after a group: 2.5 s. The worst cycle (all sevens) takes under 90 s.

use crate::ledcode::Interval;

/// A kernel virtual address is `KERNEL_BASE` + the physical address it is loaded at (linker.ld, mmu.rs).
pub const KERNEL_BASE: u64 = 0xffff_0000_0000_0000;

/// EC marker: a Rust panic.
pub const EC_PANIC: u8 = 0o77;
/// EC marker: the kernel stack overflowed.
pub const EC_STACK_OVERFLOW: u8 = 0o76;
/// EC marker: an interrupt or FIQ vector that should never be taken, or nothing was recorded.
pub const EC_OTHER: u8 = 0o75;

/// Alarm: fast pulses, on and off.
pub const ALARM_PULSES: u8 = 10;
pub const ALARM_MS: u32 = 100;
/// A digit's short pulse, and the dark between pulses of one digit.
pub const PULSE_MS: u32 = 150;
pub const PULSE_GAP_MS: u32 = 250;
/// Digit 0: one long pulse.
pub const ZERO_MS: u32 = 600;
/// Dark after a digit (before the next digit of the same group).
pub const DIGIT_GAP_MS: u32 = 700;
/// Dark after a group (and after the alarm).
pub const GROUP_GAP_MS: u32 = 2500;

pub const STAGE_DIGITS: usize = 1;
pub const EC_DIGITS: usize = 2;
pub const PC_DIGITS: usize = 12;
pub const FAR_DIGITS: usize = 8;
/// All digits of a cycle.
pub const DIGITS: usize = STAGE_DIGITS + EC_DIGITS + PC_DIGITS + FAR_DIGITS;
/// The highest stage number (the desktop is up).
pub const MAX_STAGE: u8 = 8;

const PC_MASK: u64 = (1 << 36) - 1;
const FAR_MASK: u32 = (1 << 24) - 1;

/// What a crash says. Fields are cut to the sizes the cycle carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CrashRecord {
    /// Boot stage reached, 0 to [`MAX_STAGE`].
    pub stage: u8,
    /// Exception class or marker, 6 bits.
    pub ec: u8,
    /// Low 36 bits of the faulting PC (or the panic's source line).
    pub pc: u64,
    /// Low 24 bits of the faulting address (or the panic's file hash).
    pub far: u32,
}

impl CrashRecord {
    pub fn new(stage: u8, ec: u8, pc: u64, far: u64) -> CrashRecord {
        CrashRecord {
            stage: stage.min(MAX_STAGE),
            ec: ec & 0x3f,
            pc: pc & PC_MASK,
            far: (far as u32) & FAR_MASK,
        }
    }

    /// An exception: ESR_EL1, ELR_EL1, FAR_EL1.
    pub fn exception(stage: u8, esr: u64, elr: u64, far: u64) -> CrashRecord {
        CrashRecord::new(stage, ((esr >> 26) & 0x3f) as u8, elr, far)
    }

    /// A panic at `file:line`.
    pub fn panic(stage: u8, file: &str, line: u32) -> CrashRecord {
        CrashRecord::new(
            stage,
            EC_PANIC,
            u64::from(line),
            u64::from(file_hash24(file)),
        )
    }

    /// Digit `i` of the cycle (0..[`DIGITS`]): the stage, then the octal digits of EC, PC, FAR, most
    /// significant first.
    pub fn digit(&self, i: usize) -> u8 {
        fn oct(v: u64, n: usize, k: usize) -> u8 {
            ((v >> (3 * (n - 1 - k))) & 7) as u8
        }
        if i < STAGE_DIGITS {
            self.stage
        } else if i < STAGE_DIGITS + EC_DIGITS {
            oct(u64::from(self.ec), EC_DIGITS, i - STAGE_DIGITS)
        } else if i < STAGE_DIGITS + EC_DIGITS + PC_DIGITS {
            oct(self.pc, PC_DIGITS, i - STAGE_DIGITS - EC_DIGITS)
        } else {
            oct(
                u64::from(self.far),
                FAR_DIGITS,
                i - STAGE_DIGITS - EC_DIGITS - PC_DIGITS,
            )
        }
    }

    /// The full kernel address for `pc` (valid when the crash was in the kernel).
    pub fn kernel_pc(&self) -> u64 {
        KERNEL_BASE | self.pc
    }

    pub fn is_panic(&self) -> bool {
        self.ec == EC_PANIC
    }
}

impl core::fmt::Display for CrashRecord {
    /// The values as a person types them into `scripts/led-decode.py`, octal digits as counted.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "stage={} ec=0o{:02o} pc=0o{:012o} far=0o{:08o}",
            self.stage, self.ec, self.pc, self.far
        )
    }
}

/// FNV-1a over the bytes, cut to 24 bits: names a source file in a panic's code.
pub fn file_hash24(file: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in file.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h & FAR_MASK
}

/// Is digit `i` the last of its group?
fn group_end(i: usize) -> bool {
    i + 1 == STAGE_DIGITS
        || i + 1 == STAGE_DIGITS + EC_DIGITS
        || i + 1 == STAGE_DIGITS + EC_DIGITS + PC_DIGITS
        || i + 1 == DIGITS
}

/// The intervals of one whole cycle (alarm, then every digit). Finite: the kernel starts a new one.
#[derive(Clone, Copy, Debug)]
pub struct CrashCycle {
    rec: CrashRecord,
    /// 0 = the alarm, 1.. = digit `pos - 1`.
    pos: usize,
    step: u16,
}

impl CrashCycle {
    pub fn new(rec: CrashRecord) -> CrashCycle {
        CrashCycle {
            rec,
            pos: 0,
            step: 0,
        }
    }

    /// How long the whole cycle takes, in milliseconds.
    pub fn total_ms(rec: CrashRecord) -> u32 {
        CrashCycle::new(rec).map(|i| i.ms).sum()
    }
}

impl Iterator for CrashCycle {
    type Item = Interval;

    fn next(&mut self) -> Option<Interval> {
        loop {
            let step = self.step;
            if self.pos == 0 {
                if step >= 2 * u16::from(ALARM_PULSES) {
                    self.pos = 1;
                    self.step = 0;
                    continue;
                }
                let on = step % 2 == 0;
                let ms = if !on && step + 1 == 2 * u16::from(ALARM_PULSES) {
                    GROUP_GAP_MS
                } else {
                    ALARM_MS
                };
                self.step = step + 1;
                return Some(Interval { on, ms });
            }
            let i = self.pos - 1;
            if i >= DIGITS {
                return None;
            }
            let d = self.rec.digit(i);
            let n = u16::from(d.max(1));
            if step >= 2 * n {
                self.pos += 1;
                self.step = 0;
                continue;
            }
            self.step = step + 1;
            return Some(if step % 2 == 0 {
                Interval {
                    on: true,
                    ms: if d == 0 { ZERO_MS } else { PULSE_MS },
                }
            } else {
                Interval {
                    on: false,
                    ms: if step + 1 < 2 * n {
                        PULSE_GAP_MS
                    } else if group_end(i) {
                        GROUP_GAP_MS
                    } else {
                        DIGIT_GAP_MS
                    },
                }
            });
        }
    }
}

// ---- reading it back (host tests; scripts/led-decode.py has the same rules) ----------------------

/// A pulse followed by dark shorter than this is an alarm flash (100 ms dark; a digit's pulse is followed by
/// at least 250). The alarm's last flash is followed by the group gap, so it is the pulse right after another
/// alarm flash. (Judged by the dark, not the light: 100 vs 150 ms is only a frame of a phone video apart.)
pub const ALARM_OFF_MAX_MS: u32 = 175;
/// On at least this long is a digit 0's long pulse (600) rather than a short one (150).
pub const LONG_ON_MIN_MS: u32 = 375;
/// Dark shorter than this is between pulses of one digit (250); at least this, the digit ended.
pub const DIGIT_OFF_MIN_MS: u32 = 475;
/// Dark at least this long ends a group (2500, not 700).
pub const GROUP_OFF_MIN_MS: u32 = 1600;

/// Find the first complete cycle in a list of intervals (on and off, in time order; it may start
/// anywhere, and noise before the first alarm is skipped). `None` if there is none.
pub fn decode(intervals: impl IntoIterator<Item = Interval>) -> Option<CrashRecord> {
    let mut digits = [0u8; DIGITS];
    let mut count = 0usize;
    let mut seen_alarm = false;
    let mut pulses = 0u8;
    let mut long = false;
    let mut prev_alarm = false;
    let mut it = intervals.into_iter();
    // Skip to the first "on", then take (on, the dark after it) pairs.
    while let Some(iv) = it.next() {
        if !iv.on {
            continue;
        }
        let on_ms = iv.ms;
        // The dark after the pulse; the end of the list counts as a long dark.
        let off_ms = match it.next() {
            Some(o) if !o.on => o.ms,
            Some(_) => continue,
            None => GROUP_GAP_MS,
        };
        if off_ms < ALARM_OFF_MAX_MS || prev_alarm {
            prev_alarm = off_ms < ALARM_OFF_MAX_MS;
            seen_alarm = true;
            count = 0;
            pulses = 0;
            long = false;
            continue;
        }
        if !seen_alarm {
            continue;
        }
        if on_ms >= LONG_ON_MIN_MS {
            long = true;
        } else {
            pulses = pulses.saturating_add(1);
        }
        if off_ms < DIGIT_OFF_MIN_MS {
            continue;
        }
        // The digit ended.
        let d = if long && pulses == 0 {
            0
        } else if long {
            u8::MAX
        } else {
            pulses
        };
        let max = if count < STAGE_DIGITS { MAX_STAGE } else { 7 };
        let group_ends = off_ms >= GROUP_OFF_MIN_MS;
        pulses = 0;
        long = false;
        if d > max || group_ends != group_end(count) {
            seen_alarm = false;
            count = 0;
            continue;
        }
        digits[count] = d;
        count += 1;
        if count == DIGITS {
            let oct = |from: usize, n: usize| {
                digits[from..from + n]
                    .iter()
                    .fold(0u64, |a, &d| a << 3 | u64::from(d))
            };
            let e = STAGE_DIGITS;
            return Some(CrashRecord {
                stage: digits[0],
                ec: oct(e, EC_DIGITS) as u8,
                pc: oct(e + EC_DIGITS, PC_DIGITS),
                far: oct(e + EC_DIGITS + PC_DIGITS, FAR_DIGITS) as u32,
            });
        }
    }
    None
}
