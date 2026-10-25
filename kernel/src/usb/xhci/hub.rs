//! USB 2 hubs under the xHCI driver. A hub is addressed like any device (on a root port, or on a port of
//! another hub); then its descriptor is read, its slot context is updated (Hub, number of ports, translator think
//! time, multi-TT), every port is powered, and the ports are polled with GetPortStatus every
//! 100 ms (20 ms while something on a port is changing). What each port should do next is
//! `rpikit::usbhub::PortMachine`'s decision (debounce, reset, enumerate, remove, give up); this file carries it out and
//! logs it. A device behind a hub is addressed with its route string and, if it is low or full speed behind a
//! high speed hub, that hub's slot and port (the translator); a hub it is behind that stops answering is dropped
//! with everything on it. Hubs may nest three tiers deep; at most 32 hub ports are driven in all.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use rpikit::usb::{self as u, speed};
use rpikit::usbhub::{self as hp, Action, Outcome, PortMachine, PortStatus, Why};

use super::{Attach, Controller, Error, now_ms, speed_name};
use crate::kprintln;

/// How often a hub's ports are looked at, and how often while one is being debounced or reset.
const POLL_MS: u64 = 100;
const POLL_BUSY_MS: u64 = 20;
/// A hub request gets this long (a hub answers in a frame or two).
const REQUEST_MS: u64 = 300;
/// Failed requests in a row before a hub is dropped (a hub that does not answer at all is dropped at once).
const MAX_ERRORS: u8 = 3;
/// A device may take this long after its port reset before it answers (USB's reset recovery is 10 ms).
const RESET_RECOVERY_MS: u64 = 10;

pub(super) struct HubPort {
    m: PortMachine,
    /// The slot id of the device on it (0: none).
    pub(super) child: u8,
    /// How often the port has been found unpowered and powered again.
    power_tries: u8,
}

pub(super) struct Hub {
    /// `vendor:product`, for the log.
    name: String,
    pub(super) ports: Vec<HubPort>,
    next_poll: u64,
    errors: u8,
}

impl Controller {
    fn slot_index(&self, id: u8) -> Option<usize> {
        self.slots.iter().position(|s| s.id == id)
    }

    fn hub_mut(&mut self, id: u8) -> Option<&mut Hub> {
        self.slots
            .iter_mut()
            .find(|s| s.id == id)
            .and_then(|s| s.hub.as_mut())
    }

    /// Hub ports in use over every hub.
    fn hub_ports_in_use(&self) -> u8 {
        self.slots
            .iter()
            .filter_map(|s| s.hub.as_ref())
            .map(|h| h.ports.len())
            .sum::<usize>()
            .min(255) as u8
    }

    /// The device in slot index `si` is a hub (descriptors read, not yet configured): configure it, power its ports, start
    /// looking at them.
    pub(super) fn setup_hub(
        &mut self,
        si: usize,
        vendor: u16,
        product: u16,
        protocol: u8,
        config: u8,
    ) -> Result<(), Error> {
        let (id, depth, spd) = {
            let s = &self.slots[si];
            (s.id, s.depth, s.speed)
        };
        let path = self.slots[si].path.clone();
        let name = format!("{vendor:04x}:{product:04x}");
        let tier = depth + 1;
        if !hp::hub_tier_allowed(tier) {
            kprintln!(
                "[usb] hub {name} on port {path}: it would be {tier} hubs deep, only {} are driven: ignored",
                hp::MAX_HUB_TIERS
            );
            return Err(Error::Ignored);
        }
        if spd == speed::SUPER {
            kprintln!("[usb] hub {name} on port {path}: a SuperSpeed hub, not driven: ignored");
            return Err(Error::Ignored);
        }
        self.control(si, u::req::set_configuration(config))?;
        // The hub descriptor: three tries (hubs just out of reset sometimes stall the first).
        let mut desc = None;
        for attempt in 0..3 {
            if attempt > 0 {
                crate::sched::sleep_ms(50);
            }
            match self.control(si, hp::get_hub_descriptor(16)) {
                Ok(n) => {
                    let mut b = [0u8; 16];
                    self.slots[si].buf.read_bytes(0, &mut b[..n.min(16)]);
                    desc = hp::parse_hub_descriptor(&b[..n.min(16)]);
                    if desc.is_some() {
                        break;
                    }
                }
                Err(Error::Timeout) => return Err(Error::Timeout),
                Err(_) => {}
            }
        }
        let Some(desc) = desc else {
            kprintln!("[usb] hub {name} on port {path}: no usable hub descriptor: ignored");
            return Err(Error::Device("bad hub descriptor"));
        };
        let nports = hp::usable_ports(desc.nports, self.hub_ports_in_use());
        if nports == 0 {
            kprintln!(
                "[usb] hub {name} on port {path}: {} ports, but {} hub ports are driven already: ignored",
                desc.nports,
                hp::MAX_PORTS_TOTAL
            );
            return Err(Error::Ignored);
        }
        // A high speed hub with one translator per port (protocol 2) is switched to that with alternate setting 1.
        let mut mtt = false;
        if spd == speed::HIGH && protocol == 2 {
            mtt = self.control(si, hp::set_interface(0, 1)).is_ok();
        }
        // The slot context learns it is a hub.
        self.slots[si].hub_ctx = Some((nports, mtt, desc.think_time_code()));
        {
            let slot = &self.slots[si];
            for i in 0..u::input_ctx_dwords(self.csz) {
                slot.in_ctx.write32(i * 4, 0);
            }
            self.put(&slot.in_ctx, 0, &[0, 1]);
            let mut sl = [0u32; 8];
            hp::fill_slot_full(&mut sl, &self.slot_params(si, 1));
            self.put(&slot.in_ctx, u::input_slot_offset(self.csz), &sl);
            slot.in_ctx.sync(0, 4096);
        }
        let in_pa = self.slots[si].in_ctx.pa();
        self.command(u::configure_endpoint(in_pa, id, true))?;
        // Power every port.
        let mut powered = 0;
        for p in 1..=nports {
            match self.control_t(si, hp::set_port_feature(p, hp::feature::PORT_POWER), REQUEST_MS) {
                Ok(_) => powered += 1,
                Err(Error::Timeout) => return Err(Error::Timeout),
                Err(e) => kprintln!("[usb] hub {name} on port {path}: port {p}: cannot power it: {e:?}"),
            }
        }
        if powered == 0 {
            return Err(Error::Device("no port of the hub could be powered"));
        }
        let ports = (0..nports)
            .map(|_| HubPort {
                m: PortMachine::new(),
                child: 0,
                power_tries: 0,
            })
            .collect();
        self.slots[si].hub = Some(Hub {
            name: name.clone(),
            ports,
            // The ports are looked at once the power is good.
            next_poll: now_ms() + desc.settle_ms(),
            errors: 0,
        });
        kprintln!(
            "[usb] hub {name} on port {path}: {nports} ports ({} speed, power switching {}, power good after {} ms, over-current {}, {} bit times think time, {})",
            speed_name(spd),
            match desc.power_switching() {
                hp::PowerSwitching::Ganged => "ganged",
                hp::PowerSwitching::PerPort => "per port",
                hp::PowerSwitching::None => "none",
            },
            desc.power_good_ms,
            match desc.over_current_mode() {
                0 => "global",
                1 => "per port",
                _ => "not reported",
            },
            desc.think_time_bits(),
            if mtt { "a translator per port" } else { "one translator" },
        );
        if nports < desc.nports {
            kprintln!(
                "[usb] hub {name} on port {path}: it claims {} ports, only the first {nports} are driven",
                desc.nports
            );
        }
        Ok(())
    }

    /// Look at the ports of every hub that is due.
    pub(super) fn poll_hubs(&mut self) {
        let now = now_ms();
        let due: Vec<u8> = self
            .slots
            .iter()
            .filter(|s| s.hub.as_ref().is_some_and(|h| now >= h.next_poll))
            .map(|s| s.id)
            .collect();
        for id in due {
            self.poll_hub(id);
        }
    }

    /// A request to hub `id` (the control endpoint of its slot).
    fn hub_request(&mut self, id: u8, s: u::Setup) -> Result<usize, Error> {
        let si = self.slot_index(id).ok_or(Error::Device("the hub is gone"))?;
        self.control_t(si, s, REQUEST_MS)
    }

    fn hub_port_status(&mut self, id: u8, port: u8) -> Result<PortStatus, Error> {
        let n = self.hub_request(id, hp::get_port_status(port))?;
        let si = self.slot_index(id).ok_or(Error::Device("the hub is gone"))?;
        let mut b = [0u8; 4];
        self.slots[si].buf.read_bytes(0, &mut b[..n.min(4)]);
        hp::parse_port_status(&b[..n.min(4)]).ok_or(Error::Device("short port status"))
    }

    /// A request to hub `id` failed: a hub that does not answer at all is dropped, one that keeps failing after a few tries.
    fn hub_failed(&mut self, id: u8, e: Error) {
        let timeout = matches!(e, Error::Timeout);
        let Some(h) = self.hub_mut(id) else { return };
        h.errors += 1;
        if !timeout && h.errors < MAX_ERRORS {
            return;
        }
        let name = h.name.clone();
        let path = self
            .slot_index(id)
            .map(|i| self.slots[i].path.clone())
            .unwrap_or_default();
        kprintln!(
            "[usb] hub {name} on port {path}: {} ({e:?}): dropped, with everything on it",
            if timeout { "does not answer" } else { "keeps failing requests" }
        );
        self.remove_tree(id);
    }

    fn poll_hub(&mut self, id: u8) {
        let Some(n) = self.hub_mut(id).map(|h| h.ports.len() as u8) else {
            return;
        };
        let mut busy = false;
        for port in 1..=n {
            if self.hub_mut(id).is_none() {
                return; // dropped meanwhile
            }
            let st = match self.hub_port_status(id, port) {
                Ok(s) => s,
                Err(e) => return self.hub_failed(id, e),
            };
            // Acknowledge whatever changed, or the hub keeps reporting it.
            for f in st.change_features() {
                if let Err(e) = self.hub_request(id, hp::clear_port_feature(port, f)) {
                    return self.hub_failed(id, e);
                }
            }
            if let Some(h) = self.hub_mut(id) {
                h.errors = 0;
            }
            self.check_power(id, port, st);
            self.port_step(id, port, st);
            busy |= self
                .hub_mut(id)
                .is_some_and(|h| h.ports[port as usize - 1].m.busy());
        }
        if let Some(h) = self.hub_mut(id) {
            h.next_poll = now_ms() + if busy { POLL_BUSY_MS } else { POLL_MS };
        }
    }

    /// A port that reports no power after SetPortFeature(PORT_POWER) is powered again (up to three times); then it is
    /// said once that this hub does not seem to switch the port.
    fn check_power(&mut self, id: u8, port: u8, st: PortStatus) {
        let Some(h) = self.hub_mut(id) else { return };
        let p = &mut h.ports[port as usize - 1];
        if st.powered() {
            p.power_tries = 0;
            return;
        }
        if st.connected() {
            return;
        }
        p.power_tries = p.power_tries.saturating_add(1);
        let tries = p.power_tries;
        let name = h.name.clone();
        let path = self.slots[self.slot_index(id).unwrap_or(0)].path.clone();
        if tries <= 3 {
            if tries == 1 {
                kprintln!("[usb] hub {name} on port {path}: port {port} reports no power: switching it on again");
            }
            if let Err(e) = self.hub_request(id, hp::set_port_feature(port, hp::feature::PORT_POWER)) {
                self.hub_failed(id, e);
            }
        } else if tries == 4 {
            kprintln!(
                "[usb] hub {name} on port {path}: port {port} stays unpowered after three tries (no power switching on this port?): nothing will be seen there"
            );
        }
    }

    /// Run the port machine of `port` of hub `id` on a status just read, and do what it says.
    fn port_step(&mut self, id: u8, port: u8, st: PortStatus) {
        let now = now_ms();
        let Some(h) = self.hub_mut(id) else { return };
        let act = h.ports[port as usize - 1].m.step(st, now);
        if act == Action::None {
            return;
        }
        let name = h.name.clone();
        let Some(si) = self.slot_index(id) else { return };
        let at = format!("{}.{port}", self.slots[si].path);
        match act {
            Action::None => {}
            Action::Reset => {
                kprintln!("[usb] port {at}: something connected, resetting the port");
                if let Err(e) = self.hub_request(id, hp::set_port_feature(port, hp::feature::PORT_RESET)) {
                    self.hub_failed(id, e);
                }
            }
            Action::Enumerate { speed: spd } => self.hub_enumerate(id, port, spd),
            Action::Remove => self.hub_remove_child(id, port, &at),
            Action::GiveUp { why, remove } => {
                if remove {
                    self.hub_remove_child(id, port, &at);
                }
                match why {
                    Why::Reset => kprintln!(
                        "[usb] port {at}: the reset did not finish or left the port disabled, {} tries: giving up on it until it is unplugged",
                        hp::MAX_ATTEMPTS
                    ),
                    Why::Enumerate => kprintln!(
                        "[usb] port {at}: the device would not enumerate in {} tries: giving up on it until it is unplugged",
                        hp::MAX_ATTEMPTS
                    ),
                    Why::Ignored => {}
                    Why::OverCurrent => kprintln!(
                        "[usb] hub {name} on port {}: port {port} reports over-current, the hub cut its power: the port stays off while it lasts",
                        self.slots[si].path
                    ),
                    Why::Storm => kprintln!(
                        "[usb] port {at}: it keeps connecting and disconnecting: giving up on it until it is unplugged"
                    ),
                }
            }
            Action::Storm => kprintln!(
                "[usb] port {at}: it connects and disconnects over and over: ignoring the port for {} s",
                hp::QUARANTINE_MS / 1000
            ),
        }
    }

    fn hub_remove_child(&mut self, id: u8, port: u8, at: &str) {
        let child = self
            .hub_mut(id)
            .map(|h| core::mem::take(&mut h.ports[port as usize - 1].child))
            .unwrap_or(0);
        if child != 0 {
            self.remove_tree(child);
        } else {
            kprintln!("[usb] port {at}: disconnected");
        }
    }

    /// The reset of `port` of hub `id` finished with the port enabled: address what is there.
    fn hub_enumerate(&mut self, id: u8, port: u8, spd: u8) {
        let Some(si) = self.slot_index(id) else { return };
        let parent = {
            let s = &self.slots[si];
            (s.path.clone(), s.route, s.depth, s.speed, s.tt, s.port)
        };
        let (path, route, depth, hub_speed, hub_tt, root_port) = parent;
        let Some(route) = hp::child_route(route, depth + 1, port) else {
            kprintln!("[usb] port {path}.{port}: too many hubs in a row for a route string: ignored");
            self.finish_enumerate(id, port, Outcome::Ignored);
            return;
        };
        let tt = hp::tt_for(
            spd,
            Some(hp::ParentHub {
                slot: id,
                speed: hub_speed,
                tt: hub_tt,
            }),
            port,
        );
        // The device needs a moment after its reset.
        crate::sched::sleep_ms(RESET_RECOVERY_MS);
        let at = format!("{path}.{port}");
        let r = self.enumerate_device(Attach {
            root_port,
            parent: id,
            route,
            depth: depth + 1,
            speed: spd,
            tt,
            path: at.clone(),
        });
        let outcome = match r {
            Ok(child) => {
                if let Some(h) = self.hub_mut(id) {
                    h.ports[port as usize - 1].child = child;
                    Outcome::Ok
                } else {
                    // The hub was dropped meanwhile.
                    self.remove_tree(child);
                    Outcome::Failed
                }
            }
            Err(Error::Ignored) => Outcome::Ignored,
            Err(e) => {
                kprintln!("[usb] port {at}: {} speed device: {e:?}", speed_name(spd));
                Outcome::Failed
            }
        };
        self.finish_enumerate(id, port, outcome);
    }

    /// Tell the port machine how the enumeration went, and act on its answer.
    fn finish_enumerate(&mut self, id: u8, port: u8, outcome: Outcome) {
        let now = now_ms();
        let Some(h) = self.hub_mut(id) else { return };
        let act = h.ports[port as usize - 1].m.enumerated(outcome, now);
        if let Action::GiveUp { why, .. } = act {
            let Some(si) = self.slot_index(id) else { return };
            let at = format!("{}.{port}", self.slots[si].path);
            if why == Why::Enumerate {
                kprintln!(
                    "[usb] port {at}: the device would not enumerate in {} tries: giving up on it until it is unplugged",
                    hp::MAX_ATTEMPTS
                );
            }
        }
    }
}
