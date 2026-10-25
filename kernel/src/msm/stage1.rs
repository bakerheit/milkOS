//! Stage 1 of the Pixel 2 port, in the order a person watching the phone sees it:
//!
//! 1. the tree the bootloader handed over is read (no memory, no device touched) and the
//!    framebuffer's place found; four labelled colour blocks, a white frame and two diagonals are
//!    drawn into it at once and held for `msm.hold=` seconds (5), so a wrong pixel order, a wrong
//!    row length or a cropped picture is obvious;
//! 2. RAM is set up from the tree (the reserved regions taken out), and the boot log goes on the
//!    screen as a text console: what the tree said, what the CPU says about itself, what the
//!    interrupt controller says (read only);
//! 3. a counter that goes up once a second and a marker that sweeps across the green band at the
//!    top, so a frozen phone is recognisable. The kernel polls the counter: no interrupt is enabled.
//!
//! Nothing here powers the phone off, resets it or writes to storage. The only calls to the secure
//! world are the optional `msm.psci=1` PSCI_VERSION, and the display controller is only written to
//! with `msm.kick=ctlN`.

use core::fmt::Write as _;

use crate::dtb::{Cpu, Dtb, MAX_CPUS};
use crate::{fbcon::Console, kprintln, mem, mmu, platform, sink, timer, VERSION};
use rpikit::msm::{CrashTest, Hold, Opts};

unsafe extern "C" {
    static __boot_el: u64;
    static __boot_entry: u64;
}

/// A line built on the stack (the log lines that list things).
struct Line {
    b: [u8; 200],
    n: usize,
}

impl Line {
    fn new() -> Line {
        Line { b: [0; 200], n: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.b[..self.n]).unwrap_or("?")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.b.len() - self.n;
        let take = s.len().min(room);
        self.b[self.n..self.n + take].copy_from_slice(&s.as_bytes()[..take]);
        self.n += take;
        Ok(())
    }
}

/// The text of a NUL-terminated device tree string property.
fn text(v: Option<&[u8]>) -> &str {
    let v = v.unwrap_or(b"");
    let v = &v[..v.iter().position(|&c| c == 0).unwrap_or(v.len())];
    core::str::from_utf8(v).unwrap_or("?")
}

fn sysreg_el() -> u64 {
    // SAFETY: plain load of a word boot.S wrote once.
    unsafe { core::ptr::addr_of!(__boot_el).read() }
}

fn midr_mpidr() -> (u64, u64) {
    let (a, b): (u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("mrs {}, midr_el1", "mrs {}, mpidr_el1", out(reg) a, out(reg) b, options(nomem, nostack))
    };
    (a, b)
}

/// PSCI_VERSION (function 0x84000000) through the secure monitor: `Some(version)`, or `None` for
/// "not supported".
fn psci_version() -> Option<u32> {
    let r: u64;
    // SAFETY: the SMC calling convention: w0 = function, the result in w0; x1 to x3 may change.
    unsafe {
        core::arch::asm!(
            "smc #0",
            inout("x0") 0x8400_0000u64 => r,
            inout("x1") 0u64 => _,
            inout("x2") 0u64 => _,
            inout("x3") 0u64 => _,
            options(nomem, nostack)
        )
    };
    let r = r as u32;
    (r as i32 >= 0).then_some(r)
}

/// Wait until the counter reaches `deadline` (no interrupts, no sleeping), petting the watchdog
/// a few times a second if asked to.
fn wait_until(deadline: u64, wdt: Option<usize>, freq: u64) {
    let mut next = 0;
    loop {
        let now = timer::counter();
        if now >= deadline {
            break;
        }
        if let Some(b) = wdt
            && now >= next
        {
            crate::msm::wdt::pet(b);
            next = now + freq / 4;
        }
        core::hint::spin_loop();
    }
}

pub fn run() -> ! {
    let freq = timer::init_polled();
    // SAFETY: the bootloader's pointer (boot.S kept it), or 0.
    let dtb = unsafe { Dtb::from_addr(mem::boot_dtb_ptr()) };
    let cmdline = dtb.as_ref().map_or(&b""[..], |d| d.bootargs());
    let (opts, warnings, nwarn) = Opts::parse(cmdline);

    // 1. The screen, before anything else is set up.
    // The watchdog, if we were asked to look after it: before anything slow.
    let wdt_base = crate::msm::wdt::locate(dtb.as_ref());
    let pet = if opts.wdt_pet { wdt_base } else { None };
    if let Some(b) = pet {
        crate::msm::wdt::pet(b);
    }
    let found = crate::msm::fb::locate(dtb.as_ref(), &opts);
    let mut drawn = false;
    let mut geometry_err = None;
    if let Some(f) = &found {
        match crate::msm::fb::setup(f, &opts) {
            Ok(_) if crate::msm::fb::boot_mapped(f.pa, f.size.max(1)) => {
                crate::msm::fb::draw_pattern();
                crate::msm::fb::draw_labels();
                drawn = true;
                announce_pattern();
            }
            Ok(_) => {}
            Err(why) => geometry_err = Some(why),
        }
    }
    let shown_at = timer::counter();

    // 2. Memory, then the rest.
    crate::percpu::init_boot();
    mem::init();
    #[cfg(feature = "msm-virt-test")]
    if !crate::msm::fb::ready() {
        // qemu has no splash region: ramfb, 1080x1920 like the phone.
        if let Some(_) = crate::ramfb::init() {
            if let Some(s) = crate::ramfb::set_mode(opts.width, opts.height) {
                let _ = s;
                if let Some(p) = crate::ramfb::framebuffer() {
                    crate::msm::fb::setup_ram(p as usize, opts.width, opts.height, &opts);
                }
            }
        }
    }
    if crate::msm::fb::ready() && !drawn {
        crate::msm::fb::draw_pattern();
        crate::msm::fb::draw_labels();
        drawn = true;
        announce_pattern();
    }
    // The pattern stays up for its time, counted from when it was drawn.
    match opts.hold {
        Hold::Seconds(s) => wait_until(shown_at + s as u64 * freq, pet, freq),
        Hold::Forever if drawn => loop {
            wait_until(timer::counter() + freq, pet, freq);
        },
        Hold::Forever => {}
    }

    if opts.crashtest == Some(CrashTest::Early) {
        panic!("msm.crashtest=early: stopping before the console");
    }

    // 3. The console.
    let mut console_rows = 0;
    if let Some(mut c) = crate::msm::fb::console() {
        // What was logged before the screen existed.
        crate::klog::read_since(0, |bytes| {
            for chunk in bytes.utf8_chunks() {
                c.write_str(chunk.valid());
            }
        });
        c.flush();
        console_rows = c.size().1;
        crate::msm::fb::band(&mut c, 0);
        sink::install_screen(c);
    }
    report(dtb.as_ref(), &opts, found.as_ref().map(|f| (f.pa, f.size, f.source)), geometry_err, &warnings[..nwarn], freq);
    crate::msm::wdt::report(wdt_base);
    if pet.is_some() {
        kprintln!("wdt: petting it once a second (msm.wdt=pet)");
    }
    match opts.crashtest {
        Some(CrashTest::Panic) => panic!("msm.crashtest=panic: stopping on purpose"),
        Some(CrashTest::Abort) => {
            kprintln!("crashtest: storing to an unmapped address");
            // SAFETY: not safe at all, on purpose: the address is in no mapping (the exception dump follows).
            unsafe { core::ptr::write_volatile(0x0000_dead_0000usize as *mut u32, 1) };
        }
        _ => {}
    }
    idle(dtb.as_ref(), &opts, console_rows, freq, pet)
}

/// The line in the log (and on the console, which replays it) that says the pattern was drawn and how.
fn announce_pattern() {
    if let Some(g) = crate::msm::fb::geometry() {
        let (pa, size) = crate::msm::fb::region();
        kprintln!(
            "pattern: {}x{} {} stride {} drawn at {pa:#x}+{size:#x}",
            g.width,
            g.height,
            g.fmt.name(),
            g.stride
        );
    }
}

fn report(
    dtb: Option<&Dtb>,
    opts: &Opts,
    fb: Option<(usize, usize, &str)>,
    geometry_err: Option<&str>,
    warnings: &[&str],
    freq: u64,
) {
    kprintln!("milkOS {VERSION}: {}", platform::CURRENT.name);
    kprintln!("stage 1: a screen and a log, no UART, no storage");
    let (midr, mpidr) = midr_mpidr();
    kprintln!(
        "cpu: EL{} at entry, MIDR {midr:#x}, MPIDR {:#x}",
        sysreg_el() >> 2,
        mpidr & 0xff_00ff_ffff
    );
    let (kstart, kend) = mem::kernel_range();
    // SAFETY: plain load of a word boot.S wrote.
    let entry = unsafe { core::ptr::addr_of!(__boot_entry).read() } as usize;
    if entry == kstart {
        kprintln!("image: {kstart:#x}..{kend:#x}, entered right there");
    } else {
        kprintln!("image: {kstart:#x}..{kend:#x}, entered at {entry:#x}");
        kprintln!("image: boot.S moved it to where it is linked");
    }
    match dtb {
        Some(d) => {
            kprintln!("dtb: {:#x}, {} bytes", mmu::virt_to_phys(d.base()), d.total_size());
            kprintln!("dtb: model {}", text(d.find(b"", b"model")));
            let mut l = Line::new();
            for s in d.root_compatible().split(|&b| b == 0).filter(|s| !s.is_empty()) {
                let _ = write!(l, "{} ", core::str::from_utf8(s).unwrap_or("?"));
            }
            kprintln!("dtb: compatible {}", l.as_str());
            if let Some(v) = d.find(b"", b"qcom,msm-id") {
                if v.len() >= 8 {
                    let w = |i: usize| u32::from_be_bytes([v[i], v[i + 1], v[i + 2], v[i + 3]]);
                    kprintln!("dtb: qcom,msm-id {:#x} {:#x}", w(0), w(4));
                }
            }
            let args = core::str::from_utf8(cmdline_head(d.bootargs())).unwrap_or("?");
            kprintln!("cmdline: {args}");
        }
        None => kprintln!("dtb: none usable (x0 = {:#x}), the board's defaults are used", mem::boot_dtb()),
    }
    // RAM.
    let listed = crate::msm::mem::LISTED.load(core::sync::atomic::Ordering::Relaxed);
    let reserved = crate::msm::mem::RESERVED.load(core::sync::atomic::Ordering::Relaxed);
    let rbytes = crate::msm::mem::RESERVED_BYTES.load(core::sync::atomic::Ordering::Relaxed);
    kprintln!(
        "ram: tree lists {listed} range(s); {reserved} reserved regions ({} MiB) taken out",
        rbytes >> 20
    );
    // CPUs.
    if let Some(d) = dtb {
        let mut cpus = [Cpu {
            reg: 0,
            method: crate::dtb::EnableMethod::Unknown,
            release_addr: 0,
        }; MAX_CPUS];
        let n = d.cpus(&mut cpus);
        let mut l = Line::new();
        for c in &cpus[..n] {
            let _ = write!(l, "{:x} ", c.reg);
        }
        kprintln!("cpus: {n} in the tree ({}), 1 running", match d.psci_conduit() {
            Some(crate::dtb::Conduit::Smc) => "psci/smc",
            Some(crate::dtb::Conduit::Hvc) => "psci/hvc",
            None => "no psci node",
        });
        kprintln!("cpus: mpidr {}", l.as_str());
    }
    let raw = timer::cntfrq_raw();
    if raw == 0 {
        kprintln!("timer: CNTFRQ is zero, assuming {freq} Hz");
    } else {
        kprintln!("timer: CNTFRQ {raw} Hz, polled, no interrupt");
    }
    // The screen.
    match (crate::msm::fb::geometry(), fb) {
        (Some(g), Some((pa, size, source))) => {
            kprintln!(
                "screen: {}x{} {} stride {} B (assumed)",
                g.width,
                g.height,
                g.fmt.name(),
                g.stride
            );
            kprintln!("screen: {pa:#x}+{size:#x} from {source}");
        }
        (Some(g), None) => kprintln!(
            "screen: {}x{} XRGB in RAM (qemu ramfb), stride {} B",
            g.width,
            g.height,
            g.stride
        ),
        (None, _) => kprintln!("screen: no framebuffer ({})", geometry_err.unwrap_or("none found")),
    }
    if let Some(d) = dtb {
        let mut any = false;
        d.each_reserved_child(|name, base, size| {
            if name.starts_with(b"ramoops") || name.starts_with(b"alt_ramoops") {
                kprintln!(
                    "ramoops: {} {base:#x}+{size:#x}",
                    core::str::from_utf8(name).unwrap_or("?")
                );
                any = true;
            }
        });
        if !any {
            kprintln!("ramoops: no region in the tree");
        }
    }
    crate::msm::gicv3::probe(dtb);
    if opts.psci {
        match psci_version() {
            Some(v) => kprintln!("psci: version {}.{}", v >> 16, v & 0xffff),
            None => kprintln!("psci: PSCI_VERSION is not supported"),
        }
    }
    for w in warnings {
        kprintln!("option: {w}");
    }
    kprintln!("options: fmt {} hold {:?} scale {}", opts.fmt.name(), opts.hold, opts.scale);
}

/// The first part of the command line that fits a few screen lines.
fn cmdline_head(a: &[u8]) -> &[u8] {
    &a[..a.len().min(180)]
}

/// The calm end: once a second the counter on the last console row and the marker on the band move.
fn idle(dtb: Option<&Dtb>, opts: &Opts, rows: usize, freq: u64, pet: Option<usize>) -> ! {
    let kick = match (opts.kick, dtb) {
        (Some(i), Some(d)) => crate::msm::fb::kick_probe(d, i),
        _ => None,
    };
    if let (true, Some(d)) = (opts.mdp_dump, dtb) {
        crate::msm::fb::mdp_dump(d, freq);
    }
    kprintln!("idle: counter on the bottom line, marker on the green band");
    // Keep the last row free for the counter.
    for _ in 0..2 {
        kprintln!("");
    }
    let start = timer::counter();
    let mut next = start;
    let mut kick_at = start;
    let mut secs: u64 = 0;
    loop {
        let now = timer::counter();
        if let Some(base) = kick
            && now >= kick_at
        {
            crate::msm::fb::kick(base);
            kick_at = now + freq / 10;
        }
        if now >= next {
            if let Some(b) = pet {
                crate::msm::wdt::pet(b);
            }
            update(secs, rows);
            secs += 1;
            next += freq;
        }
        core::hint::spin_loop();
    }
}

fn update(secs: u64, rows: usize) {
    let mut l = Line::new();
    let _ = write!(l, "\x1b[{};1H\x1b[Kalive {secs} s  counter {}", rows.max(1), timer::counter());
    sink::paint(|c: &mut Console| {
        c.write_str(l.as_str());
        c.flush();
        crate::msm::fb::band(c, secs as usize);
    });
}
