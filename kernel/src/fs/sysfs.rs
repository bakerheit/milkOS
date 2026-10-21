//! sysfs at `/sys`: the hardware the kernel was handed, as files.
//!
//! `/sys/devices/platform/<addr>.<name>/` has one directory per top-level device-tree node
//! that describes a device (`compatible` or `reg`), with `compatible`, `reg`, `interrupts`
//! and `driver` (which milkOS subsystem, if any, drives it). virtio-mmio slots with
//! nothing plugged in are left out; populated ones get a `virtio_device` file.
//! `/sys/kernel/version` mirrors `/proc/version`.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr::read_volatile;

use super::errno::Result;
use super::procfs;
use super::synth::SynthFs;
use crate::mem;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// A top-level device-tree node.
struct DtNode {
    name: String,
    compatible: Vec<String>,
    reg: Vec<u32>,
    interrupts: Vec<u32>,
}

fn be32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn cstr(b: &[u8], off: usize) -> Option<&str> {
    let rest = b.get(off..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    core::str::from_utf8(&rest[..end]).ok()
}

/// Top-level nodes of the boot device tree (empty if there is none).
fn dt_nodes() -> Vec<DtNode> {
    let base = mem::boot_dtb();
    if base == 0 {
        return Vec::new();
    }
    // SAFETY: boot DTB is reserved RAM, identity-mapped; header read first for the size.
    let hdr = unsafe { core::slice::from_raw_parts(base as *const u8, 40) };
    if be32(hdr, 0) != Some(FDT_MAGIC) {
        return Vec::new();
    }
    let total = be32(hdr, 4).unwrap_or(0) as usize;
    let blob = unsafe { core::slice::from_raw_parts(base as *const u8, total) };
    let (Some(st_off), Some(str_off), Some(st_size)) =
        (be32(blob, 8), be32(blob, 12), be32(blob, 36))
    else {
        return Vec::new();
    };
    let (st_off, str_off, st_end) = (
        st_off as usize,
        str_off as usize,
        (st_off + st_size) as usize,
    );
    let cells = |data: &[u8]| -> Vec<u32> {
        data.as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes(*c))
            .collect()
    };

    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur: Option<DtNode> = None;
    let mut p = st_off;
    while p + 4 <= st_end {
        let Some(tok) = be32(blob, p) else { break };
        p += 4;
        match tok {
            FDT_BEGIN_NODE => {
                let name = cstr(blob, p).unwrap_or("");
                p = (p + name.len() + 1).next_multiple_of(4);
                depth += 1;
                if depth == 2 {
                    cur = Some(DtNode {
                        name: String::from(name),
                        compatible: Vec::new(),
                        reg: Vec::new(),
                        interrupts: Vec::new(),
                    });
                }
            }
            FDT_END_NODE => {
                if depth == 2
                    && let Some(n) = cur.take()
                {
                    out.push(n);
                }
                depth = depth.saturating_sub(1);
            }
            FDT_PROP => {
                let (Some(len), Some(nameoff)) = (be32(blob, p), be32(blob, p + 4)) else {
                    break;
                };
                let data_off = p + 8;
                let Some(data) = blob.get(data_off..data_off + len as usize) else {
                    break;
                };
                p = (data_off + len as usize).next_multiple_of(4);
                if depth != 2 {
                    continue;
                }
                let Some(n) = cur.as_mut() else { continue };
                match cstr(blob, str_off + nameoff as usize).unwrap_or("") {
                    "compatible" => {
                        n.compatible = data
                            .split(|&c| c == 0)
                            .filter(|s| !s.is_empty())
                            .map(|s| String::from_utf8_lossy(s).into_owned())
                            .collect()
                    }
                    "reg" => n.reg = cells(data),
                    "interrupts" => n.interrupts = cells(data),
                    _ => {}
                }
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => break,
        }
    }
    out
}

/// Which milkOS code drives a device with this `compatible`, if any.
fn driver_for(compat: &[String]) -> &'static str {
    for c in compat {
        match c.as_str() {
            "arm,pl011" => return "uart (kernel/src/uart.rs)",
            "arm,cortex-a15-gic" => return "gic (kernel/src/gic.rs)",
            "arm,armv8-timer" => return "timer (kernel/src/timer.rs)",
            "qemu,fw-cfg-mmio" => return "fwcfg (kernel/src/fwcfg.rs)",
            "arm,pl031" => return "fs::clock (read once at boot)",
            _ => {}
        }
    }
    "none"
}

fn virtio_name(id: u32) -> &'static str {
    match id {
        1 => "net",
        2 => "block",
        3 => "console",
        4 => "rng",
        16 => "gpu",
        18 => "input",
        19 => "vsock",
        _ => "other",
    }
}

/// `(base, size)` pairs from a `reg` with 2 address + 2 size cells (qemu virt's root).
fn reg_pairs(reg: &[u32]) -> Vec<(u64, u64)> {
    reg.as_chunks::<4>()
        .0
        .iter()
        .map(|c| {
            (
                (c[0] as u64) << 32 | c[1] as u64,
                (c[2] as u64) << 32 | c[3] as u64,
            )
        })
        .collect()
}

/// Build sysfs and mount it on `/sys`.
pub fn init() -> Result<()> {
    let fs = SynthFs::new("sysfs");
    let root = fs.root_dir();
    let platform = root.add_dir("devices").add_dir("platform");
    let mut nodes = dt_nodes();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    for n in nodes {
        if n.compatible.is_empty() && n.reg.is_empty() {
            continue;
        }
        let regs = reg_pairs(&n.reg);
        let virtio = n.compatible.iter().any(|c| c == "virtio,mmio");
        let mut virtio_id = 0;
        if virtio && let Some(&(base, _)) = regs.first() {
            // MagicValue "virt" @0, DeviceID @8; both side-effect-free reads.
            // SAFETY: virtio-mmio window is below 1 GiB, mapped as device memory.
            let magic = unsafe { read_volatile(base as *const u32) };
            virtio_id = if magic == 0x7472_6976 {
                unsafe { read_volatile((base + 8) as *const u32) }
            } else {
                0
            };
            if virtio_id == 0 {
                continue; // empty slot
            }
        }
        let dirname = match n.name.split_once('@') {
            Some((name, addr)) => format!("{addr}.{name}"),
            None => n.name.clone(),
        };
        let d = platform.add_dir(&dirname);
        let mut compat = n.compatible.join("\n");
        compat.push('\n');
        d.add_text("compatible", compat);
        let mut reg = String::new();
        for (b, s) in &regs {
            let _ = writeln!(reg, "{b:#x} {s:#x}");
        }
        d.add_text("reg", reg);
        if !n.interrupts.is_empty() {
            let mut irq = String::new();
            for c in n.interrupts.as_chunks::<3>().0 {
                let intid = match c[0] {
                    0 => c[1] + 32,
                    _ => c[1] + 16,
                };
                let kind = if c[0] == 0 { "SPI" } else { "PPI" };
                let _ = writeln!(irq, "{kind} {} (INTID {intid}) flags {:#x}", c[1], c[2]);
            }
            d.add_text("interrupts", irq);
        }
        let driver = if virtio {
            d.add_text(
                "virtio_device",
                format!("{virtio_id} ({})\n", virtio_name(virtio_id)),
            );
            "virtio-mmio"
        } else {
            driver_for(&n.compatible)
        };
        d.add_text("driver", format!("{driver}\n"));
    }
    root.add_dir("kernel")
        .add_file("version", || String::from(procfs::VERSION));
    super::mount("/sys", Arc::clone(&fs) as _)?;
    Ok(())
}
