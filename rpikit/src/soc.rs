//! Names for the BCM2711's blocks by ARM physical address, for the crash dump's list of the last device
//! writes ("which block did the CPU last poke before the SError?"). Low-peripheral mode, the firmware's
//! default: the SoC peripherals at 0xfe00_0000, the PCIe window and the Ethernet MAC just below the
//! peripherals, the ARM local block and the GIC-400 at the top. A table of the blocks the kernel
//! touches, not of every register the chip has.

/// `(base, length, name)`, sorted by base.
pub const BLOCKS: &[(u64, u64, &str)] = &[
    (0xfd50_0000, 0x1_0000, "pcie"),
    (0xfd58_0000, 0x1_0000, "genet"),
    (0xfd5d_2000, 0x1000, "thermal"),
    (0xfe00_3000, 0x1000, "systimer"),
    (0xfe00_7000, 0x1000, "dma"),
    (0xfe00_b200, 0x200, "armctrl-intc"),
    (0xfe00_b880, 0x40, "mailbox"),
    (0xfe10_0000, 0x1000, "power-mgmt"),
    (0xfe10_1000, 0x1000, "clock-mgr"),
    (0xfe10_4000, 0x100, "rng200"),
    (0xfe20_0000, 0x100, "gpio"),
    (0xfe20_1000, 0x200, "pl011"),
    (0xfe20_4000, 0x200, "spi0"),
    (0xfe21_5000, 0x100, "aux-uart"),
    (0xfe30_0000, 0x100, "emmc1"),
    (0xfe34_0000, 0x100, "emmc2"),
    (0xfe98_0000, 0x1_0000, "usb-otg"),
    (0xff80_0000, 0x1000, "arm-local"),
    (0xff84_1000, 0x1000, "gic-dist"),
    (0xff84_2000, 0x2000, "gic-cpu"),
];

/// The block that holds `pa`, or `None`.
pub fn block(pa: u64) -> Option<(&'static str, u64)> {
    BLOCKS
        .iter()
        .find(|&&(base, len, _)| pa >= base && pa < base + len)
        .map(|&(base, _, name)| (name, pa - base))
}
