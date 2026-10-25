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
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

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

use rpikit::genet::regs::*;

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

fn rd(off: usize) -> u32 {
    // SAFETY: the controller's registers, mapped as Device memory (platform.rs).
    crate::serror::note_read(BASE + off);
    unsafe { read_volatile((BASE + off) as *const u32) }
}

fn wr(off: usize, v: u32) {
    // SAFETY: as above.
    let _c = crate::serror::write_guard(BASE + off, v);
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
/// Frames given to / taken from the controller, for the 10-second report in the log (the
/// network stack's own counters live behind its lock; these are readable from the poll thread).
static TX_FRAMES: AtomicU64 = AtomicU64::new(0);
static RX_FRAMES: AtomicU64 = AtomicU64::new(0);
static RX_BAD: AtomicU64 = AtomicU64::new(0);
/// Frames the stack wanted to send while the link was down (not given to the controller).
static TX_LINK_DOWN: AtomicU64 = AtomicU64::new(0);
/// Frames refused because the transmit ring was full.
static TX_RING_FULL: AtomicU64 = AtomicU64::new(0);
/// Receive interrupts taken.
static RX_IRQS: AtomicU32 = AtomicU32::new(0);
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
    // No RGMII delay setup in the PHY: Linux's driver leaves the BCM54213PE at its strapped defaults too (it only
    // adds clock skew for some other Broadcom models), and the tree says `rgmii-rxid`: the PHY's own receive delay,
    // the MAC's transmit delay (ID_MODE_DIS clear, as below).
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
                RX_BAD.fetch_add(1, Ordering::Relaxed);
                self.recycle(slot);
                continue;
            };
            self.rx_bufs.sync(slot * BUF_SIZE, len);
            self.stats.rx_packets += 1;
            self.stats.rx_bytes += len as u64;
            RX_FRAMES.fetch_add(1, Ordering::Relaxed);
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
        // Nothing goes to the controller while the link is down: the MAC's transmitter is off then (mac_link_down),
        // a queued frame would only sit in the ring until a cable shows up and be sent stale.
        if !LINK_UP.load(Ordering::Acquire) {
            self.stats.tx_dropped += 1;
            TX_LINK_DOWN.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let cons = rd(ring_reg(TDMA, R_CONS_INDEX)) as u16;
        if gk::in_flight(self.tx_prod, cons) >= RING_SIZE {
            self.stats.tx_dropped += 1;
            TX_RING_FULL.fetch_add(1, Ordering::Relaxed);
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
        TX_FRAMES.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// Zero the descriptor memory of both engines (it is the controller's own RAM, with no reset value we can rely on).
fn clear_descriptors() {
    for i in 0..TOTAL_DESC {
        bd_write(RDMA, i, 0, 0);
        bd_write(TDMA, i, 0, 0);
    }
}

/// Give the controller its receive buffers.
fn post_rx_descriptors(rx: &Buffer) {
    for i in 0..RING_SIZE {
        bd_write(RDMA, i, gk::rx_status(BUF_SIZE), rx.pa() + (i * BUF_SIZE) as u64);
    }
}

/// Why the bring-up stopped.
struct StepFailed;

/// Runs a table of steps (`rpikit::genet::init`): one access each, and a checkpoint that can catch an SError at the end of
/// every group (after every step in `milk.genet=probe`). The first SError ends it, with a line that names the group.
struct Bringup<'a> {
    probe: bool,
    mac: [u8; 6],
    rx: &'a Buffer,
    /// Steps run so far, over all tables, and how many there are (for the `step N/M` lines).
    done: usize,
    total: usize,
}

impl Bringup<'_> {
    fn run(&mut self, steps: &[gk::Step]) -> Result<(), StepFailed> {
        for (i, st) in steps.iter().enumerate() {
            self.done += 1;
            let n = self.done;
            if self.probe {
                // Before the access: if the board hangs instead of raising an SError, this is the last line on the card.
                kprintln!("[genet] probe step {n}/{}: {} ({st})", self.total, st.name);
            }
            let seen = self.exec(st);
            let label = if self.probe { Some(st.group_end.unwrap_or(st.name)) } else { st.group_end };
            if let Some(label) = label
                && !super::serror_checkpoint_catch(label)
            {
                self.report_failure(steps, i, label);
                return Err(StepFailed);
            }
            if self.probe {
                match seen {
                    Some(v) => kprintln!("[genet] probe step {n} {} ok (read {v:#010x})", st.name),
                    None => kprintln!("[genet] probe step {n} {} ok", st.name),
                }
            }
        }
        Ok(())
    }

    fn exec(&mut self, st: &gk::Step) -> Option<u32> {
        match st.op {
            gk::Op::Read => return Some(rd(st.reg)),
            gk::Op::Write(v) => wr(st.reg, v),
            gk::Op::Set(b) => {
                let old = rd(st.reg);
                wr(st.reg, old | b);
                return Some(old);
            }
            gk::Op::Clear(b) => {
                let old = rd(st.reg);
                wr(st.reg, old & !b);
                return Some(old);
            }
            gk::Op::WriteMac0 => wr(st.reg, gk::mac_registers(self.mac).0),
            gk::Op::WriteMac1 => wr(st.reg, gk::mac_registers(self.mac).1),
            gk::Op::DelayUs(us) => delay_us(us as u64),
            gk::Op::ClearDescriptors => clear_descriptors(),
            gk::Op::PostRxDescriptors => {
                post_rx_descriptors(self.rx);
                dma::sync(self.rx.va(), self.rx.len());
            }
        }
        None
    }

    fn report_failure(&self, steps: &[gk::Step], i: usize, label: &str) {
        let n = self.done;
        if self.probe {
            kprintln!(
                "[genet] probe step {n}/{} {} FAILED: the SoC answered the access ({}) with an SError (taken at checkpoint '{label}')",
                self.total,
                steps[i].name,
                steps[i]
            );
        } else {
            let g = gk::init::group_of(steps, i);
            kprintln!(
                "[genet] an SError was taken at checkpoint '{label}': the SoC refused one of the accesses of steps {}..={} (of {}):",
                n - (i - g.start),
                n + (g.end - 1 - i),
                self.total
            );
            for st in &steps[g] {
                kprintln!("[genet]   {}: {st}", st.name);
            }
        }
        kprintln!(
            "[genet] skipping the Ethernet controller (milk.genet=probe names the exact access; milk.disable=genet skips the driver)"
        );
    }
}


fn on_irq(_irq: u32, _frame: &mut crate::exceptions::TrapFrame) {
    let status = rd(INTRL2_STAT) & !rd(INTRL2_MASK_STATUS);
    wr(INTRL2_CLEAR, status);
    if status & IRQ_RXDMA_DONE != 0 {
        RX_IRQS.fetch_add(1, Ordering::Relaxed);
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
    if dtb.find_prefix(b"ethernet@", b"compatible").is_none() || dtb.node_disabled(b"ethernet@") {
        kprintln!("[genet] no Ethernet controller in the device tree (qemu's raspi4b has none)");
        if crate::fwcfg::option("genet").is_some() {
            kprintln!("[genet] milk.genet is set but there is nothing to probe: skipped");
        }
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
    // The controller's DMA reaches memory by the ARM physical address (no 0xc0000000 alias: it sits on the system
    // bus, not behind the legacy peripheral window) and not beyond the board's DMA limit.
    let limit = crate::platform::CURRENT.dma_limit as u64;
    let (rx_end, tx_end) = (
        rx_bufs.pa() + rx_bufs.len() as u64,
        tx_bufs.pa() + tx_bufs.len() as u64,
    );
    kprintln!(
        "[genet] DMA buffers: rx {:#x}..{rx_end:#x}, tx {:#x}..{tx_end:#x} (bus = physical, limit {limit:#x})",
        rx_bufs.pa(),
        tx_bufs.pa()
    );
    if rx_end > limit || tx_end > limit {
        kprintln!("[genet] the DMA buffers are beyond what the controller can address: not using it");
        return;
    }
    // Nothing before this point has touched anything but the version register: a pending SError from an earlier driver
    // is taken here, not blamed on the controller.
    super::serror_checkpoint("genet-buffers");
    let mode = match gk::init::Mode::parse(crate::fwcfg::option("genet")) {
        Ok(m) => m,
        Err(m) => {
            kprintln!("[genet] milk.genet must be probe (or absent), running normally");
            m
        }
    };
    let probe = mode == gk::init::Mode::Probe;
    let total = gk::init::INIT.len() + gk::init::IRQ_MASK.len() + gk::init::IRQ_UNMASK.len() + gk::init::DMA_ON.len();
    if probe {
        kprintln!(
            "[genet] milk.genet=probe: {total} steps, an SError checkpoint after each; the line `probe step N name ok` is printed once its access did not raise one"
        );
    }
    let mut b = Bringup { probe, mac, rx: &rx_bufs, done: 0, total };
    // Power, reset, DMA off, MAC and ring set-up: the table's order (rpikit::genet::init::INIT).
    if b.run(gk::init::INIT).is_err() {
        return;
    }
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
        None => kprintln!(
            "[genet] no PHY answered on MDIO: no Ethernet (umac cmd {:#x} port ctrl {:#x} rgmii oob {:#x} mdio cmd {:#x}, device tree phy hint {hint:?})",
            rd(UMAC_CMD),
            rd(SYS_PORT_CTRL),
            rd(EXT_RGMII_OOB_CTRL),
            rd(UMAC_MDIO_CMD)
        ),
    }
    if !super::serror_checkpoint_catch("genet-phy") {
        kprintln!("[genet] an SError was taken at checkpoint 'genet-phy' (the PHY's MDIO accesses): skipping the Ethernet controller");
        return;
    }
    // Interrupt on received frames; DMA on.
    if b.run(gk::init::IRQ_MASK).is_err() {
        return;
    }
    if crate::gic::register(IRQ, on_irq).is_ok() {
        let _ = crate::gic::enable(IRQ);
        if b.run(gk::init::IRQ_UNMASK).is_err() {
            return;
        }
    } else {
        b.done += gk::init::IRQ_UNMASK.len();
    }
    if b.run(gk::init::DMA_ON).is_err() {
        return;
    }
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
    // The poll thread needs the scheduler, which is not up yet (this runs from `rpi::init`): `start` spawns it.
    ATTACHED.store(true, Ordering::Release);
}

/// Set once the controller is attached to the network stack; `start` then spawns its poll thread.
static ATTACHED: AtomicBool = AtomicBool::new(false);

/// The Ethernet poll thread: link state every 250 ms and the rings every 10 ms next to the interrupt.
/// Called from `kmain` once the scheduler is running (it panicked "scheduler not initialised" when
/// `init` spawned it, on the first real board).
pub fn start() {
    if !ATTACHED.swap(false, Ordering::AcqRel) {
        return;
    }
    crate::sched::spawn("genet", || {
        let mut n = 0u32;
        let mut reports = 0u32;
        let mut last = (0u64, 0u64, 0u64);
        loop {
            crate::sched::sleep_ms(10);
            n += 1;
            if n % 25 == 0 {
                poll_link();
            }
            // A pending SError shows up here at the latest 10 ms after the access (the checkpoint labels the poll thread).
            if n % 10 == 0 {
                super::serror_checkpoint("genet-poll");
            }
            // Safety net next to the interrupt: look at the rings every 10 ms.
            crate::net::on_device_irq();
            // Every 10 s: does the controller work at all? Link, frames, ring indexes. The first six
            // reports always, then only when something moved.
            if n % 1000 == 0 {
                let now = (
                    TX_FRAMES.load(Ordering::Relaxed),
                    RX_FRAMES.load(Ordering::Relaxed),
                    RX_BAD.load(Ordering::Relaxed) + TX_LINK_DOWN.load(Ordering::Relaxed),
                );
                if reports < 6 || now != last {
                    report();
                    last = now;
                }
                reports += 1;
            }
        }
    });
}

/// The 10-second line: whether the PHY sees a cable, what the controller's rings and counters say.
fn report() {
    let phy = PHY_ADDR.load(Ordering::Relaxed);
    let bmsr = if phy == 0xff { None } else { mdio_read(phy, MII_BMSR) };
    kprintln!(
        "[genet] link {} (PHY status {}), tx {} frames (dropped: {} link down, {} ring full), rx {} frames ({} bad, {} irqs); tx ring cons {} prod {}, rx ring prod {}; umac cmd {:#x}",
        if LINK_UP.load(Ordering::Relaxed) { "up" } else { "down" },
        match bmsr {
            Some(v) => alloc::format!("{v:#06x}"),
            None => alloc::string::String::from("unreadable"),
        },
        TX_FRAMES.load(Ordering::Relaxed),
        TX_LINK_DOWN.load(Ordering::Relaxed),
        TX_RING_FULL.load(Ordering::Relaxed),
        RX_FRAMES.load(Ordering::Relaxed),
        RX_BAD.load(Ordering::Relaxed),
        RX_IRQS.load(Ordering::Relaxed),
        rd(ring_reg(TDMA, R_CONS_INDEX)) as u16,
        rd(ring_reg(TDMA, R_PROD_INDEX)) as u16,
        rd(ring_reg(RDMA, R_CONS_INDEX)) as u16,
        rd(UMAC_CMD)
    );
}
