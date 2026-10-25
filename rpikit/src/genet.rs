//! The pure parts of the GENET Ethernet driver: descriptor words and what autonegotiation
//! settled on (`kernel/src/rpi/genet.rs` does the registers).

/// A descriptor's length/status word fields (GENET v4+; the same layout for both directions, with
/// different meanings of the low bits).
pub mod bd {
    pub const LENGTH_SHIFT: u32 = 16;
    pub const SOP: u32 = 1 << 13;
    pub const EOP: u32 = 1 << 14;
    /// Transmit: let the MAC append the CRC.
    pub const TX_APPEND_CRC: u32 = 1 << 6;
    pub const TX_QTAG_SHIFT: u32 = 7;
    pub const TX_QTAG_MASK: u32 = 0x3f;
    /// Receive: RX error, overrun, CRC error, not octet aligned, too long.
    pub const RX_ERRORS: u32 = 0x1f;
}

/// The length/status word for a transmitted frame of `len` bytes, in one descriptor.
pub const fn tx_status(len: usize) -> u32 {
    (len as u32 & 0xfff) << bd::LENGTH_SHIFT
        | bd::TX_QTAG_MASK << bd::TX_QTAG_SHIFT
        | bd::TX_APPEND_CRC
        | bd::SOP
        | bd::EOP
}

/// The length/status word for an empty receive buffer of `size` bytes.
pub const fn rx_status(size: usize) -> u32 {
    (size as u32 & 0xfff) << bd::LENGTH_SHIFT
}

/// What the hardware wrote into a receive descriptor: the frame's length if it is a whole good
/// frame of 14 to `max` bytes (we post one buffer per frame), else `None`.
pub const fn rx_frame_len(status: u32, max: usize) -> Option<usize> {
    let len = (status >> bd::LENGTH_SHIFT & 0xfff) as usize;
    if status & bd::SOP == 0 || status & bd::EOP == 0 || status & bd::RX_ERRORS != 0 {
        return None;
    }
    if len < 14 || len > max {
        return None;
    }
    Some(len)
}

/// Frames in flight on a free-running 16-bit ring: producer minus consumer.
pub const fn in_flight(prod: u16, cons: u16) -> usize {
    prod.wrapping_sub(cons) as usize
}

/// A negotiated Ethernet mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub mbps: u32,
    pub full: bool,
}

const CTRL1000_ADV_FULL: u16 = 0x0200;
const CTRL1000_ADV_HALF: u16 = 0x0100;

/// The best mode both ends have, from the PHY's registers: our advertisement (register 4, and
/// 9 for gigabit), the partner's abilities (register 5, and 10 for gigabit).
pub fn resolve(adv: u16, lpa: u16, ctrl1000: u16, stat1000: u16) -> Option<Negotiated> {
    // The partner's gigabit abilities sit two bits higher in STAT1000 than ours in CTRL1000.
    let both1000 = ctrl1000 & (CTRL1000_ADV_FULL | CTRL1000_ADV_HALF) & (stat1000 >> 2);
    if both1000 & CTRL1000_ADV_FULL != 0 {
        return Some(Negotiated {
            mbps: 1000,
            full: true,
        });
    }
    if both1000 & CTRL1000_ADV_HALF != 0 {
        return Some(Negotiated {
            mbps: 1000,
            full: false,
        });
    }
    let both = adv & lpa;
    for (bit, mbps, full) in [
        (0x0100, 100, true),
        (0x0080, 100, false),
        (0x0040, 10, true),
        (0x0020, 10, false),
    ] {
        if both & bit != 0 {
            return Some(Negotiated { mbps, full });
        }
    }
    None
}

/// The MAC address split into the two UMAC registers.
pub const fn mac_registers(m: [u8; 6]) -> (u32, u32) {
    (
        (m[0] as u32) << 24 | (m[1] as u32) << 16 | (m[2] as u32) << 8 | m[3] as u32,
        (m[4] as u32) << 8 | m[5] as u32,
    )
}

/// The controller's register map (offsets from the base, bits, the DMA geometry). Named after the
/// public documentation of the block; the kernel driver and the init step table below share it.
pub mod regs {
    // Blocks.
    pub const SYS: usize = 0x0000;
    pub const EXT: usize = 0x0080;
    pub const INTRL2_0: usize = 0x0200;
    pub const RBUF: usize = 0x0300;
    pub const UMAC: usize = 0x0800;
    pub const RDMA: usize = 0x2000;
    pub const TDMA: usize = 0x4000;

    // SYS.
    pub const SYS_REV_CTRL: usize = SYS;
    pub const SYS_PORT_CTRL: usize = SYS + 0x04;
    pub const SYS_RBUF_FLUSH_CTRL: usize = SYS + 0x08;
    pub const SYS_TBUF_FLUSH_CTRL: usize = SYS + 0x0c;
    pub const PORT_MODE_EXT_GPHY: u32 = 3;
    // EXT.
    pub const EXT_PWR_MGMT: usize = EXT;
    pub const EXT_PWR_DOWN_BIAS: u32 = 1 << 0;
    pub const EXT_PWR_DOWN_DLL: u32 = 1 << 1;
    pub const EXT_PWR_DOWN_PHY: u32 = 1 << 2;
    pub const EXT_RGMII_OOB_CTRL: usize = EXT + 0x0c;
    pub const RGMII_LINK: u32 = 1 << 4;
    pub const OOB_DISABLE: u32 = 1 << 5;
    pub const RGMII_MODE_EN: u32 = 1 << 6;
    pub const ID_MODE_DIS: u32 = 1 << 16;
    // INTRL2_0.
    pub const INTRL2_STAT: usize = INTRL2_0;
    pub const INTRL2_CLEAR: usize = INTRL2_0 + 0x08;
    pub const INTRL2_MASK_STATUS: usize = INTRL2_0 + 0x0c;
    pub const INTRL2_MASK_SET: usize = INTRL2_0 + 0x10;
    pub const INTRL2_MASK_CLEAR: usize = INTRL2_0 + 0x14;
    pub const IRQ_RXDMA_DONE: u32 = 7 << 13; // MBDONE | PDONE | BDONE
    // RBUF.
    pub const RBUF_CTRL: usize = RBUF;
    pub const RBUF_CHK_CTRL: usize = RBUF + 0x14;
    pub const RBUF_TBUF_SIZE_CTRL: usize = RBUF + 0xb4;
    pub const TBUF_CTRL: usize = 0x0600;
    /// SYS_RBUF_FLUSH_CTRL bit 1: holds the UMAC in reset (it can come up set).
    pub const RBUF_UMAC_SW_RST: u32 = 1 << 1;
    // UMAC.
    pub const UMAC_CMD: usize = UMAC + 0x008;
    pub const UMAC_MAC0: usize = UMAC + 0x00c;
    pub const UMAC_MAC1: usize = UMAC + 0x010;
    pub const UMAC_MAX_FRAME_LEN: usize = UMAC + 0x014;
    pub const UMAC_TX_FLUSH: usize = UMAC + 0x334;
    pub const UMAC_MIB_CTRL: usize = UMAC + 0x580;
    pub const UMAC_MDIO_CMD: usize = UMAC + 0x614;
    pub const CMD_TX_EN: u32 = 1 << 0;
    pub const CMD_RX_EN: u32 = 1 << 1;
    pub const CMD_SPEED_SHIFT: u32 = 2;
    pub const CMD_PROMISC: u32 = 1 << 4;
    pub const CMD_PAD_EN: u32 = 1 << 5;
    pub const CMD_RX_PAUSE_IGNORE: u32 = 1 << 8;
    pub const CMD_HD_EN: u32 = 1 << 10;
    pub const CMD_SW_RESET: u32 = 1 << 13;
    pub const CMD_LCL_LOOP_EN: u32 = 1 << 15;
    pub const CMD_TX_PAUSE_IGNORE: u32 = 1 << 28;
    pub const MDIO_START_BUSY: u32 = 1 << 29;
    pub const MDIO_READ_FAIL: u32 = 1 << 28;
    pub const MDIO_RD: u32 = 2 << 26;
    pub const MDIO_WR: u32 = 1 << 26;

    // DMA: descriptors first (256 x 12 bytes), then the ring registers (0x40 each, 17 rings), then the
    // engine's global registers.
    pub const TOTAL_DESC: usize = 256;
    pub const DESC_SIZE: usize = 12;
    pub const RING_SIZE_BYTES: usize = 0x40;
    pub const DEFAULT_RING: usize = 16;
    pub const RINGS_SIZE: usize = RING_SIZE_BYTES * (DEFAULT_RING + 1);
    pub const DESC_BYTES: usize = TOTAL_DESC * DESC_SIZE;
    pub const fn ring_reg(dma: usize, off: usize) -> usize {
        dma + DESC_BYTES + RING_SIZE_BYTES * DEFAULT_RING + off
    }
    pub const fn dma_reg(dma: usize, off: usize) -> usize {
        dma + DESC_BYTES + RINGS_SIZE + off
    }
    // Ring registers (TX names; RX uses the same offsets for its counterparts).
    pub const R_READ_PTR: usize = 0x00; // TX read pointer / RX write pointer
    pub const R_CONS_INDEX: usize = 0x08; // TX consumer / RX producer (written by hardware)
    pub const R_PROD_INDEX: usize = 0x0c; // TX producer / RX consumer (written by software)
    pub const R_BUF_SIZE: usize = 0x10;
    pub const R_START_ADDR: usize = 0x14;
    pub const R_END_ADDR: usize = 0x1c;
    pub const R_MBUF_DONE_THRESH: usize = 0x24;
    pub const R_FLOW_PERIOD: usize = 0x28; // TX flow period / RX xon-xoff threshold
    pub const R_WRITE_PTR: usize = 0x2c; // TX write pointer / RX read pointer
    // DMA engine registers.
    pub const D_RING_CFG: usize = 0x00;
    pub const D_CTRL: usize = 0x04;
    pub const D_SCB_BURST_SIZE: usize = 0x0c;
    pub const DMA_EN: u32 = 1 << 0;
    pub const DMA_RING_BUF_EN_SHIFT: u32 = 1;
    pub const DMA_MAX_BURST_LENGTH: u32 = 0x10;
    // Descriptor words.
    pub const BD_LENGTH_STATUS: usize = 0;
    pub const BD_ADDR_LO: usize = 4;
    pub const BD_ADDR_HI: usize = 8;

    pub const RING_SIZE: usize = 64;
    pub const BUF_SIZE: usize = 2048;
    /// Largest Ethernet frame we send or expect (no FCS, no VLAN tag).
    pub const MAX_FRAME: usize = 1514;
}

pub use init::{Mode, Op, Step};

/// The bring-up as data: which register, in what order, with which checkpoint after it, so the order is
/// reviewable and host-tested, and `milk.genet=probe` can run it one access at a time.
pub mod init {
    use super::regs::*;
    use core::fmt;

    /// `milk.genet=`: how the driver runs its bring-up.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Mode {
        /// Group checkpoints only (a few accesses between two); the cheap default.
        Normal,
        /// A checkpoint after every single access, each logged as `step N name ok`.
        Probe,
    }

    impl Mode {
        /// From the option's value (None: absent). Unknown words are `Err` with the mode used instead.
        pub fn parse(v: Option<&[u8]>) -> Result<Mode, Mode> {
            match v {
                None => Ok(Mode::Normal),
                Some(b"probe") => Ok(Mode::Probe),
                Some(b"normal" | b"off" | b"0" | b"") => Ok(Mode::Normal),
                Some(_) => Err(Mode::Normal),
            }
        }
    }

    /// What a step does to its register.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Op {
        /// Read it (the value goes to the log in probe mode).
        Read,
        /// Write this value.
        Write(u32),
        /// Read, set these bits, write back.
        Set(u32),
        /// Read, clear these bits, write back.
        Clear(u32),
        /// The first / second UMAC address register, from the MAC address.
        WriteMac0,
        WriteMac1,
        /// Wait (no register).
        DelayUs(u32),
        /// Zero all descriptor words of both DMA engines (no single register: `reg` is the receive one).
        ClearDescriptors,
        /// Hand the receive buffers to the hardware (descriptors only).
        PostRxDescriptors,
    }

    /// One access of the bring-up.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Step {
        /// Short name for the log (`step 7 umac-sw-reset ok`).
        pub name: &'static str,
        /// Offset from the controller base (0 for a delay).
        pub reg: usize,
        pub op: Op,
        /// `Some(label)` ends a group: the checkpoint `label` follows this step even outside probe mode, and
        /// a pending SError taken there is blamed on the steps of the group.
        pub group_end: Option<&'static str>,
    }

    impl fmt::Display for Step {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.op {
                Op::Read => write!(f, "read genet+{:#x}", self.reg),
                Op::Write(v) => write!(f, "write genet+{:#x} <- {v:#x}", self.reg),
                Op::Set(b) => write!(f, "genet+{:#x} |= {b:#x}", self.reg),
                Op::Clear(b) => write!(f, "genet+{:#x} &= !{b:#x}", self.reg),
                Op::WriteMac0 => write!(f, "write genet+{:#x} <- mac[0..4]", self.reg),
                Op::WriteMac1 => write!(f, "write genet+{:#x} <- mac[4..6]", self.reg),
                Op::DelayUs(us) => write!(f, "wait {us} us"),
                Op::ClearDescriptors => write!(f, "clear {TOTAL_DESC} descriptors of each DMA engine"),
                Op::PostRxDescriptors => write!(f, "post {RING_SIZE} receive buffers"),
            }
        }
    }

    const fn s(name: &'static str, reg: usize, op: Op) -> Step {
        Step { name, reg, op, group_end: None }
    }
    const fn end(name: &'static str, reg: usize, op: Op, label: &'static str) -> Step {
        Step { name, reg, op, group_end: Some(label) }
    }

    /// Bits of the DMA control registers the disable step clears: the engine enable and every ring's enable.
    pub const DMA_OFF_MASK: u32 = (1 << (DEFAULT_RING as u32 + 1 + DMA_RING_BUF_EN_SHIFT)) - 1;
    /// The bits `dma-on` sets: the default ring's enable and the engine enable.
    pub const DMA_ON_BITS: u32 = 1 << (DEFAULT_RING as u32 + DMA_RING_BUF_EN_SHIFT) | DMA_EN;
    /// What the UMAC command register is set to once the controller is set up: frames to anyone for now (the
    /// address filter is a refinement for later), padding, the MAC ignores pause frames both ways.
    pub const UMAC_CMD_RUN: u32 = CMD_PROMISC | CMD_PAD_EN | CMD_RX_PAUSE_IGNORE | CMD_TX_PAUSE_IGNORE;

    const R: usize = RDMA;
    const T: usize = TDMA;

    /// The bring-up up to the rings, in order. The order is the point (see docs/raspberry-pi.md, "GENET bring-up"):
    /// the UMAC and the DMA engines must be out of reset (SYS_RBUF_FLUSH_CTRL clear) before anything but the SYS and
    /// EXT blocks is touched.
    pub const INIT: &[Step] = &[
        // Power: the EXT block's clock gates.
        s("ext-pwr-read", EXT_PWR_MGMT, Op::Read),
        end(
            "ext-power-up",
            EXT_PWR_MGMT,
            Op::Clear(EXT_PWR_DOWN_BIAS | EXT_PWR_DOWN_DLL | EXT_PWR_DOWN_PHY),
            "genet-power-up",
        ),
        // Reset: let the UMAC go (the register can come up with the reset bit set), pulse it, reset the UMAC itself.
        s("rbuf-ctrl-release", SYS_RBUF_FLUSH_CTRL, Op::Write(0)),
        s("wait-release", 0, Op::DelayUs(10)),
        s("umac-reset-assert", SYS_RBUF_FLUSH_CTRL, Op::Write(RBUF_UMAC_SW_RST)),
        s("wait-reset", 0, Op::DelayUs(10)),
        s("umac-reset-release", SYS_RBUF_FLUSH_CTRL, Op::Write(0)),
        s("wait-out-of-reset", 0, Op::DelayUs(10)),
        s("tbuf-flush-release", SYS_TBUF_FLUSH_CTRL, Op::Write(0)),
        s("umac-cmd-sw-reset", UMAC_CMD, Op::Write(CMD_SW_RESET | CMD_LCL_LOOP_EN)),
        s("wait-sw-reset", 0, Op::DelayUs(2)),
        end("umac-cmd-clear", UMAC_CMD, Op::Write(0), "genet-umac-reset"),
        // DMA off (and the transmit FIFO flushed), the engines' registers read for the first time.
        s("tdma-off", dma_reg(T, D_CTRL), Op::Clear(DMA_OFF_MASK)),
        s("rdma-off", dma_reg(R, D_CTRL), Op::Clear(DMA_OFF_MASK)),
        s("tx-flush-on", UMAC_TX_FLUSH, Op::Write(1)),
        s("wait-tx-flush", 0, Op::DelayUs(10)),
        end("tx-flush-off", UMAC_TX_FLUSH, Op::Write(0), "genet-dma-off"),
        // RGMII to an external PHY (the PHY adds the receive delay, the MAC the transmit delay), addresses, sizes.
        s("port-mode", SYS_PORT_CTRL, Op::Write(PORT_MODE_EXT_GPHY)),
        s("rgmii-oob-clear", EXT_RGMII_OOB_CTRL, Op::Clear(OOB_DISABLE | ID_MODE_DIS | RGMII_LINK)),
        s("rgmii-mode-en", EXT_RGMII_OOB_CTRL, Op::Set(RGMII_MODE_EN)),
        s("mac-address-0", UMAC_MAC0, Op::WriteMac0),
        s("mac-address-1", UMAC_MAC1, Op::WriteMac1),
        s("max-frame-len", UMAC_MAX_FRAME_LEN, Op::Write(1536)),
        s("mib-reset", UMAC_MIB_CTRL, Op::Write(7)),
        s("mib-run", UMAC_MIB_CTRL, Op::Write(0)),
        // Plain frames: no status block either way, no receive checksum, FCS stripped by the MAC.
        s("rbuf-ctrl", RBUF_CTRL, Op::Write(0)),
        s("tbuf-ctrl", TBUF_CTRL, Op::Write(0)),
        s("rbuf-chk-ctrl", RBUF_CHK_CTRL, Op::Write(0)),
        s("rbuf-tbuf-size", RBUF_TBUF_SIZE_CTRL, Op::Write(1)),
        end("umac-cmd-run", UMAC_CMD, Op::Write(UMAC_CMD_RUN), "genet-umac-setup"),
        // The rings: descriptor memory, then the receive ring's registers, then the transmit ring's.
        s("clear-descriptors", R, Op::ClearDescriptors),
        s("post-rx-buffers", R, Op::PostRxDescriptors),
        s("rdma-burst", dma_reg(R, D_SCB_BURST_SIZE), Op::Write(DMA_MAX_BURST_LENGTH)),
        s("rx-cons-index", ring_reg(R, R_CONS_INDEX), Op::Write(0)),
        s("rx-prod-index", ring_reg(R, R_PROD_INDEX), Op::Write(0)),
        s("rx-buf-size", ring_reg(R, R_BUF_SIZE), Op::Write((RING_SIZE as u32) << 16 | BUF_SIZE as u32)),
        s("rx-start-lo", ring_reg(R, R_START_ADDR), Op::Write(0)),
        s("rx-start-hi", ring_reg(R, R_START_ADDR + 4), Op::Write(0)),
        s("rx-end-lo", ring_reg(R, R_END_ADDR), Op::Write((RING_SIZE * 3 - 1) as u32)),
        s("rx-end-hi", ring_reg(R, R_END_ADDR + 4), Op::Write(0)),
        s("rx-mbuf-thresh", ring_reg(R, R_MBUF_DONE_THRESH), Op::Write(1)),
        // Flow control thresholds: pause the sender when 5 descriptors are left, resume at a sixteenth.
        s("rx-flow", ring_reg(R, R_FLOW_PERIOD), Op::Write(5 << 16 | (RING_SIZE as u32 >> 4))),
        s("rx-read-ptr-lo", ring_reg(R, R_READ_PTR), Op::Write(0)),
        s("rx-read-ptr-hi", ring_reg(R, R_READ_PTR + 4), Op::Write(0)),
        s("rx-write-ptr-lo", ring_reg(R, R_WRITE_PTR), Op::Write(0)),
        s("rx-write-ptr-hi", ring_reg(R, R_WRITE_PTR + 4), Op::Write(0)),
        s("tdma-burst", dma_reg(T, D_SCB_BURST_SIZE), Op::Write(DMA_MAX_BURST_LENGTH)),
        s("tx-mbuf-thresh", ring_reg(T, R_MBUF_DONE_THRESH), Op::Write(1)),
        s("tx-cons-index", ring_reg(T, R_CONS_INDEX), Op::Write(0)),
        s("tx-prod-index", ring_reg(T, R_PROD_INDEX), Op::Write(0)),
        s("tx-flow", ring_reg(T, R_FLOW_PERIOD), Op::Write(0)),
        s("tx-buf-size", ring_reg(T, R_BUF_SIZE), Op::Write((RING_SIZE as u32) << 16 | BUF_SIZE as u32)),
        s("tx-start-lo", ring_reg(T, R_START_ADDR), Op::Write(0)),
        s("tx-start-hi", ring_reg(T, R_START_ADDR + 4), Op::Write(0)),
        s("tx-end-lo", ring_reg(T, R_END_ADDR), Op::Write((RING_SIZE * 3 - 1) as u32)),
        s("tx-end-hi", ring_reg(T, R_END_ADDR + 4), Op::Write(0)),
        s("tx-read-ptr-lo", ring_reg(T, R_READ_PTR), Op::Write(0)),
        s("tx-read-ptr-hi", ring_reg(T, R_READ_PTR + 4), Op::Write(0)),
        s("tx-write-ptr-lo", ring_reg(T, R_WRITE_PTR), Op::Write(0)),
        end("tx-write-ptr-hi", ring_reg(T, R_WRITE_PTR + 4), Op::Write(0), "genet-rings"),
    ];

    /// After the PHY: all interrupt sources masked and cleared, before the line is registered with the GIC.
    pub const IRQ_MASK: &[Step] = &[
        s("irq-mask-all", INTRL2_MASK_SET, Op::Write(0xffff_ffff)),
        end("irq-clear-all", INTRL2_CLEAR, Op::Write(0xffff_ffff), "genet-irq"),
    ];

    /// Once the line is registered: receive interrupts on.
    pub const IRQ_UNMASK: &[Step] =
        &[end("irq-unmask-rx", INTRL2_MASK_CLEAR, Op::Write(IRQ_RXDMA_DONE), "genet-irq-on")];

    /// Last: the default ring and the DMA engines on.
    pub const DMA_ON: &[Step] = &[
        s("rdma-ring-cfg", dma_reg(R, D_RING_CFG), Op::Write(1 << DEFAULT_RING)),
        s("tdma-ring-cfg", dma_reg(T, D_RING_CFG), Op::Write(1 << DEFAULT_RING)),
        s("rdma-on", dma_reg(R, D_CTRL), Op::Set(DMA_ON_BITS)),
        end("tdma-on", dma_reg(T, D_CTRL), Op::Set(DMA_ON_BITS), "genet-dma-on"),
    ];

    /// The steps of the group that step `i` belongs to (from the step after the previous group's end through the
    /// next end at or after `i`): what a pending SError taken at that group's checkpoint could have come from.
    pub fn group_of(steps: &[Step], i: usize) -> core::ops::Range<usize> {
        let i = i.min(steps.len().saturating_sub(1));
        let start = steps[..i]
            .iter()
            .rposition(|s| s.group_end.is_some())
            .map_or(0, |p| p + 1);
        let end = steps[i..].iter().position(|s| s.group_end.is_some()).map_or(steps.len(), |p| i + p + 1);
        start..end
    }
}
