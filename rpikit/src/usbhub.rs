//! USB hubs, the pure parts: the hub class requests, the hub descriptor, a port's status word, route
//! strings, which hub's transaction translator serves a low or full speed device, the slot context of a
//! hub and of a device behind one, and the per-port state machine (debounce, reset, enumerate, remove,
//! give up) driven by a clock the caller passes in, so the host tests run it with a fake one.
//!
//! The kernel (`kernel/src/usb/xhci/hub.rs`) owns the controller and does the transfers; this decides what
//! to do next from bytes and numbers alone. Everything a hub sends is distrusted: a short or
//! garbage descriptor is refused, port numbers and counts are capped, and the port machine is bounded
//! (a port that keeps failing or flapping is given up on until the cable has been out for a moment).
//! USB 2 hubs only (a SuperSpeed hub is not driven).

use crate::usb::{Setup, speed};

// ---- limits ----------------------------------------------------------------------------------

/// Most hubs between a device and the root port (a hub directly on a root port is tier 1). Deeper hubs
/// are ignored with a log line. (The route string has room for 5 tiers.)
pub const MAX_HUB_TIERS: u8 = 3;
/// Most ports of one hub that are driven (a route string nibble holds 1..=15).
pub const MAX_PORTS_PER_HUB: u8 = 15;
/// Most hub ports driven in all, over every hub of a controller.
pub const MAX_PORTS_TOTAL: u8 = 32;

/// The ports of a hub that claims `claimed`, with `used` ports of other hubs already driven: how many to
/// drive. Never more than a nibble holds, never more than the total cap leaves.
pub const fn usable_ports(claimed: u8, used: u8) -> u8 {
    let room = MAX_PORTS_TOTAL.saturating_sub(used);
    let n = if claimed > MAX_PORTS_PER_HUB { MAX_PORTS_PER_HUB } else { claimed };
    if n > room { room } else { n }
}

/// Whether a hub that would sit at `tier` (1 = on a root port) is allowed.
pub const fn hub_tier_allowed(tier: u8) -> bool {
    tier >= 1 && tier <= MAX_HUB_TIERS
}

// ---- requests --------------------------------------------------------------------------------

/// Hub class descriptor type, and the features of SetPortFeature / ClearPortFeature (USB 2.0, table 11-17).
pub mod feature {
    pub const PORT_CONNECTION: u16 = 0;
    pub const PORT_ENABLE: u16 = 1;
    pub const PORT_SUSPEND: u16 = 2;
    pub const PORT_OVER_CURRENT: u16 = 3;
    pub const PORT_RESET: u16 = 4;
    pub const PORT_POWER: u16 = 8;
    pub const PORT_LOW_SPEED: u16 = 9;
    pub const C_PORT_CONNECTION: u16 = 16;
    pub const C_PORT_ENABLE: u16 = 17;
    pub const C_PORT_SUSPEND: u16 = 18;
    pub const C_PORT_OVER_CURRENT: u16 = 19;
    pub const C_PORT_RESET: u16 = 20;
}

pub const CLASS_HUB: u8 = 9;
pub const DESC_HUB: u16 = 0x29;

/// GET_DESCRIPTOR(hub) for up to `len` bytes.
pub const fn get_hub_descriptor(len: u16) -> Setup {
    Setup {
        request_type: 0xa0,
        request: 6,
        value: DESC_HUB << 8,
        index: 0,
        length: len,
    }
}

/// GetPortStatus for `port` (1-based): four bytes, wPortStatus then wPortChange.
pub const fn get_port_status(port: u8) -> Setup {
    Setup {
        request_type: 0xa3,
        request: 0,
        value: 0,
        index: port as u16,
        length: 4,
    }
}

pub const fn set_port_feature(port: u8, feature: u16) -> Setup {
    Setup {
        request_type: 0x23,
        request: 3,
        value: feature,
        index: port as u16,
        length: 0,
    }
}

pub const fn clear_port_feature(port: u8, feature: u16) -> Setup {
    Setup {
        request_type: 0x23,
        request: 1,
        value: feature,
        index: port as u16,
        length: 0,
    }
}

/// SET_INTERFACE (a high speed hub with a protocol of 2 gets alternate setting 1 to turn on one
/// transaction translator per port).
pub const fn set_interface(interface: u8, alt: u8) -> Setup {
    Setup {
        request_type: 0x01,
        request: 11,
        value: alt as u16,
        index: interface as u16,
        length: 0,
    }
}

// ---- the hub descriptor ------------------------------------------------------------------------

/// How a hub switches the power of its ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSwitching {
    Ganged,
    PerPort,
    /// No switching (the ports are always powered).
    None,
}

/// The fields of a USB 2 hub descriptor we use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubDesc {
    pub nports: u8,
    pub characteristics: u16,
    /// bPwrOn2PwrGood in milliseconds (the descriptor counts 2 ms units).
    pub power_good_ms: u16,
    /// bHubContrCurrent, mA.
    pub current_ma: u8,
}

/// Parse a hub descriptor (7 bytes at least, type 0x29, 1..=255 ports). `None` for anything else.
pub fn parse_hub_descriptor(d: &[u8]) -> Option<HubDesc> {
    if d.len() < 7 || d[1] != DESC_HUB as u8 || (d[0] as usize) < 7 || d[2] == 0 {
        return None;
    }
    Some(HubDesc {
        nports: d[2],
        characteristics: u16::from_le_bytes([d[3], d[4]]),
        power_good_ms: d[5] as u16 * 2,
        current_ma: d[6],
    })
}

impl HubDesc {
    pub const fn power_switching(&self) -> PowerSwitching {
        match self.characteristics & 3 {
            0 => PowerSwitching::Ganged,
            1 => PowerSwitching::PerPort,
            _ => PowerSwitching::None,
        }
    }
    /// A compound device (the hub is part of a device with other parts on its ports).
    pub const fn compound(&self) -> bool {
        self.characteristics & 4 != 0
    }
    /// 0 = over-current is reported for the hub as a whole, 1 = per port, 2/3 = not reported.
    pub const fn over_current_mode(&self) -> u8 {
        ((self.characteristics >> 3) & 3) as u8
    }
    /// TT think time, the 2-bit code both the descriptor and the xHCI slot context use: 0 = 8 full speed bit
    /// times, 1 = 16, 2 = 24, 3 = 32.
    pub const fn think_time_code(&self) -> u8 {
        ((self.characteristics >> 5) & 3) as u8
    }
    pub const fn think_time_bits(&self) -> u8 {
        8 + 8 * self.think_time_code()
    }
    /// How long to wait after powering the ports before looking at them: the descriptor's time, but at
    /// least 100 ms (devices need that to be seen reliably).
    pub const fn settle_ms(&self) -> u64 {
        if self.power_good_ms < 100 { 100 } else { self.power_good_ms as u64 }
    }
}

// ---- port status -------------------------------------------------------------------------------

pub mod status {
    pub const CONNECTION: u16 = 1 << 0;
    pub const ENABLE: u16 = 1 << 1;
    pub const SUSPEND: u16 = 1 << 2;
    pub const OVER_CURRENT: u16 = 1 << 3;
    pub const RESET: u16 = 1 << 4;
    pub const POWER: u16 = 1 << 8;
    pub const LOW_SPEED: u16 = 1 << 9;
    pub const HIGH_SPEED: u16 = 1 << 10;
    /// The wPortChange bits (same positions: connection, enable, suspend, over-current, reset).
    pub const C_CONNECTION: u16 = 1 << 0;
    pub const C_ENABLE: u16 = 1 << 1;
    pub const C_SUSPEND: u16 = 1 << 2;
    pub const C_OVER_CURRENT: u16 = 1 << 3;
    pub const C_RESET: u16 = 1 << 4;
}

/// A port's wPortStatus and wPortChange.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortStatus {
    pub status: u16,
    pub change: u16,
}

/// Parse the four bytes GetPortStatus returns. `None` if shorter.
pub fn parse_port_status(d: &[u8]) -> Option<PortStatus> {
    if d.len() < 4 {
        return None;
    }
    Some(PortStatus {
        status: u16::from_le_bytes([d[0], d[1]]),
        change: u16::from_le_bytes([d[2], d[3]]),
    })
}

impl PortStatus {
    pub const fn connected(&self) -> bool {
        self.status & status::CONNECTION != 0
    }
    pub const fn enabled(&self) -> bool {
        self.status & status::ENABLE != 0
    }
    pub const fn in_reset(&self) -> bool {
        self.status & status::RESET != 0
    }
    pub const fn powered(&self) -> bool {
        self.status & status::POWER != 0
    }
    pub const fn over_current(&self) -> bool {
        self.status & status::OVER_CURRENT != 0
    }
    /// The speed of the attached device in the xHCI slot context's encoding (low, full or high).
    pub const fn speed(&self) -> u8 {
        if self.status & status::HIGH_SPEED != 0 {
            speed::HIGH
        } else if self.status & status::LOW_SPEED != 0 {
            speed::LOW
        } else {
            speed::FULL
        }
    }
    /// The ClearPortFeature selectors that clear the change bits set here (the low five bits are the only ones a
    /// USB 2 hub defines).
    pub fn change_features(&self) -> impl Iterator<Item = u16> + '_ {
        const MAP: [(u16, u16); 5] = [
            (status::C_CONNECTION, feature::C_PORT_CONNECTION),
            (status::C_ENABLE, feature::C_PORT_ENABLE),
            (status::C_SUSPEND, feature::C_PORT_SUSPEND),
            (status::C_OVER_CURRENT, feature::C_PORT_OVER_CURRENT),
            (status::C_RESET, feature::C_PORT_RESET),
        ];
        MAP.into_iter()
            .filter(move |(bit, _)| self.change & bit != 0)
            .map(|(_, f)| f)
    }
}

// ---- topology: route strings, transaction translators, slot contexts ------------------------------------

/// The route string of a device on `port` of a hub at `tier` (1 = the hub is on a root port) whose own route
/// string is `hub_route`: the port number in the nibble of that tier, the nearest hub in the lowest. Ports
/// above 15 are 15. `None` when the tier is not 1..=5.
pub const fn child_route(hub_route: u32, tier: u8, port: u8) -> Option<u32> {
    if tier < 1 || tier > 5 {
        return None;
    }
    let p = if port > 15 { 15 } else { port } as u32;
    Some(hub_route | p << (4 * (tier as u32 - 1)))
}

/// The hub a low or full speed device is served by (xHCI 6.2.2: Parent Hub Slot ID and Parent Port Number).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentHub {
    /// Its slot id.
    pub slot: u8,
    /// The speed it runs at.
    pub speed: u8,
    /// If it is itself low or full speed behind a high speed hub: that hub's (slot, port) serving it.
    pub tt: Option<(u8, u8)>,
}

/// The (slot id, port) of the high speed hub whose translator serves a device of `dev_speed` on `port` of
/// `parent`, or `None` (a high speed device, a device on a root port, or no high speed hub above). A device behind
/// a full speed hub that is behind a high speed hub is served by that high speed hub, at the port the full speed hub is on.
pub fn tt_for(dev_speed: u8, parent: Option<ParentHub>, port: u8) -> Option<(u8, u8)> {
    if dev_speed != speed::LOW && dev_speed != speed::FULL {
        return None;
    }
    let p = parent?;
    if p.speed == speed::HIGH {
        Some((p.slot, port))
    } else {
        p.tt
    }
}

/// What a slot context is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SlotParams {
    pub route: u32,
    pub speed: u8,
    /// The last endpoint context in use (its device context index).
    pub entries: u8,
    pub root_port: u8,
    /// A hub: (ports, multi-TT, think time code).
    pub hub: Option<(u8, bool, u8)>,
    /// The translator serving a low or full speed device: (hub slot id, port).
    pub tt: Option<(u8, u8)>,
}

/// Write the whole slot context (8 dwords are enough, the rest of a 64 byte context stays as it is).
pub fn fill_slot_full(slot: &mut [u32], p: &SlotParams) {
    let (hub, ports, mtt, ttt) = match p.hub {
        Some((n, m, t)) => (1u32, n as u32, m as u32, t as u32 & 3),
        None => (0, 0, 0, 0),
    };
    slot[0] = (p.route & 0xf_ffff)
        | (p.speed as u32 & 0xf) << 20
        | mtt << 25
        | hub << 26
        | (p.entries as u32 & 0x1f) << 27;
    slot[1] = (p.root_port as u32) << 16 | ports << 24;
    let (ts, tp) = p.tt.unwrap_or((0, 0));
    slot[2] = ts as u32 | (tp as u32) << 8 | ttt << 16;
    slot[3] = 0;
}

// ---- the port state machine ------------------------------------------------------------------------

/// A device must be seen connected for this long before its port is reset (USB's connect debounce).
pub const DEBOUNCE_MS: u64 = 100;
/// A reset that has not completed by now has failed.
pub const RESET_TIMEOUT_MS: u64 = 500;
/// Tries at reset + enumerate before a port is given up on.
pub const MAX_ATTEMPTS: u8 = 3;
/// More than `STORM_FLAPS` new connections within `STORM_WINDOW_MS` is a connect/disconnect storm.
pub const STORM_WINDOW_MS: u64 = 10_000;
pub const STORM_FLAPS: u8 = 8;
/// A port in a storm is left alone this long.
pub const QUARANTINE_MS: u64 = 5_000;
/// After this many storms the port is given up on.
pub const MAX_QUARANTINES: u8 = 3;
/// A given-up port is tried again after the cable has been out this long.
pub const UNPLUG_FORGIVE_MS: u64 = 500;

/// Where a port is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing is connected.
    Empty,
    /// Something is connected, but not yet for long enough.
    Debounce { since: u64 },
    /// The port is being reset.
    Resetting { since: u64 },
    /// The device was enumerated.
    Active,
    /// Given up on until the cable has been out for a moment (`gone` = since when it has been).
    Failed { gone: Option<u64> },
    /// In a connect/disconnect storm: ignored until then.
    Quarantine { until: u64 },
}

/// Why a port was given up on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The reset never finished, or left the port disabled.
    Reset,
    /// The device would not enumerate in `MAX_ATTEMPTS` tries.
    Enumerate,
    /// The device is not one we drive (not a failure: quiet).
    Ignored,
    OverCurrent,
    /// Too many connect/disconnect storms.
    Storm,
}

/// What the driver should do after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// SetPortFeature(PORT_RESET).
    Reset,
    /// The reset finished with the port enabled: address the device, which runs at `speed` (the slot context encoding).
    Enumerate { speed: u8 },
    /// The device went away: free its slot (if it has one).
    Remove,
    /// Stop trying; `remove` = free the device's slot (if it has one) first.
    GiveUp { why: Why, remove: bool },
    /// A connect/disconnect storm: the port is left alone for a while.
    Storm,
}

/// How the driver's attempt to address and configure a device went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    /// It enumerated, and is nothing we drive (so the slot was freed).
    Ignored,
}

/// One port of a hub.
#[derive(Clone, Copy, Debug)]
pub struct PortMachine {
    pub phase: Phase,
    attempts: u8,
    flaps: u8,
    window_start: u64,
    quarantines: u8,
}

impl Default for PortMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl PortMachine {
    pub const fn new() -> PortMachine {
        PortMachine {
            phase: Phase::Empty,
            attempts: 0,
            flaps: 0,
            window_start: 0,
            quarantines: 0,
        }
    }

    /// True while the port is changing state and wants looking at often.
    pub const fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Debounce { .. } | Phase::Resetting { .. }
        )
    }

    fn give_up(&mut self, why: Why, remove: bool) -> Action {
        self.phase = Phase::Failed { gone: None };
        Action::GiveUp { why, remove }
    }

    /// One failed try (a reset that did not work, an enumeration that failed): try again after a new debounce,
    /// or give up.
    fn failed_try(&mut self, why: Why, now: u64, remove: bool) -> Action {
        self.attempts += 1;
        if self.attempts >= MAX_ATTEMPTS {
            self.give_up(why, remove)
        } else {
            self.phase = Phase::Debounce { since: now };
            if remove { Action::Remove } else { Action::None }
        }
    }

    /// Feed the port's status read at time `now` (ms). `now` must not go backwards.
    pub fn step(&mut self, st: PortStatus, now: u64) -> Action {
        let connected = st.connected();
        // Over-current: the hub has cut the port's power. Whatever was there is gone.
        if st.over_current() && !matches!(self.phase, Phase::Failed { .. } | Phase::Quarantine { .. }) {
            let remove = matches!(self.phase, Phase::Active);
            return self.give_up(Why::OverCurrent, remove);
        }
        match self.phase {
            Phase::Empty => {
                if connected {
                    if now.saturating_sub(self.window_start) > STORM_WINDOW_MS {
                        self.window_start = now;
                        self.flaps = 0;
                    }
                    self.flaps = self.flaps.saturating_add(1);
                    if self.flaps > STORM_FLAPS {
                        self.quarantines += 1;
                        if self.quarantines >= MAX_QUARANTINES {
                            return self.give_up(Why::Storm, false);
                        }
                        self.phase = Phase::Quarantine { until: now + QUARANTINE_MS };
                        return Action::Storm;
                    }
                    self.phase = Phase::Debounce { since: now };
                }
                Action::None
            }
            Phase::Debounce { since } => {
                if !connected {
                    self.phase = Phase::Empty;
                    self.attempts = 0;
                } else if st.change & status::C_CONNECTION != 0 {
                    // It bounced since the last look: start counting again.
                    self.phase = Phase::Debounce { since: now };
                } else if now.saturating_sub(since) >= DEBOUNCE_MS {
                    self.phase = Phase::Resetting { since: now };
                    return Action::Reset;
                }
                Action::None
            }
            Phase::Resetting { since } => {
                if !connected {
                    self.phase = Phase::Empty;
                    self.attempts = 0;
                    Action::None
                } else if st.change & status::C_RESET != 0 || (!st.in_reset() && st.enabled()) {
                    if st.enabled() && !st.in_reset() {
                        self.phase = Phase::Active;
                        Action::Enumerate { speed: st.speed() }
                    } else {
                        self.failed_try(Why::Reset, now, false)
                    }
                } else if now.saturating_sub(since) > RESET_TIMEOUT_MS {
                    self.failed_try(Why::Reset, now, false)
                } else {
                    Action::None
                }
            }
            Phase::Active => {
                if !connected {
                    self.phase = Phase::Empty;
                    self.attempts = 0;
                    Action::Remove
                } else if !st.enabled() {
                    // The hub disabled the port (an error on it): the device is gone for now; look again.
                    self.phase = Phase::Empty;
                    Action::Remove
                } else {
                    Action::None
                }
            }
            Phase::Failed { gone } => {
                if connected || st.over_current() {
                    // (A port that is still over-current stays given up on: one log line, not one a second.)
                    self.phase = Phase::Failed { gone: None };
                } else {
                    let g = gone.unwrap_or(now);
                    if now.saturating_sub(g) >= UNPLUG_FORGIVE_MS {
                        *self = PortMachine::new();
                        self.window_start = now;
                    } else {
                        self.phase = Phase::Failed { gone: Some(g) };
                    }
                }
                Action::None
            }
            Phase::Quarantine { until } => {
                if now >= until {
                    self.phase = Phase::Empty;
                    self.window_start = now;
                    self.flaps = 0;
                }
                Action::None
            }
        }
    }

    /// Tell the machine how the `Enumerate` it asked for went.
    pub fn enumerated(&mut self, outcome: Outcome, now: u64) -> Action {
        if self.phase != Phase::Active {
            return Action::None;
        }
        match outcome {
            Outcome::Ok => {
                self.attempts = 0;
                Action::None
            }
            Outcome::Ignored => self.give_up(Why::Ignored, false),
            Outcome::Failed => self.failed_try(Why::Enumerate, now, false),
        }
    }
}
