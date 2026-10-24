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
