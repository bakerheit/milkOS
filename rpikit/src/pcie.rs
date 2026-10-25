//! The BCM2711 PCIe root complex and the VL805 behind it: the ordered bring-up steps, the orders in which the
//! VL805 firmware handshake can be tried, and the register field maths. Pure; `kernel/src/rpi/pcie.rs` does the
//! register accesses and walks these tables (docs/raspberry-pi.md, "PCIe + USB").
//!
//! The tables exist for two reasons. An SError is reported long after the access that raised it, so every step
//! ends with a checkpoint carrying its `label` (`milk.pcie=probe` adds one after every register write); and the
//! handshake with the firmware (the mailbox call that makes the VideoCore load the VL805's code) has no
//! documented position relative to the device's command register, so [`ORDERS`] lists the plausible ones and
//! the probe run reports which of them the board accepts.

/// One stage of the bring-up, in the order it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The word the log uses (`pcie: step 4 misc-ctrl ok`).
    pub name: &'static str,
    /// The SError checkpoint label (`pcie-` + name): the crash dump names it.
    pub label: &'static str,
    /// What it does, for the docs and the probe log.
    pub what: &'static str,
}

const fn step(name: &'static str, label: &'static str, what: &'static str) -> Step {
    Step { name, label, what }
}

/// The steps, first to last.
pub const STEPS: [Step; 13] = [
    step("bridge-reset", "pcie-bridge-reset", "assert the bridge reset"),
    step("bridge-release", "pcie-bridge-release", "release the bridge reset"),
    step("serdes-power", "pcie-serdes-power", "take the SerDes out of power-down"),
    step("misc-ctrl", "pcie-misc-ctrl", "burst size, config-read-UR mode, memory access on"),
    step("inbound-window", "pcie-inbound-window", "the RAM window the devices' DMA goes through"),
    step("intr-mask", "pcie-intr-mask", "mask and clear the controller's own interrupts"),
    step("perst-release", "pcie-perst-release", "release PERST# and wait the 100 ms"),
    step("link-wait", "pcie-link-wait", "wait for the physical link and the data link"),
    step("outbound-window", "pcie-outbound-window", "the CPU window the devices' BARs live in"),
    step("root-port", "pcie-root-port", "class code, link capability, read the root port"),
    step("enumerate", "pcie-enumerate", "find the devices behind the root port, place their BARs (commands stay off)"),
    step("vl805-handshake", "pcie-vl805-handshake", "tell the firmware about the VL805 (see ORDERS)"),
    step("enable", "pcie-enable", "memory decode and bus mastering on, first read of the controller's BAR"),
];

/// Index of the step called `name` in [`STEPS`].
pub fn step_index(name: &str) -> Option<usize> {
    STEPS.iter().position(|s| s.name == name)
}

/// One way of ordering the firmware's VL805 handshake against the command registers (memory decode, bus
/// mastering) of the root port and the device. The BARs and the bridge windows are always in place first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    pub name: &'static str,
    /// SError checkpoint label of this attempt.
    pub label: &'static str,
    /// Make the firmware call at all.
    pub notify: bool,
    /// Memory decode is already on (root port and device) when the call is made.
    pub memory_before: bool,
    /// Bus mastering is already on when the call is made.
    pub master_before: bool,
    pub what: &'static str,
}

/// The orders, in the order they are tried. The first is the default: everything off while the firmware
/// resets the chip, the way a PCI core that enables a device only when its driver asks does it.
pub const ORDERS: [Order; 4] = [
    Order {
        name: "notify-first",
        label: "pcie-order-notify-first",
        notify: true,
        memory_before: false,
        master_before: false,
        what: "firmware call with memory decode and bus mastering off",
    },
    Order {
        name: "notify-memory-on",
        label: "pcie-order-notify-memory-on",
        notify: true,
        memory_before: true,
        master_before: false,
        what: "firmware call with memory decode on, bus mastering off",
    },
    Order {
        name: "notify-all-on",
        label: "pcie-order-notify-all-on",
        notify: true,
        memory_before: true,
        master_before: true,
        what: "firmware call with memory decode and bus mastering on (the first version of this driver)",
    },
    Order {
        name: "no-notify",
        label: "pcie-order-no-notify",
        notify: false,
        memory_before: false,
        master_before: false,
        what: "no firmware call (boards whose VL805 loads its own code from an EEPROM)",
    },
];

/// The word the firmware wants for the device it should reset: bus, device and function as the controller's
/// config-space index register has them (`bus << 20 | dev << 15 | func << 12`; 0x0010_0000 for 01:00.0).
pub const fn vl805_notify_word(bus: u8, dev: u8, func: u8) -> u32 {
    (bus as u32) << 20 | ((dev & 0x1f) as u32) << 15 | ((func & 7) as u32) << 12
}

/// The `milk.pcie=` option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Bring the link up, try the orders until one works, USB if it does.
    Normal,
    /// The same, with a checkpoint after every register write, a log line per step, and every order tried.
    Probe,
}

/// Parse the value of `milk.pcie=`; `None` for a word we do not know (the caller says so and uses [`Mode::Normal`]).
pub fn parse_mode(v: &[u8]) -> Option<Mode> {
    match v {
        b"" | b"normal" | b"on" => Some(Mode::Normal),
        b"probe" => Some(Mode::Probe),
        _ => None,
    }
}

/// The inbound window's size field: log2(size) - 15 for 64 KiB..32 GiB, the controller's encoding below that
/// (4 KiB..32 KiB start at 0x1c). 0 for sizes it cannot express.
pub fn encode_ibar_size(size: u64) -> u32 {
    if size == 0 {
        return 0;
    }
    let log2 = 63 - size.leading_zeros();
    match log2 {
        12..=15 => log2 - 12 + 0x1c,
        16..=35 => log2 - 15,
        _ => 0,
    }
}

/// The SCB size field of the misc control register for an inbound window of `size` bytes (log2 - 15).
pub fn scb_size_field(size: u64) -> u32 {
    if size < 1 << 15 { 0 } else { 63 - size.leading_zeros() - 15 }
}

/// The outbound window registers for CPU `cpu`.. `cpu + size` seen by the devices as `pci`..: the low and
/// high PCI base words, the base/limit word (megabyte units, 12 bits each, in bits 4..15 and 20..31) and the
/// two high-bits words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outbound {
    pub pci_lo: u32,
    pub pci_hi: u32,
    pub base_limit: u32,
    pub base_hi: u32,
    pub limit_hi: u32,
}

/// Mask of the fields `base_limit` holds.
pub const OUTBOUND_BASE_LIMIT_MASK: u32 = 0x0000_fff0 | 0xfff0_0000;

pub fn outbound(pci: u64, cpu: u64, size: u64) -> Outbound {
    let base_mb = cpu >> 20;
    let limit_mb = (cpu + size.max(1) - 1) >> 20;
    Outbound {
        pci_lo: pci as u32,
        pci_hi: (pci >> 32) as u32,
        base_limit: ((base_mb as u32) & 0xfff) << 4 | ((limit_mb as u32) & 0xfff) << 20,
        base_hi: ((base_mb >> 12) as u32) & 0xff,
        limit_hi: ((limit_mb >> 12) as u32) & 0xff,
    }
}

/// What the device's identity looks like after the firmware call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Survival {
    /// Same vendor/device word as before.
    Same,
    /// Nothing answers (all ones or zeros): the link is down or the chip is still in reset.
    Gone,
    /// Something else answers.
    Changed,
}

pub fn survival(before: u32, after: u32) -> Survival {
    if after == before {
        Survival::Same
    } else if after == 0 || after == !0 {
        Survival::Gone
    } else {
        Survival::Changed
    }
}

/// Does the first dword of an xHCI controller's capability registers (CAPLENGTH in the low byte, HCIVERSION in
/// bits 16..31) look like one? The VL805 says 0x0100_0020 (version 1.00, 32 bytes of capability registers).
pub fn plausible_xhci_caps(v: u32) -> bool {
    let caplen = v & 0xff;
    let version = v >> 16;
    (0x0090..=0x0120).contains(&version) && (0x10..=0x40).contains(&caplen)
}
