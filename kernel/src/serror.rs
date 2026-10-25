//! Finding the culprit of an asynchronous external abort (SError).
//!
//! An SError is raised after the fact: a posted write to a block that is not clocked, a DMA master
//! that hit a bad address, a memory error. The CPU takes it at the next moment SErrors are unmasked,
//! which in this kernel is only the first `eret` to EL0 on that CPU (EL1 runs with PSTATE.A set the
//! whole time), long after the access. Two aids make the culprit visible (docs/raspberry-pi.md,
//! "Reading a crash"):
//!
//! * **Checkpoints** ([`checkpoint`]): `dsb sy`, unmask SError, `isb`, mask again. A pending SError
//!   is taken *there*, in EL1, and the dump says which checkpoint it was ("SError at checkpoint
//!   'genet-init'") and which one passed before it on that CPU: the access happened between the two.
//!   Labels are `area-step` words (`rpi-rng`, `sdhci`, `genet`, `sched-init`, `proc-init`,
//!   `tick`, ...); the list of call sites is the table in docs/raspberry-pi.md.
//! * **Breadcrumbs** ([`note_write`]): every MMIO accessor of the Pi drivers records the register it
//!   wrote in a per-CPU ring of the last [`RING`] writes; the dump prints them with the block names
//!   (`rpikit::soc`), so even a crash without a checkpoint in the right place shows the last
//!   devices the CPU touched.
//!
//! `milk.serror=trace` (the default on the Pi image) switches both on, `milk.serror=off` makes
//! them no-ops (one relaxed load per call). On other boards the default is off: qemu cannot raise an
//! SError, and nothing else should pay for it.

use core::arch::asm;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::kprintln;
use crate::percpu::{self, MAX_CPUS};

/// Writes remembered per CPU.
pub const RING: usize = 8;

const OFF: u8 = 0;
const TRACE: u8 = 1;
/// `milk.serror=paranoid`: a checkpoint after every recorded register write (slow; for pinning an SError to one register).
const PARANOID: u8 = 2;
/// A crash dump is being printed: no more checkpoints (the dump's own device accesses must not take the SError).
static DYING: AtomicBool = AtomicBool::new(false);

static MODE: AtomicU8 = AtomicU8::new(if crate::platform::RASPI { TRACE } else { OFF });

/// Checkpoints and breadcrumbs on? (Cheap enough to ask in the hot paths.)
#[inline]
pub fn enabled() -> bool {
    MODE.load(Ordering::Relaxed) != OFF && !DYING.load(Ordering::Relaxed)
}

/// Is the feature on at all (even if a dump is in progress)?
fn configured() -> bool {
    MODE.load(Ordering::Relaxed) != OFF
}

/// A crash dump starts: stop taking checkpoints (called by the dump).
pub fn dying() {
    DYING.store(true, Ordering::Relaxed);
}

/// A `&'static str` in one atomic (length in the top 16 bits, the low 48 bits of the address: the
/// strings are in the kernel image, whose addresses start with 0xffff), so a dump can read another
/// CPU's label without a lock and without tearing.
struct Label(AtomicU64);

impl Label {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn set(&self, s: &'static str) {
        let len = s.len().min(0xffff) as u64;
        self.0
            .store(len << 48 | (s.as_ptr() as u64 & 0xffff_ffff_ffff), Ordering::Release);
    }

    fn get(&self) -> Option<&'static str> {
        let v = self.0.load(Ordering::Acquire);
        if v == 0 {
            return None;
        }
        let ptr = (0xffffu64 << 48 | v & 0xffff_ffff_ffff) as *const u8;
        // SAFETY: `set` stored a `&'static str` of the kernel image (higher half) this way.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, (v >> 48) as usize) };
        Some(core::str::from_utf8(bytes).unwrap_or("?"))
    }
}

struct Cpu {
    /// The checkpoint being run (valid while `checking`).
    attempt: Label,
    /// The last checkpoint that passed.
    passed: Label,
    checking: AtomicBool,
    /// Inside [`checkpoint_catch`]: an SError taken here is swallowed (and noted in `caught`), not a crash.
    catching: AtomicBool,
    caught: AtomicBool,
    /// Checkpoints that passed on this CPU.
    count: AtomicU64,
    /// The last [`RING`] register writes: address (virtual), value; `writes` counts them all.
    addr: [AtomicUsize; RING],
    val: [AtomicU32; RING],
    writes: AtomicUsize,
    /// The last register read (address only).
    last_read: AtomicUsize,
    /// The last write to the ACT LED's GPIO registers (kept out of the ring: the LED code flashes and the
    /// sign of life would push everything else out of it).
    led_addr: AtomicUsize,
    led_val: AtomicU32,
}

impl Cpu {
    const fn new() -> Self {
        Self {
            attempt: Label::new(),
            passed: Label::new(),
            checking: AtomicBool::new(false),
            catching: AtomicBool::new(false),
            caught: AtomicBool::new(false),
            count: AtomicU64::new(0),
            addr: [const { AtomicUsize::new(0) }; RING],
            val: [const { AtomicU32::new(0) }; RING],
            writes: AtomicUsize::new(0),
            last_read: AtomicUsize::new(0),
            led_addr: AtomicUsize::new(0),
            led_val: AtomicU32::new(0),
        }
    }
}

static CPUS: [Cpu; MAX_CPUS] = [const { Cpu::new() }; MAX_CPUS];

/// Read `milk.serror` (call once the command line is readable, right after `mem::init`).
pub fn init() {
    let mut buf = [0u8; 16];
    let Some(f) = crate::fwcfg::find_file("opt/milkos/serror")
        .filter(|f| (f.size as usize) <= buf.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut buf[..f.size as usize]))
    else {
        if enabled() {
            kprintln!("serror: checkpoints and write breadcrumbs on (milk.serror=off turns them off)");
        }
        return;
    };
    match &buf[..f.size as usize] {
        b"trace" | b"on" | b"1" => {
            MODE.store(TRACE, Ordering::Relaxed);
            kprintln!("serror: checkpoints and write breadcrumbs on (milk.serror=trace)");
        }
        b"paranoid" => {
            MODE.store(PARANOID, Ordering::Relaxed);
            kprintln!(
                "serror: paranoid: checkpoints and write breadcrumbs on, and a checkpoint after every register write (slow)"
            );
        }
        b"off" | b"0" => {
            MODE.store(OFF, Ordering::Relaxed);
            kprintln!("serror: checkpoints and write breadcrumbs off (milk.serror=off)");
        }
        other => kprintln!(
            "serror: milk.serror={:?} is not trace, paranoid or off, keeping the default",
            core::str::from_utf8(other).unwrap_or("?")
        ),
    }
}

/// Time a thousand checkpoints and say what one costs (needs the timer).
pub fn measure() {
    if !enabled() {
        return;
    }
    let t0 = crate::timer::now_ns();
    for _ in 0..1000 {
        checkpoint("serror-bench");
    }
    let ns = crate::timer::now_ns().saturating_sub(t0);
    kprintln!("serror: a checkpoint costs {} ns (1000 timed)", ns / 1000);
}

/// Take a pending SError now, here, instead of whenever it would be unmasked next. If one is
/// pending it fires at the `isb` (vector 7, the dump names `label`); if not, a few cycles later
/// this returns. `label` should say where in the code this is (`area-step`).
///
/// The access that raised an SError happened after the previous checkpoint that passed *on the
/// same CPU* and before this one.
#[inline(never)]
pub fn checkpoint(label: &'static str) {
    if !enabled() {
        return;
    }
    // IRQs off so the CPU number and the label pair stay together.
    let flags = crate::sync::irq_save();
    let c = &CPUS[percpu::id()];
    c.attempt.set(label);
    c.checking.store(true, Ordering::Relaxed);
    // SAFETY: barriers and a PSTATE.A toggle. The SError, if one is pending, is taken between the
    // `msr daifclr` and the `isb` completing, on this stack, and ends in the crash dump.
    unsafe {
        asm!(
            "dsb sy",
            "msr daifclr, #4",
            "isb",
            "msr daifset, #4",
            options(nostack)
        );
    }
    c.checking.store(false, Ordering::Relaxed);
    c.passed.set(label);
    c.count.fetch_add(1, Ordering::Relaxed);
    crate::sync::irq_restore(flags);
}

/// A checkpoint for a driver that can live without its device: like [`checkpoint`], but a pending SError is taken
/// and swallowed here (see [`swallow`]) instead of ending in the crash dump. Returns `false` if there was one (or
/// more: all pending ones are drained), so the driver can log which accesses it did since its previous checkpoint
/// and give up. Works whatever `milk.serror=` says.
#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
#[inline(never)]
pub fn checkpoint_catch(label: &'static str) -> bool {
    let flags = crate::sync::irq_save();
    let c = &CPUS[percpu::id()];
    let mut clean = true;
    // A second SError can be pending behind the first (it is taken once the first one's handler is done): go again until quiet.
    for _ in 0..4 {
        c.caught.store(false, Ordering::Relaxed);
        c.catching.store(true, Ordering::Relaxed);
        // SAFETY: as in `checkpoint`; the vector handler returns here when `catching` is set.
        unsafe {
            asm!(
                "dsb sy",
                "msr daifclr, #4",
                "isb",
                "msr daifset, #4",
                options(nostack)
            );
        }
        c.catching.store(false, Ordering::Relaxed);
        if !c.caught.swap(false, Ordering::Relaxed) {
            break;
        }
        clean = false;
    }
    if clean {
        c.passed.set(label);
        c.count.fetch_add(1, Ordering::Relaxed);
    }
    crate::sync::irq_restore(flags);
    clean
}

/// The SError vector (current EL) calls this first: if this CPU is inside [`checkpoint_catch`], note it, keep SError masked
/// on the way back (`frame.spsr`) and return true (no crash).
pub fn swallow(frame: &mut crate::exceptions::TrapFrame, vector: usize) -> bool {
    if !(4..8).contains(&vector) {
        return false;
    }
    let c = &CPUS[percpu::id()];
    if !c.catching.load(Ordering::Relaxed) {
        return false;
    }
    c.caught.store(true, Ordering::Relaxed);
    frame.spsr |= 1 << 8; // PSTATE.A
    true
}

/// Remember a register write on this CPU (virtual address and value), for the dump. Hold the returned guard
/// across the write itself (`let _c = write_guard(addr, v); write_volatile(..)`): in `milk.serror=paranoid` its
/// drop is a checkpoint right after the write, so a bus error for that register is taken before anything else
/// happens and the newest entry of the list is the register.
#[inline]
#[must_use]
pub fn write_guard(addr: usize, val: u32) -> WriteGuard {
    if !enabled() {
        return WriteGuard(false);
    }
    let c = &CPUS[percpu::id()];
    let i = c.writes.fetch_add(1, Ordering::Relaxed) % RING;
    c.addr[i].store(addr, Ordering::Relaxed);
    c.val[i].store(val, Ordering::Relaxed);
    WriteGuard(MODE.load(Ordering::Relaxed) == PARANOID)
}

/// See [`write_guard`].
pub struct WriteGuard(bool);

impl Drop for WriteGuard {
    #[inline]
    fn drop(&mut self) {
        if self.0 {
            checkpoint("mmio-write");
        }
    }
}

/// Record a register write without a guard (the tests).
pub fn note_write(addr: usize, val: u32) {
    drop(write_guard(addr, val));
}

/// Remember a write to the ACT LED's GPIO registers (its own slot, not the ring).
#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
#[inline]
pub fn note_led(addr: usize, val: u32) {
    if enabled() {
        let c = &CPUS[percpu::id()];
        c.led_addr.store(addr, Ordering::Relaxed);
        c.led_val.store(val, Ordering::Relaxed);
    }
}

/// Remember a register read on this CPU (the last one only).
#[cfg_attr(not(feature = "raspi"), allow(dead_code))]
#[inline]
pub fn note_read(addr: usize) {
    if enabled() {
        CPUS[percpu::id()].last_read.store(addr, Ordering::Relaxed);
    }
}

/// The write that is `back` entries old on `cpu` (0 = the newest): virtual address and value.
fn write_back(cpu: usize, back: usize) -> Option<(usize, u32)> {
    let c = &CPUS[cpu];
    let n = c.writes.load(Ordering::Relaxed);
    if back >= n.min(RING) {
        return None;
    }
    let i = (n - 1 - back) % RING;
    Some((c.addr[i].load(Ordering::Relaxed), c.val[i].load(Ordering::Relaxed)))
}

/// A register address as `block+0xoff` (or the plain address).
struct Reg(usize);

impl core::fmt::Display for Reg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let pa = if self.0 >= crate::mmu::KERNEL_BASE {
            (self.0 - crate::mmu::KERNEL_BASE) as u64
        } else {
            self.0 as u64
        };
        match rpikit::soc::block(pa) {
            Some((name, off)) => write!(f, "{pa:#x} {name}+{off:#x}"),
            None => write!(f, "{pa:#x}"),
        }
    }
}

/// Which checkpoints this CPU has seen, for a dump: `(passed, attempt, count, checking)`.
fn state(cpu: usize) -> (Option<&'static str>, Option<&'static str>, u64, bool) {
    let c = &CPUS[cpu];
    (
        c.passed.get(),
        c.attempt.get(),
        c.count.load(Ordering::Relaxed),
        c.checking.load(Ordering::Relaxed),
    )
}

/// The attribution part of a crash dump on `cpu`: whether the exception (`vector`, 0..15) is an SError
/// and where it was taken, the last checkpoints of every CPU, and the register writes that preceded.
pub fn report(cpu: usize, vector: usize) {
    let is_serror = vector % 4 == 3;
    if !configured() {
        if is_serror {
            kprintln!(
                "  SError checkpoints are off (milk.serror=off): no attribution, see the ELR and the ESR"
            );
        }
        return;
    }
    let (passed, attempt, count, checking) = state(cpu);
    if is_serror {
        match (checking && (4..8).contains(&vector), attempt, passed) {
            (true, Some(at), prev) => {
                kprintln!("  SError at checkpoint '{at}' on cpu {cpu}");
                if at == "mmio-write" {
                    if let Some((addr, val)) = write_back(cpu, 0) {
                        kprintln!(
                            "  paranoid mode: the bus error came back from the write to {} <- {val:#010x}",
                            Reg(addr)
                        );
                    }
                }
                match prev {
                    Some(p) => kprintln!(
                        "  the access that caused it happened after checkpoint '{p}' passed and before '{at}' ({count} passed on this cpu)"
                    ),
                    None => kprintln!(
                        "  the access that caused it happened before '{at}', the first checkpoint on this cpu"
                    ),
                }
            }
            (_, _, Some(p)) => {
                kprintln!(
                    "  SError taken outside a checkpoint (the first eret to EL0 on cpu {cpu}, or a vector from lower EL)"
                );
                kprintln!(
                    "  the access that caused it happened after checkpoint '{p}' passed on this cpu and before this point ({count} passed)"
                );
            }
            _ => kprintln!(
                "  SError taken before any checkpoint passed on cpu {cpu}: the access was earlier than the first one"
            ),
        }
    } else if let Some(p) = passed {
        kprintln!("  last SError checkpoint that passed on cpu {cpu}: '{p}' ({count} passed)");
    }
    // Every CPU's last checkpoint (the culprit may be another CPU's, if the SError was routed here).
    for other in percpu::online() {
        if other == cpu {
            continue;
        }
        let (p, a, n, chk) = state(other);
        if let Some(p) = p {
            if chk {
                kprintln!(
                    "  cpu {other}: last checkpoint passed '{p}' ({n} passed), now inside '{}'",
                    a.unwrap_or("?")
                );
            } else {
                kprintln!("  cpu {other}: last checkpoint passed '{p}' ({n} passed)");
            }
        }
    }
    // The register writes.
    let mut any = false;
    for back in (0..RING).rev() {
        if let Some((addr, val)) = write_back(cpu, back) {
            if !any {
                kprintln!("  last {} register writes on cpu {cpu} (oldest first):", RING);
                any = true;
            }
            kprintln!("    {} <- {val:#010x}", Reg(addr));
        }
    }
    if !any {
        kprintln!("  no register writes recorded on cpu {cpu}");
    }
    let la = CPUS[cpu].led_addr.load(Ordering::Relaxed);
    if la != 0 {
        kprintln!(
            "  last ACT LED write on cpu {cpu} (not in the list above): {} <- {:#010x}",
            Reg(la),
            CPUS[cpu].led_val.load(Ordering::Relaxed)
        );
    }
    let r = CPUS[cpu].last_read.load(Ordering::Relaxed);
    if r != 0 {
        kprintln!("  last register read on cpu {cpu}: {}", Reg(r));
    }
    for other in percpu::online() {
        if other == cpu {
            continue;
        }
        if let Some((addr, val)) = write_back(other, 0) {
            kprintln!("  cpu {other}'s newest register write: {} <- {val:#010x}", Reg(addr));
        }
    }
}

/// The closing summary of a dump (the crash screen shows the end of the log): one or two lines that
/// carry the attribution, so a photo of the screen is enough.
pub fn summary(cpu: usize, vector: usize) {
    if !configured() {
        return;
    }
    let (passed, attempt, _count, checking) = state(cpu);
    let is_serror = vector % 4 == 3;
    let mut line = heapless_line::Line::new();
    let _ = write!(line, "  where: ");
    if is_serror && checking && (4..8).contains(&vector) {
        let _ = write!(
            line,
            "SError AT checkpoint '{}', after '{}'",
            attempt.unwrap_or("?"),
            passed.unwrap_or("(none)")
        );
    } else if is_serror {
        let _ = write!(
            line,
            "SError outside a checkpoint, after '{}'",
            passed.unwrap_or("(none)")
        );
    } else {
        let _ = write!(line, "last checkpoint '{}'", passed.unwrap_or("(none)"));
    }
    kprintln!("{}", line.as_str());
    let mut line = heapless_line::Line::new();
    let mut n = 0;
    for back in 0..3 {
        if let Some((addr, val)) = write_back(cpu, back) {
            let _ = write!(line, "{}{} <- {val:#x}", if n > 0 { "; " } else { "" }, Reg(addr));
            n += 1;
        }
    }
    if n > 0 {
        kprintln!("  newest writes (newest first): {}", line.as_str());
    }
}

/// A fixed-size line buffer (a crash dump must not allocate).
mod heapless_line {
    pub struct Line {
        buf: [u8; 192],
        len: usize,
    }

    impl Line {
        pub const fn new() -> Self {
            Self {
                buf: [0; 192],
                len: 0,
            }
        }

        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
        }
    }

    impl core::fmt::Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let room = self.buf.len() - self.len;
            // Cut on a character boundary if the line is full.
            let mut n = s.len().min(room);
            while !s.is_char_boundary(n) {
                n -= 1;
            }
            self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
            self.len += n;
            Ok(())
        }
    }
}

/// The CPU's RAS level (ID_AA64PFR0_EL1.RAS) and the error records that are cheap to read: the number of
/// records and, for the first few, status / address / misc0. Prints nothing on a CPU without RAS (the
/// Cortex-A72 of the Pi 4). Also DISR_EL1, the deferred SError status.
pub fn print_ras(pfr0: u64) {
    if rpikit::esr::pfr0_ras(pfr0) == 0 {
        return;
    }
    let (errid, disr): (u64, u64);
    // SAFETY: RAS registers exist when ID_AA64PFR0_EL1.RAS != 0. ERRIDR_EL1 = S3_0_C5_C3_0,
    // DISR_EL1 = S3_0_C12_C1_1.
    unsafe {
        asm!("mrs {}, s3_0_c5_c3_0", out(reg) errid, options(nomem, nostack));
        asm!("mrs {}, s3_0_c12_c1_1", out(reg) disr, options(nomem, nostack));
    }
    let n = (errid & 0xffff) as usize;
    kprintln!("  RAS: ERRIDR_EL1 = {errid:#x} ({n} error record(s)), DISR_EL1 = {disr:#x}");
    for i in 0..n.min(4) {
        let (status, addr, misc0): (u64, u64, u64);
        // SAFETY: select record i (ERRSELR_EL1 = S3_0_C5_C3_1), then ERXSTATUS_EL1 = S3_0_C5_C4_2,
        // ERXADDR_EL1 = S3_0_C5_C4_3, ERXMISC0_EL1 = S3_0_C5_C5_0.
        unsafe {
            asm!("msr s3_0_c5_c3_1, {}", "isb", in(reg) i as u64, options(nomem, nostack));
            asm!("mrs {}, s3_0_c5_c4_2", out(reg) status, options(nomem, nostack));
            asm!("mrs {}, s3_0_c5_c4_3", out(reg) addr, options(nomem, nostack));
            asm!("mrs {}, s3_0_c5_c5_0", out(reg) misc0, options(nomem, nostack));
        }
        kprintln!("  RAS record {i}: ERXSTATUS = {status:#x} ERXADDR = {addr:#x} ERXMISC0 = {misc0:#x}");
    }
}

/// Raise a deliberate SError-shaped exception for the tests (`milk.crashtest=serror`): a fake frame with the
/// ESR of an SError, run through the real crash path, as if it were taken at a checkpoint. qemu cannot
/// raise a real one. The breadcrumbs and the checkpoint state it prints are real.
pub fn simulate() -> ! {
    // Some breadcrumbs, from a block that exists on the Pi.
    let base = crate::mmu::phys_to_virt(0xfd58_0000);
    for (off, v) in [(0x2000usize, 1u32), (0x2004, 0xdead_0000), (0x4014, 0), (0x4018, 3)] {
        note_write(base + off, v);
    }
    let cpu = percpu::id();
    CPUS[cpu].passed.set("crashtest-before");
    CPUS[cpu].attempt.set("crashtest-serror");
    CPUS[cpu].checking.store(true, Ordering::Relaxed);
    let mut frame = crate::exceptions::TrapFrame::zeroed();
    frame.esr = rpikit::esr::serror_esr(0);
    frame.elr = simulate as *const () as u64;
    frame.spsr = 0x3c5;
    // Vector 7: current EL, SP_ELx, SError.
    crate::exceptions::crash(&frame, 7, "SError")
}
