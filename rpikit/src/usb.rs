//! The pure parts of the xHCI USB stack: TRB (transfer request block) encoding, event decoding,
//! context layouts, descriptors, and the HID boot protocol (keyboard and mouse reports).
//!
//! The kernel's driver (`kernel/src/usb/`) owns the memory and the registers and polls;
//! everything that is just numbers lives here so the host tests can check it against the
//! xHCI 1.1 specification's layouts: a Setup TRB for GET_DESCRIPTOR, the slot and endpoint
//! contexts for a low-speed keyboard behind the root port, the configuration descriptor walk
//! for a composite device, a keyboard report with rollover, auto-repeat timing. Everything that
//! comes from a device is distrusted: lengths are checked, a malformed descriptor yields fewer
//! answers, never a read outside the slice.

// ---- TRBs ---------------------------------------------------------------------------------

/// TRB type codes (xHCI 1.1, table 6-91).
pub mod trb_type {
    pub const NORMAL: u32 = 1;
    pub const SETUP_STAGE: u32 = 2;
    pub const DATA_STAGE: u32 = 3;
    pub const STATUS_STAGE: u32 = 4;
    pub const LINK: u32 = 6;
    pub const EVENT_DATA: u32 = 7;
    pub const NO_OP: u32 = 8;
    pub const ENABLE_SLOT: u32 = 9;
    pub const DISABLE_SLOT: u32 = 10;
    pub const ADDRESS_DEVICE: u32 = 11;
    pub const CONFIGURE_ENDPOINT: u32 = 12;
    pub const EVALUATE_CONTEXT: u32 = 13;
    pub const RESET_ENDPOINT: u32 = 14;
    pub const STOP_ENDPOINT: u32 = 15;
    pub const SET_TR_DEQUEUE: u32 = 16;
    pub const NO_OP_COMMAND: u32 = 23;
    pub const TRANSFER_EVENT: u32 = 32;
    pub const COMMAND_COMPLETION: u32 = 33;
    pub const PORT_STATUS_CHANGE: u32 = 34;
    pub const HOST_CONTROLLER_EVENT: u32 = 37;
}

/// Completion codes (xHCI 1.1, table 6-90) the driver cares about.
pub mod code {
    pub const SUCCESS: u8 = 1;
    pub const USB_TRANSACTION_ERROR: u8 = 4;
    pub const STALL: u8 = 6;
    pub const SHORT_PACKET: u8 = 13;
}

const CYCLE: u32 = 1;
const TOGGLE_CYCLE: u32 = 1 << 1;
const CHAIN: u32 = 1 << 4;
const IOC: u32 = 1 << 5;
const IDT: u32 = 1 << 6;
const BSR: u32 = 1 << 9;
const DIR_IN: u32 = 1 << 16;

/// One 16-byte TRB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub const fn kind(&self) -> u32 {
        (self.control >> 10) & 0x3f
    }
    pub const fn cycle(&self) -> bool {
        self.control & CYCLE != 0
    }
    /// The four little-endian words as they sit in memory.
    pub const fn words(&self) -> [u32; 4] {
        [
            self.param as u32,
            (self.param >> 32) as u32,
            self.status,
            self.control,
        ]
    }
    pub const fn from_words(w: [u32; 4]) -> Trb {
        Trb {
            param: w[0] as u64 | (w[1] as u64) << 32,
            status: w[2],
            control: w[3],
        }
    }
    /// The same TRB with its cycle bit set to `cycle`.
    pub const fn with_cycle(mut self, cycle: bool) -> Trb {
        self.control = self.control & !CYCLE | cycle as u32;
        self
    }
}

const fn ctl(kind: u32, flags: u32, cycle: bool) -> u32 {
    kind << 10 | flags | cycle as u32
}

/// A USB setup packet (the 8 bytes that start a control transfer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    /// As the 64-bit parameter of a Setup Stage TRB (the bytes in wire order, little endian).
    pub const fn param(&self) -> u64 {
        self.request_type as u64
            | (self.request as u64) << 8
            | (self.value as u64) << 16
            | (self.index as u64) << 32
            | (self.length as u64) << 48
    }
    pub const fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }
}

/// Standard and HID class requests.
pub mod req {
    use super::Setup;
    pub const GET_DESCRIPTOR: u8 = 6;
    pub const SET_CONFIGURATION: u8 = 9;
    pub const DESC_DEVICE: u16 = 1;
    pub const DESC_CONFIG: u16 = 2;

    pub const fn get_descriptor(kind: u16, index: u8, len: u16) -> Setup {
        Setup {
            request_type: 0x80,
            request: GET_DESCRIPTOR,
            value: kind << 8 | index as u16,
            index: 0,
            length: len,
        }
    }
    pub const fn set_configuration(value: u8) -> Setup {
        Setup {
            request_type: 0x00,
            request: SET_CONFIGURATION,
            value: value as u16,
            index: 0,
            length: 0,
        }
    }
    /// HID SET_PROTOCOL: 0 = boot protocol, 1 = report protocol, for `interface`.
    pub const fn hid_set_protocol(interface: u16, protocol: u16) -> Setup {
        Setup {
            request_type: 0x21,
            request: 0x0b,
            value: protocol,
            index: interface,
            length: 0,
        }
    }
    /// HID SET_IDLE: report only on change (duration 0).
    pub const fn hid_set_idle(interface: u16) -> Setup {
        Setup {
            request_type: 0x21,
            request: 0x0a,
            value: 0,
            index: interface,
            length: 0,
        }
    }
}

/// Setup Stage TRB: the setup packet as immediate data. `data` = the data stage's direction if it
/// has one (TRT: 2 = OUT data, 3 = IN data, 0 = no data stage).
pub const fn setup_stage(s: Setup, cycle: bool) -> Trb {
    let trt = if s.length == 0 {
        0
    } else if s.is_in() {
        3
    } else {
        2
    };
    Trb {
        param: s.param(),
        status: 8,
        control: ctl(trb_type::SETUP_STAGE, IDT | trt << 16, cycle),
    }
}

/// Data Stage TRB for a `len`-byte buffer at physical address `buf`.
pub const fn data_stage(buf: u64, len: u32, dir_in: bool, cycle: bool) -> Trb {
    Trb {
        param: buf,
        status: len & 0x1_ffff,
        control: ctl(trb_type::DATA_STAGE, if dir_in { DIR_IN } else { 0 }, cycle),
    }
}

/// Status Stage TRB (the direction is opposite to the data stage's; IN when there was none), with
/// an event when it completes.
pub const fn status_stage(dir_in: bool, cycle: bool) -> Trb {
    Trb {
        param: 0,
        status: 0,
        control: ctl(
            trb_type::STATUS_STAGE,
            IOC | if dir_in { DIR_IN } else { 0 },
            cycle,
        ),
    }
}

/// Normal TRB (interrupt and bulk transfers), with an event on completion.
pub const fn normal(buf: u64, len: u32, cycle: bool) -> Trb {
    Trb {
        param: buf,
        status: len & 0x1_ffff,
        control: ctl(trb_type::NORMAL, IOC, cycle),
    }
}

/// Link TRB to `next` (the ring's segment start); `toggle` flips the cycle bit the producer
/// writes on wrapping.
pub const fn link(next: u64, toggle: bool, cycle: bool) -> Trb {
    Trb {
        param: next,
        status: 0,
        control: ctl(trb_type::LINK, if toggle { TOGGLE_CYCLE } else { 0 }, cycle),
    }
}

/// Reset Endpoint command: take a halted endpoint (after a stall or an error) out of the halted state.
pub const fn reset_endpoint(slot: u8, dci: u8, cycle: bool) -> Trb {
    Trb {
        param: 0,
        status: 0,
        control: ctl(
            trb_type::RESET_ENDPOINT,
            (slot as u32) << 24 | (dci as u32 & 0x1f) << 16,
            cycle,
        ),
    }
}

/// Set TR Dequeue Pointer command: restart the endpoint's transfer ring at physical address `ptr` (16-byte
/// aligned) with the dequeue cycle state `dcs`.
pub const fn set_tr_dequeue(ptr: u64, dcs: bool, slot: u8, dci: u8, cycle: bool) -> Trb {
    Trb {
        param: (ptr & !0xf) | dcs as u64,
        status: 0,
        control: ctl(
            trb_type::SET_TR_DEQUEUE,
            (slot as u32) << 24 | (dci as u32 & 0x1f) << 16,
            cycle,
        ),
    }
}

pub const fn noop_command(cycle: bool) -> Trb {
    Trb {
        param: 0,
        status: 0,
        control: ctl(trb_type::NO_OP_COMMAND, 0, cycle),
    }
}

pub const fn enable_slot(cycle: bool) -> Trb {
    Trb {
        param: 0,
        status: 0,
        control: ctl(trb_type::ENABLE_SLOT, 0, cycle),
    }
}

pub const fn disable_slot(slot: u8, cycle: bool) -> Trb {
    Trb {
        param: 0,
        status: 0,
        control: ctl(trb_type::DISABLE_SLOT, (slot as u32) << 24, cycle),
    }
}

/// Address Device command for `slot` with the input context at `input`. `bsr` = block the SET_ADDRESS
/// request (only assign the slot).
pub const fn address_device(input: u64, slot: u8, bsr: bool, cycle: bool) -> Trb {
    Trb {
        param: input,
        status: 0,
        control: ctl(
            trb_type::ADDRESS_DEVICE,
            (slot as u32) << 24 | if bsr { BSR } else { 0 },
            cycle,
        ),
    }
}

pub const fn configure_endpoint(input: u64, slot: u8, cycle: bool) -> Trb {
    Trb {
        param: input,
        status: 0,
        control: ctl(trb_type::CONFIGURE_ENDPOINT, (slot as u32) << 24, cycle),
    }
}

pub const fn evaluate_context(input: u64, slot: u8, cycle: bool) -> Trb {
    Trb {
        param: input,
        status: 0,
        control: ctl(trb_type::EVALUATE_CONTEXT, (slot as u32) << 24, cycle),
    }
}

/// A TRB with the chain bit, for transfers split over several TRBs.
pub const fn chained(mut t: Trb) -> Trb {
    t.control |= CHAIN;
    t
}

// ---- events -------------------------------------------------------------------------------

/// What an event ring entry says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A command finished: the command TRB's address, completion code, the slot (for Enable Slot, the
    /// slot assigned).
    CommandCompletion { trb: u64, code: u8, slot: u8 },
    /// A transfer finished (or ended early): the TRB's address, completion code, bytes NOT
    /// transferred, the slot and the endpoint's device context index.
    Transfer {
        trb: u64,
        code: u8,
        residual: u32,
        slot: u8,
        dci: u8,
    },
    /// A root hub port changed state (1-based port number).
    PortStatusChange { port: u8 },
    /// Anything else (the type code).
    Other(u32),
}

pub fn decode_event(t: &Trb) -> Event {
    let code = (t.status >> 24) as u8;
    match t.kind() {
        trb_type::COMMAND_COMPLETION => Event::CommandCompletion {
            trb: t.param,
            code,
            slot: (t.control >> 24) as u8,
        },
        trb_type::TRANSFER_EVENT => Event::Transfer {
            trb: t.param,
            code,
            residual: t.status & 0x00ff_ffff,
            slot: (t.control >> 24) as u8,
            dci: ((t.control >> 16) & 0x1f) as u8,
        },
        trb_type::PORT_STATUS_CHANGE => Event::PortStatusChange {
            port: (t.param >> 24) as u8,
        },
        k => Event::Other(k),
    }
}

// ---- ports and speeds ----------------------------------------------------------------------

/// PORTSC speed field values.
pub mod speed {
    pub const FULL: u8 = 1;
    pub const LOW: u8 = 2;
    pub const HIGH: u8 = 3;
    pub const SUPER: u8 = 4;
}

/// PORTSC fields (xHCI 1.1, 5.4.8).
pub mod portsc {
    pub const CCS: u32 = 1 << 0;
    pub const PED: u32 = 1 << 1;
    pub const PR: u32 = 1 << 4;
    pub const PP: u32 = 1 << 9;
    pub const CSC: u32 = 1 << 17;
    pub const PEC: u32 = 1 << 18;
    pub const WRC: u32 = 1 << 19;
    pub const OCC: u32 = 1 << 20;
    pub const PRC: u32 = 1 << 21;
    pub const PLC: u32 = 1 << 22;
    pub const CEC: u32 = 1 << 23;
    /// The status-change bits, which are cleared by writing 1 to them.
    pub const CHANGE_BITS: u32 = CSC | PEC | WRC | OCC | PRC | PLC | CEC;
    /// Bits that must be 0 in a write that only touches other bits: the change bits, and PED (a write of
    /// 1 disables the port).
    pub const PRESERVE_MASK: u32 = !(CHANGE_BITS | PED);

    pub const fn speed(v: u32) -> u8 {
        ((v >> 10) & 0xf) as u8
    }
    /// The PLS (link state) field.
    pub const fn link_state(v: u32) -> u8 {
        ((v >> 5) & 0xf) as u8
    }
    /// A write that clears the change bits in `clear` and leaves the rest of a read value `v`
    /// as it was.
    pub const fn write_clearing(v: u32, clear: u32) -> u32 {
        (v & PRESERVE_MASK) | (clear & CHANGE_BITS)
    }
    /// A write that starts a port reset.
    pub const fn write_reset(v: u32) -> u32 {
        (v & PRESERVE_MASK) | PR
    }
}

/// Max packet size of the default control endpoint before the device descriptor says better.
pub const fn default_max_packet0(speed: u8) -> u16 {
    match speed {
        speed::LOW => 8,
        speed::FULL => 64,
        speed::HIGH => 64,
        speed::SUPER => 512,
        _ => 8,
    }
}

/// The endpoint context interval field for an interrupt endpoint with descriptor `b_interval` on a
/// device of `speed`: the period is 2^value * 125 microseconds. Full and low speed count in ms
/// (frames), high and super speed in 2^(n-1) microframes.
pub fn interrupt_interval(speed: u8, b_interval: u8) -> u8 {
    match speed {
        speed::HIGH | speed::SUPER => b_interval.clamp(1, 16) - 1,
        _ => {
            // ms -> 125 us units: ms * 8, then floor(log2), clamped to the legal 3..=10.
            let units = (b_interval.max(1) as u32) * 8;
            (31 - units.leading_zeros()).clamp(3, 10) as u8
        }
    }
}

// ---- contexts ---------------------------------------------------------------------------

/// Endpoint types for the endpoint context.
pub mod ep_type {
    pub const CONTROL: u32 = 4;
    pub const INTERRUPT_IN: u32 = 7;
}

/// The device context index of an endpoint address: EP0 is 1, then OUT = 2n, IN = 2n + 1.
pub const fn dci(ep_addr: u8) -> u8 {
    let n = ep_addr & 0x0f;
    if n == 0 { 1 } else { n * 2 + (ep_addr >> 7) }
}

/// Context size in dwords: 8 (32 bytes) or 16 (64 bytes, when HCCPARAMS1.CSZ is set).
pub const fn ctx_dwords(csz: bool) -> usize {
    if csz { 16 } else { 8 }
}

/// Layout of an Input Context in dwords: control context first, then the slot context, then the
/// endpoint contexts by DCI.
pub const fn input_slot_offset(csz: bool) -> usize {
    ctx_dwords(csz)
}
pub const fn input_ep_offset(csz: bool, dci: u8) -> usize {
    ctx_dwords(csz) * (1 + dci as usize)
}
/// Size of an input context in dwords (control + slot + 31 endpoints).
pub const fn input_ctx_dwords(csz: bool) -> usize {
    ctx_dwords(csz) * 33
}
/// Size of a device (output) context in dwords (slot + 31 endpoints).
pub const fn device_ctx_dwords(csz: bool) -> usize {
    ctx_dwords(csz) * 32
}

/// Fill the Input Control Context: which contexts the command drops / adds (bit n = context n,
/// 0 = slot, 1 = EP0, ...).
pub fn fill_input_control(ctx: &mut [u32], drop: u32, add: u32) {
    ctx[0] = drop;
    ctx[1] = add;
}

/// Slot Context for a device on root hub port `root_port` (1-based) at `speed` with endpoints up to
/// device context index `entries`.
pub fn fill_slot(slot: &mut [u32], speed: u8, entries: u8, root_port: u8) {
    slot[0] = (speed as u32) << 20 | (entries as u32 & 0x1f) << 27;
    slot[1] = (root_port as u32) << 16;
    slot[2] = 0;
    slot[3] = 0;
}

/// Endpoint Context of a control endpoint (EP0): error count 3, the max packet size, the transfer
/// ring at `ring` (physical, 16-byte aligned) with cycle state 1.
pub fn fill_ep0(ep: &mut [u32], max_packet: u16, ring: u64) {
    ep[0] = 0;
    ep[1] = 3 << 1 | ep_type::CONTROL << 3 | (max_packet as u32) << 16;
    let dq = ring | 1;
    ep[2] = dq as u32;
    ep[3] = (dq >> 32) as u32;
    ep[4] = 8; // average TRB length
}

/// Endpoint Context of an interrupt IN endpoint.
pub fn fill_interrupt_in(ep: &mut [u32], max_packet: u16, interval: u8, ring: u64) {
    ep[0] = (interval as u32) << 16;
    ep[1] = 3 << 1 | ep_type::INTERRUPT_IN << 3 | (max_packet as u32) << 16;
    let dq = ring | 1;
    ep[2] = dq as u32;
    ep[3] = (dq >> 32) as u32;
    // Average TRB length and max ESIT payload (the packet size).
    ep[4] = (max_packet as u32).max(1) | (max_packet as u32) << 16;
}

// ---- descriptors ------------------------------------------------------------------------

/// The fields of a device descriptor we use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDesc {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub num_configs: u8,
}

/// Parse the first 8 or 18 bytes of a device descriptor. With only 8 bytes the vendor and
/// product are 0 (enough for the control endpoint's max packet size).
pub fn parse_device(d: &[u8]) -> Option<DeviceDesc> {
    if d.len() < 8 || d[1] != 1 || d[0] < 8 {
        return None;
    }
    let (vendor, product, num_configs) = if d.len() >= 18 && d[0] >= 18 {
        (
            u16::from_le_bytes([d[8], d[9]]),
            u16::from_le_bytes([d[10], d[11]]),
            d[17],
        )
    } else {
        (0, 0, 0)
    };
    Some(DeviceDesc {
        class: d[4],
        subclass: d[5],
        protocol: d[6],
        max_packet0: d[7],
        vendor,
        product,
        num_configs,
    })
}

/// The total length a configuration descriptor claims (from its first 4+ bytes).
pub fn config_total_length(d: &[u8]) -> Option<u16> {
    if d.len() < 4 || d[1] != 2 || d[0] < 9 {
        return None;
    }
    Some(u16::from_le_bytes([d[2], d[3]]))
}

/// Which HID boot device an interface is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidKind {
    Keyboard,
    Mouse,
}

/// An interface that speaks the HID boot protocol, with its interrupt IN endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidBoot {
    /// bConfigurationValue of the configuration it is in.
    pub config: u8,
    pub interface: u8,
    pub kind: HidKind,
    /// The endpoint address (0x81 style).
    pub ep_addr: u8,
    pub max_packet: u16,
    pub interval: u8,
}

pub const MAX_HID: usize = 4;

/// Walk a configuration descriptor (with its interface and endpoint descriptors) and return the HID
/// boot interfaces (class 3, subclass 1; protocol 1 keyboard, 2 mouse) that have an interrupt IN
/// endpoint. Hostile or truncated data gives fewer (or no) results.
pub fn find_hid_boot(cfg: &[u8], out: &mut [HidBoot; MAX_HID]) -> usize {
    let mut n = 0;
    let Some(total) = config_total_length(cfg) else {
        return 0;
    };
    let cfg = &cfg[..cfg.len().min(total as usize)];
    if cfg.len() < 9 {
        return 0;
    }
    let config = cfg[5];
    let mut i = cfg[0] as usize;
    // The current interface, if it is a HID boot one, until an endpoint completes it.
    let mut cur: Option<(u8, HidKind)> = None;
    while i + 2 <= cfg.len() {
        let len = cfg[i] as usize;
        if len < 2 || i + len > cfg.len() {
            break;
        }
        let kind = cfg[i + 1];
        match kind {
            4 if len >= 9 => {
                let (num, alt, class, sub, proto) =
                    (cfg[i + 2], cfg[i + 3], cfg[i + 5], cfg[i + 6], cfg[i + 7]);
                cur = match (alt, class, sub, proto) {
                    (0, 3, 1, 1) => Some((num, HidKind::Keyboard)),
                    (0, 3, 1, 2) => Some((num, HidKind::Mouse)),
                    _ => None,
                };
            }
            5 if len >= 7 => {
                if let Some((iface, k)) = cur {
                    let addr = cfg[i + 2];
                    let attrs = cfg[i + 3];
                    // Interrupt (attrs & 3 == 3), IN (bit 7).
                    if attrs & 3 == 3 && addr & 0x80 != 0 && n < MAX_HID {
                        out[n] = HidBoot {
                            config,
                            interface: iface,
                            kind: k,
                            ep_addr: addr,
                            max_packet: u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]) & 0x7ff,
                            interval: cfg[i + 6],
                        };
                        n += 1;
                        cur = None; // one endpoint per interface is enough
                    }
                }
            }
            _ => {}
        }
        i += len;
    }
    n
}

// ---- HID boot protocol ---------------------------------------------------------------------

/// evdev code of a HID keyboard usage (usage page 0x07), 0 for none.
pub const fn usage_to_evdev(usage: u8) -> u16 {
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    match usage {
        0x04..=0x1d => LETTERS[(usage - 0x04) as usize],
        0x1e..=0x26 => (usage - 0x1e) as u16 + 2, // 1..9
        0x27 => 11,                               // 0
        0x28 => 28,
        0x29 => 1,
        0x2a => 14,
        0x2b => 15,
        0x2c => 57,
        0x2d => 12,
        0x2e => 13,
        0x2f => 26,
        0x30 => 27,
        0x31 | 0x32 => 43,
        0x33 => 39,
        0x34 => 40,
        0x35 => 41,
        0x36 => 51,
        0x37 => 52,
        0x38 => 53,
        0x39 => 58,
        0x3a..=0x43 => (usage - 0x3a) as u16 + 59, // F1..F10
        0x44 => 87,
        0x45 => 88,
        0x46 => 99,
        0x47 => 70,
        0x48 => 119,
        0x49 => 110,
        0x4a => 102,
        0x4b => 104,
        0x4c => 111,
        0x4d => 107,
        0x4e => 109,
        0x4f => 106,
        0x50 => 105,
        0x51 => 108,
        0x52 => 103,
        0x53 => 69,
        0x54 => 98,
        0x55 => 55,
        0x56 => 74,
        0x57 => 78,
        0x58 => 96,
        0x59 => 79,
        0x5a => 80,
        0x5b => 81,
        0x5c => 75,
        0x5d => 76,
        0x5e => 77,
        0x5f => 71,
        0x60 => 72,
        0x61 => 73,
        0x62 => 82,
        0x63 => 83,
        0x64 => 86,
        0x65 => 127,
        0x67 => 117,
        _ => 0,
    }
}

/// evdev codes of the eight modifier bits of the boot keyboard report's first byte: left ctrl,
/// shift, alt, meta, then the right ones.
pub const MODIFIER_EVDEV: [u16; 8] = [29, 42, 56, 125, 97, 54, 100, 126];

/// A key going down or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyChange {
    pub evdev: u16,
    /// 1 = press, 0 = release, 2 = auto-repeat (the `input::report_key` values).
    pub value: i32,
}

/// Auto-repeat: the first repeat after this many ms, then every `REPEAT_MS`.
pub const REPEAT_DELAY_MS: u64 = 500;
pub const REPEAT_MS: u64 = 33;

/// The state of one boot keyboard: the last report, and what is repeating.
#[derive(Clone, Copy, Debug)]
pub struct Keyboard {
    prev: [u8; 8],
    /// The key that repeats (the last one pressed), and when its next repeat is due (ms).
    repeat: Option<(u16, u64)>,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    pub const fn new() -> Keyboard {
        Keyboard {
            prev: [0; 8],
            repeat: None,
        }
    }

    /// Feed a boot keyboard report (8 bytes: modifiers, reserved, six key usages) at time `now_ms`;
    /// `f` gets every press and release it implies. A rollover-error report (usage 1 in
    /// every slot) is ignored; a short report is ignored.
    pub fn report(&mut self, r: &[u8], now_ms: u64, mut f: impl FnMut(KeyChange)) {
        if r.len() < 8 {
            return;
        }
        let mut cur = [0u8; 8];
        cur.copy_from_slice(&r[..8]);
        if cur[2..8].iter().all(|&u| u == 0x01) {
            return; // phantom state: too many keys down
        }
        // Modifiers first on a press (so Shift is already down when the letter arrives) and last on
        // a release.
        let (old_m, new_m) = (self.prev[0], cur[0]);
        for (bit, &code) in MODIFIER_EVDEV.iter().enumerate() {
            if new_m & (1 << bit) != 0 && old_m & (1 << bit) == 0 {
                f(KeyChange {
                    evdev: code,
                    value: 1,
                });
            }
        }
        for &u in &self.prev[2..8] {
            if u > 3 && !cur[2..8].contains(&u) {
                let e = usage_to_evdev(u);
                if e != 0 {
                    f(KeyChange { evdev: e, value: 0 });
                }
                if self.repeat.is_some_and(|(k, _)| k == e) {
                    self.repeat = None;
                }
            }
        }
        for &u in &cur[2..8] {
            if u > 3 && !self.prev[2..8].contains(&u) {
                let e = usage_to_evdev(u);
                if e != 0 {
                    f(KeyChange { evdev: e, value: 1 });
                    self.repeat = Some((e, now_ms + REPEAT_DELAY_MS));
                }
            }
        }
        for (bit, &code) in MODIFIER_EVDEV.iter().enumerate() {
            if new_m & (1 << bit) == 0 && old_m & (1 << bit) != 0 {
                f(KeyChange {
                    evdev: code,
                    value: 0,
                });
            }
        }
        self.prev = cur;
    }

    /// Call regularly (every few ms) with the time: emits auto-repeats of the held key.
    pub fn tick(&mut self, now_ms: u64, mut f: impl FnMut(KeyChange)) {
        if let Some((key, due)) = self.repeat
            && now_ms >= due
        {
            f(KeyChange {
                evdev: key,
                value: 2,
            });
            self.repeat = Some((key, now_ms + REPEAT_MS));
        }
    }

    /// The keyboard went away: release everything that was down.
    pub fn release_all(&mut self, mut f: impl FnMut(KeyChange)) {
        for &u in &self.prev[2..8] {
            let e = usage_to_evdev(u);
            if u > 3 && e != 0 {
                f(KeyChange { evdev: e, value: 0 });
            }
        }
        for (bit, &code) in MODIFIER_EVDEV.iter().enumerate() {
            if self.prev[0] & (1 << bit) != 0 {
                f(KeyChange {
                    evdev: code,
                    value: 0,
                });
            }
        }
        *self = Keyboard::new();
    }
}

/// A boot mouse report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    /// Bit 0 left, 1 right, 2 middle.
    pub buttons: u8,
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
}

/// Parse a boot mouse report: buttons, dx, dy (signed bytes), and a wheel byte if the device sends
/// one. `None` if shorter than 3 bytes.
pub fn parse_mouse(r: &[u8]) -> Option<MouseReport> {
    if r.len() < 3 {
        return None;
    }
    Some(MouseReport {
        buttons: r[0] & 0x07,
        dx: r[1] as i8 as i32,
        dy: r[2] as i8 as i32,
        wheel: r.get(3).map_or(0, |&w| w as i8 as i32),
    })
}
