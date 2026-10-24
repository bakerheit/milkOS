//! SD card commands and answers, and the SDHCI register values that carry them.
//!
//! Pure functions over numbers: the kernel's SDHCI driver (`rpi/sdhci.rs`) pokes the
//! registers and polls, these decide what to write and what an answer means. The SDHCI layout
//! is the SD Host Controller Simplified Specification's, which the Pi 4's EMMC2 and qemu's
//! `generic-sdhci` both follow.

/// SD command indexes (physical layer simplified spec).
pub mod cmd {
    pub const GO_IDLE_STATE: u8 = 0;
    pub const ALL_SEND_CID: u8 = 2;
    pub const SEND_RELATIVE_ADDR: u8 = 3;
    /// ACMD6, after CMD55.
    pub const SET_BUS_WIDTH: u8 = 6;
    pub const SELECT_CARD: u8 = 7;
    pub const SEND_IF_COND: u8 = 8;
    pub const SEND_CSD: u8 = 9;
    pub const STOP_TRANSMISSION: u8 = 12;
    pub const SEND_STATUS: u8 = 13;
    pub const SET_BLOCKLEN: u8 = 16;
    pub const READ_SINGLE_BLOCK: u8 = 17;
    pub const READ_MULTIPLE_BLOCK: u8 = 18;
    pub const WRITE_BLOCK: u8 = 24;
    pub const WRITE_MULTIPLE_BLOCK: u8 = 25;
    /// ACMD41, after CMD55.
    pub const SD_SEND_OP_COND: u8 = 41;
    /// ACMD51, after CMD55.
    pub const SEND_SCR: u8 = 51;
    pub const APP_CMD: u8 = 55;
}

/// What the card sends back for a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resp {
    None,
    /// 136 bits (R2: CID, CSD).
    R136,
    /// 48 bits (R1, R3, R6, R7).
    R48,
    /// 48 bits, then the card holds DAT0 low while busy (R1b).
    R48Busy,
}

/// Direction of a data phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Card to host.
    Read,
    /// Host to card.
    Write,
}

/// One command to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub index: u8,
    pub arg: u32,
    pub resp: Resp,
    pub data: Option<Dir>,
    /// More than one block (sets the multi-block bits and the block-count enable).
    pub multi: bool,
    /// Let the controller send CMD12 itself after the last block.
    pub auto_stop: bool,
}

impl Command {
    pub const fn new(index: u8, arg: u32, resp: Resp) -> Command {
        Command {
            index,
            arg,
            resp,
            data: None,
            multi: false,
            auto_stop: false,
        }
    }

    /// Which response checks the controller can do for this command: the CRC check applies to
    /// every 48-bit answer except R3 (ACMD41, whose CRC bits are all ones) and to R2; the
    /// index check to R1/R1b/R6/R7 but not R2/R3.
    fn checks(&self) -> (bool, bool) {
        let r3 = self.index == cmd::SD_SEND_OP_COND;
        match self.resp {
            Resp::None => (false, false),
            Resp::R136 => (true, false),
            Resp::R48 | Resp::R48Busy => (!r3, !r3),
        }
    }
}

// SDHCI COMMAND register (0x0e) fields.
const CMD_RESP_136: u16 = 0b01;
const CMD_RESP_48: u16 = 0b10;
const CMD_RESP_48_BUSY: u16 = 0b11;
const CMD_CRC_CHECK: u16 = 1 << 3;
const CMD_INDEX_CHECK: u16 = 1 << 4;
const CMD_DATA_PRESENT: u16 = 1 << 5;

/// The SDHCI COMMAND register value (offset 0x0e) for `c`.
pub fn sdhci_command(c: &Command) -> u16 {
    let mut v = (c.index as u16 & 0x3f) << 8;
    v |= match c.resp {
        Resp::None => 0,
        Resp::R136 => CMD_RESP_136,
        Resp::R48 => CMD_RESP_48,
        Resp::R48Busy => CMD_RESP_48_BUSY,
    };
    let (crc, idx) = c.checks();
    if crc {
        v |= CMD_CRC_CHECK;
    }
    if idx {
        v |= CMD_INDEX_CHECK;
    }
    if c.data.is_some() {
        v |= CMD_DATA_PRESENT;
    }
    v
}

// SDHCI TRANSFER MODE register (0x0c) fields.
const MODE_DMA: u16 = 1 << 0;
const MODE_BLOCK_COUNT: u16 = 1 << 1;
const MODE_AUTO_CMD12: u16 = 1 << 2;
const MODE_READ: u16 = 1 << 4;
const MODE_MULTI: u16 = 1 << 5;

/// The SDHCI TRANSFER MODE register value (offset 0x0c) for `c`; `dma` selects SDMA.
pub fn sdhci_mode(c: &Command, dma: bool) -> u16 {
    let Some(dir) = c.data else {
        return 0;
    };
    let mut v = 0;
    if dir == Dir::Read {
        v |= MODE_READ;
    }
    if c.multi {
        v |= MODE_MULTI | MODE_BLOCK_COUNT;
        if c.auto_stop {
            v |= MODE_AUTO_CMD12;
        }
    }
    if dma {
        v |= MODE_DMA;
    }
    v
}

/// The two registers go out as one 32-bit write at 0x0c: mode in the low half, command in the
/// high one (writing the command half starts the command, and the Pi's controller only accepts
/// 32-bit writes there).
pub fn sdhci_cmd_word(c: &Command, dma: bool) -> u32 {
    (sdhci_command(c) as u32) << 16 | sdhci_mode(c, dma) as u32
}

/// Address argument for a data command: SDHC and SDXC cards take block numbers, standard
/// capacity cards bytes. `None` if the byte address does not fit 32 bits.
pub fn data_arg(lba: u64, high_capacity: bool) -> Option<u32> {
    if high_capacity {
        u32::try_from(lba).ok()
    } else {
        u32::try_from(lba.checked_mul(512)?).ok()
    }
}

/// CMD8 argument: 2.7-3.6 V, check pattern 0xAA.
pub const IF_COND_ARG: u32 = 0x1aa;

/// Does the CMD8 answer echo our argument (a version 2.0 card)?
pub fn if_cond_ok(resp0: u32) -> bool {
    resp0 & 0xfff == IF_COND_ARG
}

/// ACMD41 argument: 3.2-3.4 V and "we support high capacity" when the card said it is a
/// version 2.0 one.
pub fn op_cond_arg(v2_card: bool) -> u32 {
    0x00ff_8000 | if v2_card { 1 << 30 } else { 0 }
}

/// An ACMD41 answer (OCR): power-up finished?
pub fn ocr_ready(ocr: u32) -> bool {
    ocr & (1 << 31) != 0
}

/// An ACMD41 answer (OCR): high capacity (block addressed)?
pub fn ocr_high_capacity(ocr: u32) -> bool {
    ocr & (1 << 30) != 0
}

/// Card status bits (R1) that mean a command failed.
pub const R1_ERROR_MASK: u32 = 0xfdf8_0000;

/// The card's state (R1 bits 12:9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardState {
    Idle,
    Ready,
    Ident,
    Stby,
    Tran,
    Data,
    Rcv,
    Prg,
    Dis,
    Other(u8),
}

pub fn card_state(status: u32) -> CardState {
    match (status >> 9) & 0xf {
        0 => CardState::Idle,
        1 => CardState::Ready,
        2 => CardState::Ident,
        3 => CardState::Stby,
        4 => CardState::Tran,
        5 => CardState::Data,
        6 => CardState::Rcv,
        7 => CardState::Prg,
        8 => CardState::Dis,
        n => CardState::Other(n as u8),
    }
}

/// An R6 answer (CMD3): the card's relative address and the short status.
pub fn parse_r6(resp0: u32) -> (u16, u16) {
    ((resp0 >> 16) as u16, resp0 as u16)
}

/// The 136-bit answers arrive in RESP0..3 without their CRC byte: the register file holds
/// bits 127..8 of the card's 128-bit register. Put them together.
pub fn r136(resp: [u32; 4]) -> u128 {
    resp[0] as u128 | (resp[1] as u128) << 32 | (resp[2] as u128) << 64 | (resp[3] as u128) << 96
}

/// What a CSD says about the card's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csd {
    /// CSD structure version: 0 = standard capacity, 1 = high/extended capacity.
    pub version: u8,
    /// Capacity in 512-byte blocks.
    pub blocks: u64,
}

/// Read the capacity out of a CSD answer ([`r136`] of the four response registers). `None` for a
/// structure version we do not know or a size of zero.
pub fn parse_csd(resp: [u32; 4]) -> Option<Csd> {
    let r = r136(resp);
    // Registers are shifted down by 8 bits (the CRC byte is not stored): CSD bit n is r bit n-8.
    let field = |hi: u32, lo: u32| -> u64 { ((r >> (lo - 8)) & ((1u128 << (hi - lo + 1)) - 1)) as u64 };
    let version = field(127, 126) as u8;
    let blocks = match version {
        0 => {
            let read_bl_len = field(83, 80) as u32;
            let c_size = field(73, 62);
            let c_size_mult = field(49, 47) as u32;
            if read_bl_len > 11 {
                return None;
            }
            let bytes = (c_size + 1) << (c_size_mult + 2) << read_bl_len;
            bytes / 512
        }
        1 => (field(69, 48) + 1) * 1024,
        _ => return None,
    };
    if blocks == 0 {
        return None;
    }
    Some(Csd { version, blocks })
}

/// SDHCI clock control: the divider and the register value that selects it. SDHCI 3.0 divided
/// clock mode: the SD clock is `base / (2 * n)` (`n` = 0 means undivided). Picks the smallest
/// divider that does not exceed `target_hz`; returns (n, resulting Hz). `None` if the base clock
/// is unknown (0) or the target 0.
pub fn clock_divider(base_hz: u32, target_hz: u32) -> Option<(u16, u32)> {
    if base_hz == 0 || target_hz == 0 {
        return None;
    }
    if base_hz <= target_hz {
        return Some((0, base_hz));
    }
    // Smallest n with base / (2n) <= target, i.e. n >= ceil(base / (2 target)).
    let n = (base_hz as u64).div_ceil(2 * target_hz as u64).min(1023) as u16;
    Some((n, base_hz / (2 * n as u32)))
}

/// CLOCK_CONTROL register (0x2c) bits for divider `n` with the internal clock enabled: the
/// low 8 bits of `n` in 15:8, the upper 2 in 7:6, internal clock enable in bit 0.
pub fn clock_control(n: u16) -> u16 {
    ((n & 0xff) << 8) | (((n >> 8) & 3) << 6) | 1
}

/// Capacity in bytes, saturating.
pub fn blocks_to_bytes(blocks: u64) -> u64 {
    blocks.saturating_mul(512)
}
