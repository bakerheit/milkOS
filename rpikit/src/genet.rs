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

/// A descriptor's three words as the controller keeps them, in memory order: length/status, buffer address low, buffer
/// address high.
pub const fn tx_desc_words(pa: u64, len: usize) -> [u32; 3] {
    [tx_status(len), pa as u32, (pa >> 32) as u32]
}

/// The shortest frame the MAC sends without the FCS it appends (the driver pads to this).
pub const MIN_FRAME: usize = 60;

/// The source address of the `txprobe` frame: locally administered (bit 1 of the first byte), unicast, nobody's real one.
pub const TXPROBE_SRC: [u8; 6] = [0x02, 0x6d, 0x69, 0x6c, 0x6b, 0x74];
/// EtherType 0x88b5: "Local Experimental Ethertype 1" (IEEE 802). No IP, no DHCP, nothing a host answers.
pub const TXPROBE_ETHERTYPE: u16 = 0x88b5;

/// The one frame `milk.genet=txprobe` sends: broadcast destination, [`TXPROBE_SRC`], EtherType 0x88b5, a short text,
/// zero padded to [`MIN_FRAME`] bytes (the MAC appends the FCS).
pub fn txprobe_frame() -> [u8; MIN_FRAME] {
    let mut f = [0u8; MIN_FRAME];
    f[..6].fill(0xff);
    f[6..12].copy_from_slice(&TXPROBE_SRC);
    f[12..14].copy_from_slice(&TXPROBE_ETHERTYPE.to_be_bytes());
    let text = b"milkOS genet txprobe";
    f[14..14 + text.len()].copy_from_slice(text);
    f
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

/// The UMAC command register with the speed and duplex bits set for `n` (other bits kept).
pub const fn umac_cmd_for(cmd: u32, n: Negotiated) -> u32 {
    let speed = match n.mbps {
        1000 => 2,
        100 => 1,
        _ => 0,
    };
    let cleared = cmd & !(3 << regs::CMD_SPEED_SHIFT | regs::CMD_HD_EN);
    cleared | speed << regs::CMD_SPEED_SHIFT | if n.full { 0 } else { regs::CMD_HD_EN }
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
    /// Words (4 bytes) per descriptor, and the descriptor memory's size in words.
    pub const DESC_WORDS: usize = 3;
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
    pub const D_STATUS: usize = 0x08;
    pub const D_SCB_BURST_SIZE: usize = 0x0c;
    /// Transmit arbiter mode and the per-ring priorities (read only by `txprobe`; nothing writes them).
    pub const D_ARB_CTRL: usize = 0x2c;
    pub const D_PRIORITY_0: usize = 0x30;
    pub const DMA_EN: u32 = 1 << 0;
    pub const DMA_RING_BUF_EN_SHIFT: u32 = 1;
    pub const DMA_MAX_BURST_LENGTH: u32 = 0x10;
    // Descriptor words.
    pub const BD_LENGTH_STATUS: usize = 0;
    pub const BD_ADDR_LO: usize = 4;
    pub const BD_ADDR_HI: usize = 8;

    /// The ring registers' END_ADDR for a ring of `nr` descriptors starting at descriptor 0: the last 32-bit word of the
    /// ring in the descriptor memory (`nr * 3 - 1`).
    pub const fn ring_end(nr: usize) -> u32 {
        (nr * DESC_WORDS - 1) as u32
    }

    pub const RING_SIZE: usize = 64;
    pub const BUF_SIZE: usize = 2048;
    /// Largest Ethernet frame we send or expect (no FCS, no VLAN tag).
    pub const MAX_FRAME: usize = 1514;
}

pub use init::{Gate, Op, Options, Step};

/// The bring-up as data: which register, in what order, with which checkpoint after it, so the order is
/// reviewable and host-tested, and `milk.genet=probe` can run it one access at a time.
pub mod init {
    use super::regs::*;
    use core::fmt;

    /// Which part of the controller a step belongs to, so a `milk.genet=` option can leave it out.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Gate {
        /// Always runs.
        Always,
        /// The receive DMA engine (left off by `norx`).
        RxDma,
        /// The transmit DMA engine (left off by `notx`).
        TxDma,
        /// The interrupt line (left masked by `polled`, and by `norx`: nothing would raise it).
        Irq,
        /// The MAC's transmitter going on at link-up (left for `txprobe` to do by hand).
        TxEn,
        /// The network stack sending (nothing for `notx` and `txprobe`: no frame of the stack's reaches the ring).
        StackTx,
    }

    /// `milk.genet=`: a comma separated list of words, how the driver runs. All off (no option) is the normal driver.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Options {
        /// A checkpoint after every single access, each logged as `step N name ok`, and a log flush before each step
        /// of the link-up path and of the first frames (`link: step N name`).
        pub probe: bool,
        /// The controller is brought up and the PHY runs, but the MAC is never programmed for a link and the network
        /// stack never sees the link: a cable can be in.
        pub nolink: bool,
        /// Link up and MAC programmed, but the receive DMA engine stays off (and with it the interrupt).
        pub norx: bool,
        /// Link up and MAC programmed, but the transmit DMA engine stays off: nothing is ever sent.
        pub notx: bool,
        /// The interrupt stays masked: the rings are polled by the Ethernet thread only.
        pub polled: bool,
        /// Link up with the MAC's transmitter still off and the network stack held back; then, once, by hand and with every
        /// register access logged and saved first: the transmitter on, a pause, one broadcast frame (EtherType 0x88b5,
        /// no IP) handed to the ring, the doorbell, the wait for the consumer index. Implies `probe`.
        pub txprobe: bool,
    }

    impl Options {
        /// From the option's value (None: absent). Words are separated by commas; `normal`, `off`, `0` and the empty
        /// word mean nothing. The second part is the first word nobody knows (the rest is still used).
        pub fn parse(v: Option<&[u8]>) -> (Options, Option<&[u8]>) {
            let mut o = Options::default();
            let mut unknown = None;
            for w in v.unwrap_or(b"").split(|&c| c == b',') {
                match w {
                    b"probe" => o.probe = true,
                    b"nolink" => o.nolink = true,
                    b"norx" => o.norx = true,
                    b"notx" => o.notx = true,
                    b"polled" => o.polled = true,
                    b"txprobe" => {
                        o.txprobe = true;
                        o.probe = true;
                    }
                    b"" | b"normal" | b"off" | b"0" => {}
                    other => unknown = unknown.or(Some(other)),
                }
            }
            (o, unknown)
        }

        /// Does a step of this part run?
        pub const fn runs(&self, g: Gate) -> bool {
            match g {
                Gate::Always => true,
                Gate::RxDma => !self.norx,
                Gate::TxDma => !self.notx,
                Gate::Irq => !self.polled && !self.norx,
                Gate::TxEn => !self.txprobe,
                Gate::StackTx => !self.notx && !self.txprobe,
            }
        }

        /// Is anything but `probe` asked for?
        pub const fn degraded(&self) -> bool {
            self.nolink || self.norx || self.notx || self.polled || self.txprobe
        }

        /// How many steps of `steps` run with these options.
        pub fn count(&self, steps: &[Step]) -> usize {
            steps.iter().filter(|s| self.runs(s.gate)).count()
        }

        /// The words, for the log (`probe,norx`; `normal` for none).
        pub fn write_words(&self, f: &mut dyn fmt::Write) -> fmt::Result {
            let mut first = true;
            for (on, w) in [
                (self.probe, "probe"),
                (self.nolink, "nolink"),
                (self.norx, "norx"),
                (self.notx, "notx"),
                (self.polled, "polled"),
                (self.txprobe, "txprobe"),
            ] {
                if on {
                    f.write_str(if first { "" } else { "," })?;
                    f.write_str(w)?;
                    first = false;
                }
            }
            if first {
                f.write_str("normal")?;
            }
            Ok(())
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
        /// Link-up path: read the PHY's advertisement and the partner's abilities (MDIO) and work out speed and duplex.
        LinkResolve,
        /// Link-up path: put the negotiated speed and duplex into the UMAC command register (read-modify-write).
        MacSpeed,
        /// No access of its own: a point in the story the log wants to name (the text says what happens next).
        Event(&'static str),
        /// `txprobe`: write descriptor 0's address low / address high / length-status word (values from the probe frame's page).
        DescAddrLo,
        DescAddrHi,
        DescStatus,
        /// `txprobe`: do nothing for this many milliseconds, saying in the (saved) log every half second that the CPU lives.
        Settle(u32),
        /// `txprobe`: poll the register (the ring's consumer index) until it reads 1, at most 500 ms.
        WaitConsumer,
    }

    /// One access of the bring-up.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Step {
        /// Short name for the log (`step 7 umac-sw-reset ok`).
        pub name: &'static str,
        /// Offset from the controller base (0 for a delay).
        pub reg: usize,
        pub op: Op,
        /// Which `milk.genet=` option leaves it out.
        pub gate: Gate,
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
                Op::LinkResolve => write!(f, "read the PHY's registers 4, 5, 9 and 10 over MDIO"),
                Op::MacSpeed => write!(f, "genet+{:#x}: speed and duplex bits <- the negotiated mode", self.reg),
                Op::Event(what) => f.write_str(what),
                Op::DescAddrLo => write!(f, "write genet+{:#x} <- the probe frame's bus address, low word", self.reg),
                Op::DescAddrHi => write!(f, "write genet+{:#x} <- the probe frame's bus address, high word", self.reg),
                Op::DescStatus => write!(f, "write genet+{:#x} <- length 60, SOP, EOP, append CRC, queue tag 0x3f", self.reg),
                Op::Settle(ms) => write!(f, "wait {ms} ms, saying every 500 ms that the CPU lives"),
                Op::WaitConsumer => write!(f, "read genet+{:#x} until it is 1 (at most 500 ms)", self.reg),
            }
        }
    }

    const fn s(name: &'static str, reg: usize, op: Op) -> Step {
        Step { name, reg, op, gate: Gate::Always, group_end: None }
    }
    const fn end(name: &'static str, reg: usize, op: Op, label: &'static str) -> Step {
        Step { name, reg, op, gate: Gate::Always, group_end: Some(label) }
    }
    const fn gated(name: &'static str, reg: usize, op: Op, gate: Gate) -> Step {
        Step { name, reg, op, gate, group_end: None }
    }
    const fn gated_end(name: &'static str, reg: usize, op: Op, gate: Gate, label: &'static str) -> Step {
        Step { name, reg, op, gate, group_end: Some(label) }
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
        s("rx-end-lo", ring_reg(R, R_END_ADDR), Op::Write(ring_end(RING_SIZE))),
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
        s("tx-end-lo", ring_reg(T, R_END_ADDR), Op::Write(ring_end(RING_SIZE))),
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

    /// Last: the default ring and the DMA engines on (each left off by its option).
    pub const DMA_ON: &[Step] = &[
        gated("rdma-ring-cfg", dma_reg(R, D_RING_CFG), Op::Write(1 << DEFAULT_RING), Gate::RxDma),
        gated("tdma-ring-cfg", dma_reg(T, D_RING_CFG), Op::Write(1 << DEFAULT_RING), Gate::TxDma),
        gated("rdma-on", dma_reg(R, D_CTRL), Op::Set(DMA_ON_BITS), Gate::RxDma),
        gated_end("tdma-on", dma_reg(T, D_CTRL), Op::Set(DMA_ON_BITS), Gate::TxDma, "genet-dma-on"),
    ];

    /// The link-up path, run by the Ethernet thread when the PHY reports a link (`nolink` never runs it). Linux's driver
    /// does the same things in this order (`bcmgenet_mii_setup`: the RGMII block's link bit first, then the UMAC's
    /// speed and duplex); the MAC's receiver and transmitter go on last, the receive interrupt between them. After the
    /// last step the network stack may send. In `milk.genet=probe` the log is saved to the card before each step.
    pub const LINK_UP: &[Step] = &[
        s("link-resolve", 0, Op::LinkResolve),
        s("link-rgmii-link", EXT_RGMII_OOB_CTRL, Op::Set(RGMII_LINK)),
        s("link-umac-speed", UMAC_CMD, Op::MacSpeed),
        s("link-mac-rx-en", UMAC_CMD, Op::Set(CMD_RX_EN)),
        gated("link-irq-unmask", INTRL2_MASK_CLEAR, Op::Write(IRQ_RXDMA_DONE), Gate::Irq),
        gated("link-mac-tx-en", UMAC_CMD, Op::Set(CMD_TX_EN), Gate::TxEn),
        end("link-announce", 0, Op::Event("the network stack is told the link is up"), "genet-link-up"),
    ];

    /// The first traffic after a link-up, named once each in `milk.genet=probe` (the log is saved first): the first
    /// frame the controller has received (before the CPU touches its descriptor or buffer) and the first frame handed
    /// to the transmitter (before the doorbell).
    pub const FIRST_FRAMES: &[Step] = &[
        gated(
            "first-rx-frame",
            0,
            Op::Event("the controller says a frame has landed; next the CPU reads its descriptor and buffer"),
            Gate::RxDma,
        ),
        gated(
            "first-tx-kick",
            0,
            Op::Event("the first frame is in its descriptor; next the transmit producer index is written"),
            Gate::StackTx,
        ),
    ];

    /// `milk.genet=txprobe`, run once by the Ethernet thread about a second after the link is up (the network stack sends nothing,
    /// and `link-mac-tx-en` was left out of the link-up path). Every step is logged, and the log saved to the card, *before* it
    /// runs. The first five are reads of the transmit DMA's registers nobody else touches (arbiter, priorities, status); then
    /// the MAC's transmitter on with nothing to send for two seconds (is the transmitter alone safe?); then descriptor 0 written
    /// word by word, read back with every ring register; then the doorbell, the wait for the consumer index, and a last pause.
    pub const TXPROBE: &[Step] = &[
        s("txp-dma-status", dma_reg(T, D_STATUS), Op::Read),
        s("txp-arb-ctrl", dma_reg(T, D_ARB_CTRL), Op::Read),
        s("txp-priority-0", dma_reg(T, D_PRIORITY_0), Op::Read),
        s("txp-priority-1", dma_reg(T, D_PRIORITY_0 + 4), Op::Read),
        s("txp-priority-2", dma_reg(T, D_PRIORITY_0 + 8), Op::Read),
        s("txp-frame", 0, Op::Event("the probe frame is built in a page of its own and cleaned out to memory")),
        s("txp-mac-tx-en", UMAC_CMD, Op::Set(CMD_TX_EN)),
        s("txp-settle-tx-en", 0, Op::Settle(2000)),
        s("txp-desc-addr-lo", T + BD_ADDR_LO, Op::DescAddrLo),
        s("txp-desc-addr-hi", T + BD_ADDR_HI, Op::DescAddrHi),
        s("txp-desc-status", T + BD_LENGTH_STATUS, Op::DescStatus),
        s("txp-readback", 0, Op::Event("descriptor 0 and every transmit register are read back")),
        s("txp-doorbell", ring_reg(T, R_PROD_INDEX), Op::Write(1)),
        s("txp-wait-sent", ring_reg(T, R_CONS_INDEX), Op::WaitConsumer),
        s("txp-after", 0, Op::Event("the transmit registers are read back once more")),
        end("txp-settle-sent", 0, Op::Settle(3000), "genet-txprobe"),
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
