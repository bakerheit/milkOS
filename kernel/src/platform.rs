//! The board the kernel runs on: where its devices are, what it has and what it does not.
//!
//! One source tree, two boards: qemu `virt` (the default build) and the Raspberry Pi 4 (the
//! BCM2711; feature `raspi`, which also decides the image's link address, see
//! kernel/build.rs). Everything board specific reads [`CURRENT`], a constant, so a `virt`
//! build compiles to exactly what it did before this file existed and the Pi build has no
//! dead `virt` code paths to trip over. Facts that depend on the firmware rather than the
//! SoC (how much RAM, which CPUs, how to start them) still come from the device tree at
//! boot (dtb.rs, mem.rs, smp.rs); [`check_dtb`] warns when the tree disagrees with the board
//! this image was built for.
//!
//! The boot tables (boot.S) need to know which 1 GiB blocks of the physical address space to
//! map before anything else runs, hence [`Platform::boot_map`].

use crate::dtb::Dtb;

/// What a 1 GiB block of the boot identity map is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    /// Not mapped.
    None,
    /// Device memory (nGnRE, execute never).
    Device,
    /// Normal cacheable RAM.
    Ram,
}

/// virtio-mmio transports (qemu virt only).
#[derive(Clone, Copy)]
pub struct VirtioMmio {
    pub base: usize,
    pub stride: usize,
    pub slots: usize,
    /// "SPI n" of slot 0.
    pub spi_base: u32,
}

/// A physical address range `[start, end)`.
pub type PhysRange = (usize, usize);

#[derive(Clone, Copy)]
pub struct Platform {
    pub name: &'static str,
    /// A substring of the root `compatible` of the device tree this board gets.
    pub dtb_compatible: &'static str,
    /// PL011 UART0 (the console).
    pub uart_base: usize,
    /// Its GIC interrupt ID.
    pub uart_irq: u32,
    /// GICv2 distributor and CPU interface.
    pub gicd: usize,
    pub gicc: usize,
    /// Physical ranges the kernel maps as Device memory (everything else is RAM).
    pub device: &'static [PhysRange],
    /// What the boot identity map has for each of the first four GiB (boot.S).
    pub boot_map: [Block; 4],
    /// RAM to assume when there is no usable device tree (kept small: right on every model).
    pub ram_fallback: (usize, usize),
    /// virtio-mmio transports, if the board has them.
    pub virtio: Option<VirtioMmio>,
    /// qemu fw_cfg (MMIO), if the board has it.
    pub fwcfg: Option<usize>,
    /// Peripheral base (BCM2711 low-peripheral mode), if the board has the BCM283x blocks.
    pub periph: Option<usize>,
    /// How secondary CPUs are started when the device tree does not say.
    pub psci_default_smc: bool,
}

/// qemu `-M virt`.
pub const VIRT: Platform = Platform {
    name: "qemu virt",
    dtb_compatible: "linux,dummy-virt",
    uart_base: 0x0900_0000,
    uart_irq: 32 + 1,
    gicd: 0x0800_0000,
    gicc: 0x0801_0000,
    // Everything below 1 GiB: UART, GIC, virtio-mmio, fw_cfg, the PCIe low window, flash.
    device: &[(0, 0x4000_0000)],
    boot_map: [Block::Device, Block::Ram, Block::None, Block::None],
    ram_fallback: (0x4000_0000, 512 << 20),
    virtio: Some(VirtioMmio {
        base: 0x0a00_0000,
        stride: 0x200,
        slots: 32,
        spi_base: 16,
    }),
    fwcfg: Some(0x0902_0000),
    periph: None,
    psci_default_smc: false,
};

/// Raspberry Pi 4 (BCM2711), and qemu `-M raspi4b`, in the firmware's default low-peripheral
/// mode: the ARM sees the SoC's peripherals at 0xfe00_0000, the ARM local block at 0xff80_0000
/// and the GIC-400 at 0xff84_1000.
pub const RASPI4: Platform = Platform {
    name: "Raspberry Pi 4 (BCM2711)",
    dtb_compatible: "brcm,bcm2711",
    uart_base: 0xfe20_1000,
    // SPI 121 in the device tree (GPU interrupt 57 + 64).
    uart_irq: 32 + 121,
    gicd: 0xff84_1000,
    gicc: 0xff84_2000,
    // PCIe, GENET, the SoC peripherals, the ARM local block and the GIC: the top 64 MiB.
    device: &[(0xfc00_0000, 0x1_0000_0000)],
    // The kernel and the firmware's device tree sit in the first GiB; 0xc000_0000.. holds the
    // peripherals (the UART talks before the full tables exist).
    boot_map: [Block::Ram, Block::None, Block::None, Block::Device],
    // 512 MiB is below every model's first RAM bank (a 1 GiB board loses the top of it to
    // the GPU).
    ram_fallback: (0, 512 << 20),
    virtio: None,
    fwcfg: None,
    periph: Some(0xfe00_0000),
    psci_default_smc: true,
};

/// This build's board.
pub const CURRENT: Platform = if cfg!(feature = "raspi") {
    RASPI4
} else {
    VIRT
};

/// True on the Raspberry Pi 4 build.
pub const RASPI: bool = cfg!(feature = "raspi");

/// A register of the BCM283x peripheral block (`offset` from the peripheral base, e.g.
/// 0x20_1000 for UART0): its physical address. Only on boards that have the block.
pub const fn periph(offset: usize) -> usize {
    match CURRENT.periph {
        Some(b) => b + offset,
        None => 0,
    }
}

/// Does the root `compatible` list (NUL-separated strings, as stored in the tree) name this
/// board?
pub fn compatible_matches(compat: &[u8], wanted: &str) -> bool {
    compat
        .split(|&b| b == 0)
        .any(|s| s.windows(wanted.len()).any(|w| w == wanted.as_bytes()) && !wanted.is_empty())
}

/// The root `compatible` of `dtb`, if it has one (first match only; a few bytes).
pub fn root_compatible<'a>(dtb: &Dtb, out: &'a mut [u8; 96]) -> &'a [u8] {
    let mut n = 0;
    dtb.for_each_prop(|_parent, _node, depth, prop, data| {
        if depth == 1 && prop == b"compatible" && n == 0 {
            n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
        }
    });
    &out[..n]
}

/// Log which board this is and, when a device tree came with the kernel, whether the tree
/// agrees. A mismatch means the wrong image was loaded (a `virt` kernel on a Pi or the other
/// way round); it cannot be fixed at run time, since the image is linked for one load address.
pub fn check_dtb() {
    use crate::kprintln;
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { Dtb::from_addr(crate::mem::boot_dtb_ptr()) };
    let mut buf = [0u8; 96];
    let compat = dtb.as_ref().map(|d| root_compatible(d, &mut buf));
    match compat {
        Some(c) if !c.is_empty() => {
            let shown = core::str::from_utf8(c.split(|&b| b == 0).next().unwrap_or(b"")).unwrap_or("?");
            if compatible_matches(c, CURRENT.dtb_compatible) {
                kprintln!("platform: {} (device tree: {shown})", CURRENT.name);
            } else {
                kprintln!(
                    "platform: {} built, but the device tree says {shown}: unexpected board",
                    CURRENT.name
                );
            }
        }
        _ => kprintln!("platform: {} (no device tree compatible string)", CURRENT.name),
    }
}
