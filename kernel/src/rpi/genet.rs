//! The Raspberry Pi 4's Gigabit Ethernet MAC (Broadcom GENET v5) with its BCM54213PE PHY, behind
//! RGMII, as a network interface.
//!
//! **Untested on hardware.** qemu's raspi4b does not model it, so this was written from the
//! controller's register layout and the order Broadcom's driver brings it up in, and logs what it
//! reads (PHY id, link, DMA state) so the first run on a board shows where it stands
//! (docs/raspberry-pi.md, "Stage 2"). The descriptor words and the autonegotiation result are
//! `rpikit::genet` (host-tested).
//!
//! How it works: one receive ring and one transmit ring (the default ring, number 16 of the DMA
//! engine), 64 descriptors each, one 2 KiB buffer per descriptor, the descriptors living in the
//! controller's own memory (an array of 12-byte records: length/status, address low, address high).
//! The hardware advances the receive PRODUCER index when a frame lands and the transmit CONSUMER
//! index when one has gone out; software advances the other two. The PHY is driven over the MAC's MDIO
//! master: reset, advertise everything, wait for autonegotiation, then program the MAC's speed and
//! duplex from what the two ends agreed on. A thread polls the link every 250 ms and pokes the network
//! stack every 10 ms as a safety net next to the interrupt.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::Timeout;
use crate::dma::{self, Buffer};
use crate::kprintln;
use crate::mmu::phys_to_virt;
use crate::net::nic::{DevStats, Nic, RxBuf};

/// The controller, ARM physical (low-peripheral mode): just below the PCIe window.
const GENET_PHYS: usize = 0xfd58_0000;
const BASE: usize = phys_to_virt(GENET_PHYS);
/// INTRL2_0's interrupt: SPI 157.
const IRQ: u32 = 32 + 157;

// Blocks.
const SYS: usize = 0x0000;
const EXT: usize = 0x0080;
const INTRL2_0: usize = 0x0200;
const RBUF: usize = 0x0300;
const UMAC: usize = 0x0800;
const RDMA: usize = 0x2000;
const TDMA: usize = 0x4000;

// SYS.
const SYS_REV_CTRL: usize = SYS;
const SYS_PORT_CTRL: usize = SYS + 0x04;
const SYS_RBUF_FLUSH_CTRL: usize = SYS + 0x08;
const SYS_TBUF_FLUSH_CTRL: usize = SYS + 0x0c;
const PORT_MODE_EXT_GPHY: u32 = 3;
// EXT.
const EXT_PWR_MGMT: usize = EXT;
const EXT_PWR_DOWN_BIAS: u32 = 1 << 0;
const EXT_PWR_DOWN_DLL: u32 = 1 << 1;
const EXT_PWR_DOWN_PHY: u32 = 1 << 2;
const EXT_RGMII_OOB_CTRL: usize = EXT + 0x0c;
const RGMII_LINK: u32 = 1 << 4;
const OOB_DISABLE: u32 = 1 << 5;
const RGMII_MODE_EN: u32 = 1 << 6;
const ID_MODE_DIS: u32 = 1 << 16;
// INTRL2_0.
const INTRL2_STAT: usize = INTRL2_0;
const INTRL2_CLEAR: usize = INTRL2_0 + 0x08;
const INTRL2_MASK_STATUS: usize = INTRL2_0 + 0x0c;
const INTRL2_MASK_SET: usize = INTRL2_0 + 0x10;
const INTRL2_MASK_CLEAR: usize = INTRL2_0 + 0x14;
const IRQ_RXDMA_DONE: u32 = 7 << 13; // MBDONE | PDONE | BDONE
// RBUF.
const RBUF_CTRL: usize = RBUF;
// UMAC.
const UMAC_CMD: usize = UMAC + 0x008;
const UMAC_MAC0: usize = UMAC + 0x00c;
const UMAC_MAC1: usize = UMAC + 0x010;
const UMAC_MAX_FRAME_LEN: usize = UMAC + 0x014;
const UMAC_TX_FLUSH: usize = UMAC + 0x334;
const UMAC_MIB_CTRL: usize = UMAC + 0x580;
const UMAC_MDIO_CMD: usize = UMAC + 0x614;
const CMD_TX_EN: u32 = 1 << 0;
const CMD_RX_EN: u32 = 1 << 1;
const CMD_SPEED_SHIFT: u32 = 2;
const CMD_PROMISC: u32 = 1 << 4;
const CMD_PAD_EN: u32 = 1 << 5;
const CMD_RX_PAUSE_IGNORE: u32 = 1 << 8;
const CMD_HD_EN: u32 = 1 << 10;
const CMD_SW_RESET: u32 = 1 << 13;
const CMD_LCL_LOOP_EN: u32 = 1 << 15;
const CMD_TX_PAUSE_IGNORE: u32 = 1 << 28;
const MDIO_START_BUSY: u32 = 1 << 29;
const MDIO_READ_FAIL: u32 = 1 << 28;
const MDIO_RD: u32 = 2 << 26;
const MDIO_WR: u32 = 1 << 26;

// DMA: descriptors first (256 x 12 bytes), then the ring registers (0x40 each, 17 rings), then the
// engine's global registers.
const TOTAL_DESC: usize = 256;
const DESC_SIZE: usize = 12;
const RING_SIZE_BYTES: usize = 0x40;
const DEFAULT_RING: usize = 16;
const RINGS_SIZE: usize = RING_SIZE_BYTES * (DEFAULT_RING + 1);
const DESC_BYTES: usize = TOTAL_DESC * DESC_SIZE;
const fn ring_reg(dma: usize, off: usize) -> usize {
    dma + DESC_BYTES + RING_SIZE_BYTES * DEFAULT_RING + off
}
const fn dma_reg(dma: usize, off: usize) -> usize {
    dma + DESC_BYTES + RINGS_SIZE + off
}
// Ring registers (TX names; RX uses the same offsets for its counterparts).
const R_READ_PTR: usize = 0x00; // TX read pointer / RX write pointer
const R_CONS_INDEX: usize = 0x08; // TX consumer / RX producer (written by hardware)
const R_PROD_INDEX: usize = 0x0c; // TX producer / RX consumer (written by software)
const R_BUF_SIZE: usize = 0x10;
const R_START_ADDR: usize = 0x14;
const R_END_ADDR: usize = 0x1c;
const R_MBUF_DONE_THRESH: usize = 0x24;
const R_FLOW_PERIOD: usize = 0x28; // TX flow period / RX xon-xoff threshold
const R_WRITE_PTR: usize = 0x2c; // TX write pointer / RX read pointer
// DMA engine registers.
const D_RING_CFG: usize = 0x00;
const D_CTRL: usize = 0x04;
const D_SCB_BURST_SIZE: usize = 0x0c;
const DMA_EN: u32 = 1 << 0;
const DMA_RING_BUF_EN_SHIFT: u32 = 1;
const DMA_MAX_BURST_LENGTH: u32 = 0x08;
// Descriptor words.
const BD_LENGTH_STATUS: usize = 0;
const BD_ADDR_LO: usize = 4;
const BD_ADDR_HI: usize = 8;

const RING_SIZE: usize = 64;
const BUF_SIZE: usize = 2048;
/// Largest Ethernet frame we send or expect (no FCS, no VLAN tag).
const MAX_FRAME: usize = 1514;

// MII registers and bits.
const MII_BMCR: u8 = 0;
const MII_BMSR: u8 = 1;
const MII_PHYSID1: u8 = 2;
const MII_PHYSID2: u8 = 3;
const MII_ADVERTISE: u8 = 4;
const MII_LPA: u8 = 5;
const MII_CTRL1000: u8 = 9;
const MII_STAT1000: u8 = 10;
const BMCR_RESET: u16 = 1 << 15;
const BMCR_ANENABLE: u16 = 1 << 12;
const BMCR_ANRESTART: u16 = 1 << 9;
const BMSR_LSTATUS: u16 = 1 << 2;
const BMSR_ANEGCOMPLETE: u16 = 1 << 5;
const ADVERTISE_ALL: u16 = 0x01e0 | 0x0400 | 0x0800 | 1; // 10/100 full and half, pause, selector
const CTRL1000_ADV_FULL: u16 = 0x0200;
const CTRL1000_ADV_HALF: u16 = 0x0100;
// Broadcom aux control (shadow registers), for the RGMII receive clock skew.
const MII_BCM54XX_AUXCTL: u8 = 0x18;
const AUXCTL_SHDWSEL_MISC: u16 = 0x07;
const AUXCTL_MISC_WREN: u16 = 0x8000;
const AUXCTL_MISC_RGMII_SKEW_EN: u16 = 0x0100;

fn rd(off: usize) -> u32 {
    // SAFETY: the controller's registers, mapped as Device memory (platform.rs).
    unsafe { read_volatile((BASE + off) as *const u32) }
}

fn wr(off: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile((BASE + off) as *mut u32, v) }
}

fn delay_us(us: u64) {
    let t = Timeout::us(us);
    while !t.expired() {
        core::hint::spin_loop();
    }
}

// ---- MDIO -----------------------------------------------------------------------------------

fn mdio_wait() -> Option<u32> {
    let t = Timeout::ms(20);
    loop {
        let v = rd(UMAC_MDIO_CMD);
        if v & MDIO_START_BUSY == 0 {
            return Some(v);
        }
        if t.expired() {
            return None;
        }
    }
}

fn mdio_read(phy: u8, reg: u8) -> Option<u16> {
    mdio_wait()?;
    wr(
        UMAC_MDIO_CMD,
        MDIO_START_BUSY | MDIO_RD | (phy as u32) << 21 | (reg as u32) << 16,
    );
    let v = mdio_wait()?;
    if v & MDIO_READ_FAIL != 0 {
        return None;
    }
    Some(v as u16)
}

fn mdio_write(phy: u8, reg: u8, val: u16) -> bool {
    if mdio_wait().is_none() {
        return false;
    }
    wr(
        UMAC_MDIO_CMD,
        MDIO_START_BUSY | MDIO_WR | (phy as u32) << 21 | (reg as u32) << 16 | val as u32,
    );
    mdio_wait().is_some()
}

// ---- link state shared with the poll thread ------------------------------------------------------

static LINK_UP: AtomicBool = AtomicBool::new(false);
/// The PHY's MDIO address (0xff: none found).
static PHY_ADDR: AtomicU8 = AtomicU8::new(0xff);

use rpikit::genet::{self as gk, Negotiated, resolve};

/// Program the MAC for the negotiated mode and tell the RGMII block the link is up.
fn mac_link(n: Negotiated) {
    let speed = match n.mbps {
        1000 => 2,
        100 => 1,
        _ => 0,
    };
    let mut cmd = rd(UMAC_CMD);
    cmd &= !(3 << CMD_SPEED_SHIFT | CMD_HD_EN);
    cmd |= speed << CMD_SPEED_SHIFT | if n.full { 0 } else { CMD_HD_EN };
    cmd |= CMD_TX_EN | CMD_RX_EN;
    wr(UMAC_CMD, cmd);
    wr(
        EXT_RGMII_OOB_CTRL,
        (rd(EXT_RGMII_OOB_CTRL) & !OOB_DISABLE) | RGMII_LINK | RGMII_MODE_EN,
    );
}

fn mac_link_down() {
    wr(EXT_RGMII_OOB_CTRL, rd(EXT_RGMII_OOB_CTRL) & !RGMII_LINK);
    wr(UMAC_CMD, rd(UMAC_CMD) & !(CMD_TX_EN | CMD_RX_EN));
}

/// Find the PHY: `hint` first (the device tree's address), then every address.
fn find_phy(hint: Option<u8>) -> Option<(u8, u32)> {
    let order = hint.into_iter().chain(0..32);
    for a in order {
        let (Some(id1), Some(id2)) = (mdio_read(a, MII_PHYSID1), mdio_read(a, MII_PHYSID2)) else {
            continue;
        };
        if id1 != 0xffff && id1 != 0 || id2 != 0xffff && id2 != 0 {
            return Some((a, (id1 as u32) << 16 | id2 as u32));
        }
    }
    None
}

/// Reset the PHY, give it the RGMII receive delay the board needs (rgmii-rxid), advertise
/// everything and start autonegotiation.
fn phy_start(phy: u8) {
    mdio_write(phy, MII_BMCR, BMCR_RESET);
    let t = Timeout::ms(500);
    while mdio_read(phy, MII_BMCR).is_some_and(|v| v & BMCR_RESET != 0) && !t.expired() {
        delay_us(1000);
    }
    // RGMII RX clock skew on (the PHY adds the receive delay): shadow register 7, bit 8.
    if mdio_write(
        phy,
        MII_BCM54XX_AUXCTL,
        AUXCTL_SHDWSEL_MISC | AUXCTL_SHDWSEL_MISC << 12,
    ) && let Some(v) = mdio_read(phy, MII_BCM54XX_AUXCTL)
    {
        mdio_write(
            phy,
            MII_BCM54XX_AUXCTL,
            AUXCTL_SHDWSEL_MISC | (v & !0x7007) | AUXCTL_MISC_WREN | AUXCTL_MISC_RGMII_SKEW_EN,
        );
    }
    mdio_write(phy, MII_ADVERTISE, ADVERTISE_ALL);
    mdio_write(phy, MII_CTRL1000, CTRL1000_ADV_FULL | CTRL1000_ADV_HALF);
    mdio_write(phy, MII_BMCR, BMCR_ANENABLE | BMCR_ANRESTART);
}

/// Look at the PHY and bring the MAC to match; returns whether the link is up.
fn poll_link() -> bool {
    let phy = PHY_ADDR.load(Ordering::Relaxed);
    if phy == 0xff {
        return false;
    }
    // The link status bit latches low: read twice for the current state.
    let _ = mdio_read(phy, MII_BMSR);
    let Some(bmsr) = mdio_read(phy, MII_BMSR) else {
        return false;
    };
    let up = bmsr & BMSR_LSTATUS != 0 && bmsr & BMSR_ANEGCOMPLETE != 0;
    let was = LINK_UP.load(Ordering::Relaxed);
    if up && !was {
        let adv = mdio_read(phy, MII_ADVERTISE).unwrap_or(0);
        let lpa = mdio_read(phy, MII_LPA).unwrap_or(0);
        let c1000 = mdio_read(phy, MII_CTRL1000).unwrap_or(0);
        let s1000 = mdio_read(phy, MII_STAT1000).unwrap_or(0);
        match resolve(adv, lpa, c1000, s1000) {
            Some(n) => {
                mac_link(n);
                LINK_UP.store(true, Ordering::Release);
                kprintln!(
                    "[genet] link up: {} Mb/s {} duplex (adv {adv:#06x} lpa {lpa:#06x} ctrl1000 {c1000:#06x} stat1000 {s1000:#06x})",
                    n.mbps,
                    if n.full { "full" } else { "half" }
                );
            }
            None => kprintln!("[genet] the PHY says link up but no common mode (lpa {lpa:#06x})"),
        }
    } else if !up && was {
        LINK_UP.store(false, Ordering::Release);
        mac_link_down();
        kprintln!("[genet] link down");
    }
    up
}

// ---- the device ----------------------------------------------------------------------------------

pub struct Genet {
    mac: [u8; 6],
    rx_bufs: Buffer,
    tx_bufs: Buffer,
    /// Receive: the next descriptor to look at (free-running, 16-bit like the hardware's index).
    rx_cons: u16,
    /// Transmit: the next descriptor to fill (free-running).
    tx_prod: u16,
    rx_kicked: bool,
    pub stats: DevStats,
}

// SAFETY: the buffers are owned by this struct; the registers are accessed under the network stack's lock.
unsafe impl Send for Genet {}

fn bd_write(dma: usize, i: usize, status: u32, pa: u64) {
    let b = dma + i * DESC_SIZE;
    wr(b + BD_ADDR_LO, pa as u32);
    wr(b + BD_ADDR_HI, (pa >> 32) as u32);
    wr(b + BD_LENGTH_STATUS, status);
}

impl Genet {
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link_up(&self) -> bool {
        LINK_UP.load(Ordering::Acquire)
    }

    fn rx_pa(&self, i: usize) -> u64 {
        self.rx_bufs.pa() + (i * BUF_SIZE) as u64
    }

    pub fn rx_pop(&mut self) -> Option<RxBuf> {
        loop {
            let prod = rd(ring_reg(RDMA, R_CONS_INDEX)) as u16;
            if prod == self.rx_cons {
                return None;
            }
            let slot = self.rx_cons as usize % RING_SIZE;
            let status = rd(RDMA + slot * DESC_SIZE + BD_LENGTH_STATUS);
            let Some(len) = gk::rx_frame_len(status, BUF_SIZE) else {
                // A bad or fragmented frame (we only post whole-frame buffers): drop it.
                self.stats.rx_errors += 1;
                self.recycle(slot);
                continue;
            };
            self.rx_bufs.sync(slot * BUF_SIZE, len);
            self.stats.rx_packets += 1;
            self.stats.rx_bytes += len as u64;
            let ptr = (self.rx_bufs.va() + slot * BUF_SIZE) as *const u8;
            return Some(RxBuf::new(slot as u16, ptr, len));
        }
    }

    /// Give descriptor `slot` back to the hardware and advance our consumer index past it.
    fn recycle(&mut self, slot: usize) {
        bd_write(RDMA, slot, gk::rx_status(BUF_SIZE), self.rx_pa(slot));
        self.rx_cons = self.rx_cons.wrapping_add(1);
        self.rx_kicked = true;
    }

    pub fn rx_done(&mut self, b: RxBuf) {
        self.recycle(b.head as usize);
    }

    pub fn rx_kick(&mut self) {
        if self.rx_kicked {
            self.rx_kicked = false;
            // SAFETY: barrier: descriptors before the index.
            unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
            wr(ring_reg(RDMA, R_PROD_INDEX), self.rx_cons as u32);
        }
    }

    pub fn tx_reclaim(&mut self) {
        // Nothing to give back: the buffers belong to descriptors, reused in order once the hardware's
        // consumer index has passed them (send_with checks).
    }

    pub fn send_with(&mut self, fill: impl FnOnce(&mut [u8]) -> usize) -> bool {
        let cons = rd(ring_reg(TDMA, R_CONS_INDEX)) as u16;
        if gk::in_flight(self.tx_prod, cons) >= RING_SIZE {
            self.stats.tx_dropped += 1;
            return false;
        }
        let slot = self.tx_prod as usize % RING_SIZE;
        let va = self.tx_bufs.va() + slot * BUF_SIZE;
        // SAFETY: slot's buffer, BUF_SIZE long, not owned by the hardware (checked above).
        let buf = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, MAX_FRAME) };
        let n = fill(buf).min(MAX_FRAME);
        if n == 0 {
            return false;
        }
        // Pad short frames to the Ethernet minimum (60 bytes without the FCS the MAC appends).
        let n = if n < 60 {
            buf[n..60].fill(0);
            60
        } else {
            n
        };
        self.tx_bufs.sync(slot * BUF_SIZE, n);
        let pa = self.tx_bufs.pa() + (slot * BUF_SIZE) as u64;
        bd_write(TDMA, slot, gk::tx_status(n), pa);
        self.tx_prod = self.tx_prod.wrapping_add(1);
        // SAFETY: barrier: the descriptor before the producer index that releases it.
        unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
        wr(ring_reg(TDMA, R_PROD_INDEX), self.tx_prod as u32);
        self.stats.tx_packets += 1;
        self.stats.tx_bytes += n as u64;
        true
    }
}

fn init_rings(rx: &Buffer) {
    // Descriptor memory is the controller's own: clear it.
    for i in 0..TOTAL_DESC {
        bd_write(RDMA, i, 0, 0);
        bd_write(TDMA, i, 0, 0);
    }
    // Receive ring: RING_SIZE buffers posted, indexes at zero.
    for i in 0..RING_SIZE {
        bd_write(
            RDMA,
            i,
            gk::rx_status(BUF_SIZE),
            rx.pa() + (i * BUF_SIZE) as u64,
        );
    }
    wr(dma_reg(RDMA, D_SCB_BURST_SIZE), DMA_MAX_BURST_LENGTH);
    let r = |off| ring_reg(RDMA, off);
    wr(r(R_CONS_INDEX), 0);
    wr(r(R_PROD_INDEX), 0);
    wr(r(R_BUF_SIZE), (RING_SIZE as u32) << 16 | BUF_SIZE as u32);
    wr(r(R_START_ADDR), 0);
    wr(r(R_START_ADDR + 4), 0);
    wr(r(R_END_ADDR), (RING_SIZE * 3 - 1) as u32);
    wr(r(R_END_ADDR + 4), 0);
    wr(r(R_MBUF_DONE_THRESH), 1);
    // Flow control thresholds: pause the sender when 5 descriptors are left, resume at a sixteenth.
    wr(r(R_FLOW_PERIOD), 5 << 16 | (RING_SIZE as u32 >> 4).max(1));
    wr(r(R_READ_PTR), 0);
    wr(r(R_READ_PTR + 4), 0);
    wr(r(R_WRITE_PTR), 0);
    wr(r(R_WRITE_PTR + 4), 0);
    // Transmit ring.
    wr(dma_reg(TDMA, D_SCB_BURST_SIZE), DMA_MAX_BURST_LENGTH);
    let t = |off| ring_reg(TDMA, off);
    wr(t(R_MBUF_DONE_THRESH), 1);
    wr(t(R_CONS_INDEX), 0);
    wr(t(R_PROD_INDEX), 0);
    wr(t(R_FLOW_PERIOD), 0);
    wr(t(R_BUF_SIZE), (RING_SIZE as u32) << 16 | BUF_SIZE as u32);
    wr(t(R_START_ADDR), 0);
    wr(t(R_START_ADDR + 4), 0);
    wr(t(R_END_ADDR), (RING_SIZE * 3 - 1) as u32);
    wr(t(R_END_ADDR + 4), 0);
    wr(t(R_READ_PTR), 0);
    wr(t(R_READ_PTR + 4), 0);
    wr(t(R_WRITE_PTR), 0);
    wr(t(R_WRITE_PTR + 4), 0);
}

fn dma_enable() {
    let rings = 1u32 << (DEFAULT_RING as u32 + DMA_RING_BUF_EN_SHIFT);
    wr(dma_reg(RDMA, D_RING_CFG), 1 << DEFAULT_RING);
    wr(dma_reg(TDMA, D_RING_CFG), 1 << DEFAULT_RING);
    wr(
        dma_reg(RDMA, D_CTRL),
        rd(dma_reg(RDMA, D_CTRL)) | rings | DMA_EN,
    );
    wr(
        dma_reg(TDMA, D_CTRL),
        rd(dma_reg(TDMA, D_CTRL)) | rings | DMA_EN,
    );
}

fn dma_disable() {
    let mask = (1u32 << (DEFAULT_RING as u32 + 1 + DMA_RING_BUF_EN_SHIFT)) - 1;
    wr(dma_reg(TDMA, D_CTRL), rd(dma_reg(TDMA, D_CTRL)) & !mask);
    wr(dma_reg(RDMA, D_CTRL), rd(dma_reg(RDMA, D_CTRL)) & !mask);
    wr(UMAC_TX_FLUSH, 1);
    delay_us(10);
    wr(UMAC_TX_FLUSH, 0);
}

fn on_irq(_irq: u32, _frame: &mut crate::exceptions::TrapFrame) {
    let status = rd(INTRL2_STAT) & !rd(INTRL2_MASK_STATUS);
    wr(INTRL2_CLEAR, status);
    if status & IRQ_RXDMA_DONE != 0 {
        crate::net::on_device_irq();
    }
}

/// The MAC address the firmware put in the device tree, or the one it reports itself.
fn mac_address(dtb: &rpikit::dtb::Dtb) -> [u8; 6] {
    if let Some(m) = dtb
        .find_prefix(b"ethernet@", b"local-mac-address")
        .filter(|m| m.len() == 6)
        && m.iter().any(|&b| b != 0)
    {
        return [m[0], m[1], m[2], m[3], m[4], m[5]];
    }
    super::mailbox::board_mac().unwrap_or([0x02, 0x6d, 0x69, 0x6c, 0x6b, 0x01])
}

/// Bring up the controller if the device tree says it is there, hand it to the network stack, and start
/// the thread that watches the link. Log lines tell how far each step got.
pub fn init() {
    // SAFETY: the bootloader's pointer, or 0.
    let Some(dtb) = (unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) }) else {
        return;
    };
    if dtb.find_prefix(b"ethernet@", b"compatible").is_none() {
        kprintln!("[genet] no Ethernet controller in the device tree (qemu's raspi4b has none)");
        return;
    }
    let mac = mac_address(&dtb);
    kprintln!(
        "[genet] controller at {GENET_PHYS:#x}, version register {:#010x}, mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        rd(SYS_REV_CTRL),
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    let (Some(rx_bufs), Some(tx_bufs)) = (
        Buffer::new(RING_SIZE * BUF_SIZE / 4096, 1),
        Buffer::new(RING_SIZE * BUF_SIZE / 4096, 1),
    ) else {
        kprintln!("[genet] no memory for the rings");
        return;
    };
    // Power up the MAC's blocks, reset the UMAC and the buffers.
    wr(
        EXT_PWR_MGMT,
        rd(EXT_PWR_MGMT) & !(EXT_PWR_DOWN_BIAS | EXT_PWR_DOWN_DLL | EXT_PWR_DOWN_PHY),
    );
    dma_disable();
    wr(SYS_RBUF_FLUSH_CTRL, rd(SYS_RBUF_FLUSH_CTRL) | 2);
    delay_us(10);
    wr(SYS_RBUF_FLUSH_CTRL, rd(SYS_RBUF_FLUSH_CTRL) & !2);
    delay_us(10);
    wr(SYS_TBUF_FLUSH_CTRL, 0);
    wr(UMAC_CMD, 0);
    wr(UMAC_CMD, CMD_SW_RESET | CMD_LCL_LOOP_EN);
    delay_us(2);
    wr(UMAC_CMD, 0);
    // RGMII to an external PHY; the PHY adds the receive delay, the MAC the transmit delay.
    wr(SYS_PORT_CTRL, PORT_MODE_EXT_GPHY);
    wr(
        EXT_RGMII_OOB_CTRL,
        (rd(EXT_RGMII_OOB_CTRL) & !(OOB_DISABLE | ID_MODE_DIS | RGMII_LINK)) | RGMII_MODE_EN,
    );
    // Addresses, frame size, counters.
    let (m0, m1) = gk::mac_registers(mac);
    wr(UMAC_MAC0, m0);
    wr(UMAC_MAC1, m1);
    wr(UMAC_MAX_FRAME_LEN, 1536);
    wr(UMAC_MIB_CTRL, 7);
    wr(UMAC_MIB_CTRL, 0);
    wr(RBUF_CTRL, 0);
    // Frames to anyone for now (a switch only sends us ours, plus broadcasts): the address filter is
    // a refinement for later.
    wr(
        UMAC_CMD,
        CMD_PROMISC | CMD_PAD_EN | CMD_RX_PAUSE_IGNORE | CMD_TX_PAUSE_IGNORE,
    );
    init_rings(&rx_bufs);
    dma::sync(rx_bufs.va(), rx_bufs.len());
    // The PHY.
    let hint = dtb
        .find_prefix(b"ethernet-phy@", b"reg")
        .filter(|r| r.len() >= 4)
        .map(|r| r[3]);
    match find_phy(hint) {
        Some((addr, id)) => {
            kprintln!("[genet] PHY at MDIO address {addr}, id {id:#010x}");
            PHY_ADDR.store(addr, Ordering::Release);
            phy_start(addr);
        }
        None => kprintln!("[genet] no PHY answered on MDIO: no Ethernet"),
    }
    // Interrupt on received frames; DMA on.
    wr(INTRL2_MASK_SET, 0xffff_ffff);
    wr(INTRL2_CLEAR, 0xffff_ffff);
    if crate::gic::register(IRQ, on_irq).is_ok() {
        let _ = crate::gic::enable(IRQ);
        wr(INTRL2_MASK_CLEAR, IRQ_RXDMA_DONE);
    }
    dma_enable();
    let dev = Genet {
        mac,
        rx_bufs,
        tx_bufs,
        rx_cons: 0,
        tx_prod: 0,
        rx_kicked: false,
        stats: DevStats::default(),
    };
    crate::net::attach_device(Nic::Genet(dev), 0);
    crate::sched::spawn("genet", || {
        let mut n = 0u32;
        loop {
            crate::sched::sleep_ms(10);
            n += 1;
            if n % 25 == 0 {
                poll_link();
            }
            // Safety net next to the interrupt: look at the rings every 10 ms.
            crate::net::on_device_irq();
        }
    });
}
