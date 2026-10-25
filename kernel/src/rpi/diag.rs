//! Facts about the board for the boot log: what a person reading `MILKLOG.TXT` (or a screen photo) needs to
//! debug a board they cannot see into. Nothing here changes anything; every line is one `kprintln!`.
//!
//! * [`boot_report`]: the build, the exception level the firmware entered the kernel at and the one it runs at,
//!   the CPU (MIDR, MPIDR, the feature registers), the generic counter, the cache geometry, the MMU settings.
//! * [`dtb_report`]: the firmware's device tree: model, `compatible` strings, memory banks, `/chosen` (the
//!   command line and whatever else the bootloader put there), the CPUs, and the `status` / `compatible` of the
//!   nodes of the devices the drivers use.

use core::arch::asm;

use crate::kprintln;

macro_rules! sysreg {
    ($name:literal) => {{
        let v: u64;
        // SAFETY: a read of a system register that exists on every ARMv8-A core we run on.
        unsafe { asm!(concat!("mrs {}, ", $name), out(reg) v, options(nomem, nostack)) };
        v
    }};
}

unsafe extern "C" {
    static __boot_el: u64;
}

/// The build and the CPU.
pub fn boot_report() {
    // SAFETY: a plain word written once by boot.S.
    let entry_el = unsafe { core::ptr::addr_of!(__boot_el).read() } >> 2 & 3;
    let el = sysreg!("CurrentEL") >> 2 & 3;
    kprintln!(
        "boot: milkOS {} (build {}), {}",
        crate::VERSION,
        env!("MILKOS_BUILD"),
        crate::platform::CURRENT.name
    );
    kprintln!(
        "boot: entered at EL{entry_el} (the firmware's), running at EL{el}{}",
        if entry_el == 2 && el == 1 {
            " (el.S dropped to EL1)"
        } else {
            ""
        }
    );
    let midr = sysreg!("midr_el1");
    kprintln!(
        "cpu: MIDR {midr:#010x} (implementer {:#x} variant {} part {:#x} revision {}) MPIDR {:#x}",
        midr >> 24 & 0xff,
        midr >> 20 & 0xf,
        midr >> 4 & 0xfff,
        midr & 0xf,
        sysreg!("mpidr_el1")
    );
    kprintln!(
        "cpu: ID_AA64PFR0 {:#x} ISAR0 {:#x} ISAR1 {:#x} MMFR0 {:#x} MMFR1 {:#x} MMFR2 {:#x}",
        sysreg!("id_aa64pfr0_el1"),
        sysreg!("id_aa64isar0_el1"),
        sysreg!("id_aa64isar1_el1"),
        sysreg!("id_aa64mmfr0_el1"),
        sysreg!("id_aa64mmfr1_el1"),
        sysreg!("id_aa64mmfr2_el1")
    );
    let ctr = sysreg!("ctr_el0");
    kprintln!(
        "cpu: CTR {ctr:#x} (data cache line {} B, instruction cache line {} B), CNTFRQ {} Hz, SCTLR_EL1 {:#x}, TCR_EL1 {:#x}, MAIR_EL1 {:#x}",
        4usize << (ctr >> 16 & 0xf),
        4usize << (ctr & 0xf),
        sysreg!("cntfrq_el0"),
        sysreg!("sctlr_el1"),
        sysreg!("tcr_el1"),
        sysreg!("mair_el1")
    );
}

/// A property value for a log line: text if it is text, else words.
fn show(data: &[u8], out: &mut alloc::string::String) {
    use core::fmt::Write;
    let text = !data.is_empty()
        && data.last() == Some(&0)
        && data[..data.len() - 1]
            .split(|&b| b == 0)
            .all(|s| !s.is_empty() && s.iter().all(|b| (0x20..0x7f).contains(b)));
    if text {
        for (i, s) in data[..data.len() - 1].split(|&b| b == 0).enumerate() {
            if i > 0 {
                out.push('|');
            }
            out.push_str(core::str::from_utf8(s).unwrap_or("?"));
        }
    } else if data.is_empty() {
        out.push_str("(empty)");
    } else if data.len() % 4 == 0 {
        #[allow(clippy::chunks_exact_to_as_chunks)] // words of a runtime-length property
        for (i, w) in data.chunks_exact(4).take(12).enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{:#x}", u32::from_be_bytes([w[0], w[1], w[2], w[3]]));
        }
        if data.len() > 48 {
            let _ = write!(out, " ... ({} bytes)", data.len());
        }
    } else {
        let _ = write!(out, "{} bytes", data.len());
    }
}

/// Node names (prefixes) whose `compatible`, `status` and `reg` are logged: the devices the drivers use.
const INTERESTING: &[&[u8]] = &[
    b"mailbox",
    b"firmware",
    b"mmc@",
    b"mmcnr",
    b"emmc",
    b"sdhci",
    b"pcie@",
    b"ethernet@",
    b"ethernet-phy",
    b"xhci",
    b"usb@",
    b"spi@",
    b"rng@",
    b"gpio@",
    b"serial@",
    b"uart",
    b"thermal",
    b"clocks",
    b"clk-",
    b"timer",
    b"interrupt-controller",
    b"reserved-memory",
    b"framebuffer",
    b"hdmi",
    b"gpu",
    b"v3d",
    b"vc4",
    b"psci",
];

/// The firmware's device tree, in the log.
pub fn dtb_report() {
    // SAFETY: the bootloader's pointer, or 0.
    let Some(dtb) = (unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) }) else {
        kprintln!("dtb: none (x0 was {:#x})", crate::mem::boot_dtb());
        return;
    };
    kprintln!(
        "dtb: {} bytes at {:#x}",
        dtb.total_size(),
        crate::mem::boot_dtb()
    );
    let mut lines = 0usize;
    dtb.for_each_prop(|parent, node, depth, prop, data| {
        let name = core::str::from_utf8(node).unwrap_or("?");
        let pname = core::str::from_utf8(prop).unwrap_or("?");
        let chosen = node == b"chosen";
        let root = depth == 1
            && matches!(
                prop,
                b"model" | b"compatible" | b"serial-number" | b"#address-cells" | b"#size-cells"
            );
        let memory = node.starts_with(b"memory") && matches!(prop, b"reg" | b"device_type");
        let device = depth >= 2
            && matches!(
                prop,
                b"compatible" | b"status" | b"reg" | b"ranges" | b"clock-frequency"
            )
            && INTERESTING
                .iter()
                .any(|p| node.starts_with(p) || parent.starts_with(p));
        if !(chosen || root || memory || device) || lines >= 150 {
            return;
        }
        if matches!(prop, b"kaslr-seed" | b"rng-seed") {
            return; // not for a log that gets shared
        }
        let mut v = alloc::string::String::new();
        show(data, &mut v);
        kprintln!("dtb: {name}{}{pname} = {v}", if root { " /" } else { ": " });
        lines += 1;
    });
    let mut banks = [crate::dtb::MemRange { base: 0, size: 0 }; crate::dtb::MAX_BANKS];
    let n = dtb.memory(&mut banks);
    for b in &banks[..n] {
        kprintln!(
            "dtb: memory bank {:#x}..{:#x} ({} MiB)",
            b.base,
            b.base + b.size,
            b.size >> 20
        );
    }
    kprintln!(
        "dtb: kernel command line: {}",
        core::str::from_utf8(dtb.bootargs()).unwrap_or("(not text)")
    );
    let mut cpus = [crate::dtb::Cpu {
        reg: 0,
        method: crate::dtb::EnableMethod::Unknown,
        release_addr: 0,
    }; crate::dtb::MAX_CPUS];
    let n = dtb.cpus(&mut cpus);
    for c in &cpus[..n] {
        kprintln!(
            "dtb: cpu reg {:#x} enable-method {:?} release address {:#x}",
            c.reg,
            c.method,
            c.release_addr
        );
    }
    kprintln!("dtb: psci conduit {:?}", dtb.psci_conduit());
}
