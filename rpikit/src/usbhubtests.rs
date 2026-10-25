//! Host tests of `usbhub`: requests, the hub descriptor, port status, route strings, transaction translators,
//! slot contexts, and the port state machine run with a fake clock, plus fuzzing of everything a hub says.

use crate::usb::{self, Setup, speed, trb_type};
use crate::usbhub::*;
use std::vec::Vec;

fn rng(s: &mut u64) -> u64 {
    // xorshift64*
    *s ^= *s >> 12;
    *s ^= *s << 25;
    *s ^= *s >> 27;
    s.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

fn wire(s: Setup) -> [u8; 8] {
    s.param().to_le_bytes()
}

#[test]
fn hub_class_requests_are_on_the_wire_as_usb_2_says() {
    // GET_DESCRIPTOR(hub, 0x29) class request to the device, 16 bytes.
    assert_eq!(wire(get_hub_descriptor(16)), [0xa0, 6, 0, 0x29, 0, 0, 16, 0]);
    // GetPortStatus: class request to "other" (the port), IN, 4 bytes, wIndex = port.
    assert_eq!(wire(get_port_status(3)), [0xa3, 0, 0, 0, 3, 0, 4, 0]);
    assert!(get_port_status(3).is_in());
    // SetPortFeature(PORT_POWER) on port 2, ClearPortFeature(C_PORT_CONNECTION) on port 4.
    assert_eq!(wire(set_port_feature(2, feature::PORT_POWER)), [0x23, 3, 8, 0, 2, 0, 0, 0]);
    assert_eq!(wire(clear_port_feature(4, feature::C_PORT_CONNECTION)), [0x23, 1, 16, 0, 4, 0, 0, 0]);
    assert_eq!(wire(set_port_feature(1, feature::PORT_RESET))[2], 4);
    assert_eq!(wire(set_interface(0, 1)), [0x01, 11, 1, 0, 0, 0, 0, 0]);
    assert!(!set_port_feature(1, 8).is_in());
}

#[test]
fn reset_endpoint_and_set_tr_dequeue_commands() {
    let t = usb::reset_endpoint(5, 1, true);
    assert_eq!(t.kind(), trb_type::RESET_ENDPOINT);
    assert_eq!(t.control, 14 << 10 | 1 << 16 | 5 << 24 | 1);
    let t = usb::set_tr_dequeue(0x1234_5670, true, 5, 1, true);
    assert_eq!(t.kind(), trb_type::SET_TR_DEQUEUE);
    assert_eq!(t.param, 0x1234_5671);
    assert_eq!(t.control, 16 << 10 | 1 << 16 | 5 << 24 | 1);
    // The low bits of the pointer are the cycle state, not address.
    assert_eq!(usb::set_tr_dequeue(0x1234_567f, false, 1, 3, false).param, 0x1234_5670);
}

#[test]
fn hub_descriptor_of_a_four_port_hub() {
    // 9 bytes: length, type, 4 ports, individual power + per-port over-current + think time 2, 50 x 2 ms, 100 mA, removable, power mask.
    let d = [9, 0x29, 4, 0x01 | 1 << 3 | 2 << 5, 0, 50, 100, 0, 0xff];
    let h = parse_hub_descriptor(&d).unwrap();
    assert_eq!(h.nports, 4);
    assert_eq!(h.power_switching(), PowerSwitching::PerPort);
    assert_eq!(h.over_current_mode(), 1);
    assert_eq!(h.think_time_code(), 2);
    assert_eq!(h.think_time_bits(), 24);
    assert_eq!(h.power_good_ms, 100);
    assert_eq!(h.settle_ms(), 100);
    assert_eq!(h.current_ma, 100);
    assert!(!h.compound());
    // Ganged, none, compound, a long power-on time.
    let g = parse_hub_descriptor(&[7, 0x29, 7, 0x04, 0, 200, 0]).unwrap();
    assert_eq!(g.power_switching(), PowerSwitching::Ganged);
    assert!(g.compound());
    assert_eq!(g.settle_ms(), 400);
    assert_eq!(parse_hub_descriptor(&[7, 0x29, 7, 0x02, 0, 0, 0]).unwrap().power_switching(), PowerSwitching::None);
    // A short settle time is raised to 100 ms.
    assert_eq!(parse_hub_descriptor(&[7, 0x29, 1, 0, 0, 1, 0]).unwrap().settle_ms(), 100);
}

#[test]
fn hub_descriptor_refuses_garbage() {
    assert!(parse_hub_descriptor(&[]).is_none());
    assert!(parse_hub_descriptor(&[9, 0x29, 4, 0, 0, 0]).is_none(), "too short");
    assert!(parse_hub_descriptor(&[9, 0x2a, 4, 0, 0, 0, 0]).is_none(), "a SuperSpeed hub descriptor");
    assert!(parse_hub_descriptor(&[9, 1, 4, 0, 0, 0, 0]).is_none(), "a device descriptor");
    assert!(parse_hub_descriptor(&[9, 0x29, 0, 0, 0, 0, 0]).is_none(), "no ports");
    assert!(parse_hub_descriptor(&[3, 0x29, 4, 0, 0, 0, 0]).is_none(), "claims to be 3 bytes long");
}

#[test]
fn port_status_bits_speed_and_changes() {
    let st = parse_port_status(&[0x03, 0x01, 0x01, 0x00]).unwrap();
    assert_eq!(st.status, 0x0103);
    assert!(st.connected() && st.enabled() && st.powered());
    assert!(!st.in_reset() && !st.over_current());
    assert_eq!(st.speed(), speed::FULL);
    assert_eq!(st.change_features().collect::<Vec<_>>(), [feature::C_PORT_CONNECTION]);
    // Low speed (bit 9), high speed (bit 10); high wins if a broken hub sets both.
    assert_eq!(PortStatus { status: 0x0303, change: 0 }.speed(), speed::LOW);
    assert_eq!(PortStatus { status: 0x0503, change: 0 }.speed(), speed::HIGH);
    assert_eq!(PortStatus { status: 0x0703, change: 0 }.speed(), speed::HIGH);
    // Several change bits clear in order; bits a USB 2 hub does not define are ignored.
    let st = PortStatus { status: 0, change: 0x1f | 0xffe0 };
    assert_eq!(
        st.change_features().collect::<Vec<_>>(),
        [16, 17, 18, 19, 20]
    );
    assert!(parse_port_status(&[1, 2, 3]).is_none());
    // Reset and over-current bits.
    let st = parse_port_status(&[0x1b, 0x01, 0x10, 0x00]).unwrap();
    assert!(st.in_reset() && st.over_current());
    assert_eq!(st.change_features().collect::<Vec<_>>(), [feature::C_PORT_RESET]);
}

#[test]
fn route_strings_put_the_nearest_hub_in_the_lowest_nibble() {
    // A device on port 3 of a hub that is on a root port (tier 1): route 0x3.
    assert_eq!(child_route(0, 1, 3), Some(0x3));
    // That hub's port 4 holds another hub (tier 2, route 0x4); a device on its port 2: 0x24.
    assert_eq!(child_route(0x4, 2, 2), Some(0x24));
    // A third tier: port 1 of a hub with route 0x24 (tier 3) -> 0x124.
    assert_eq!(child_route(0x24, 3, 1), Some(0x124));
    // Port numbers above 15 become 15; tier 6 does not exist; tier 0 is a root port, which has no route.
    assert_eq!(child_route(0, 1, 40), Some(0xf));
    assert_eq!(child_route(0x1_2345, 6, 1), None);
    assert_eq!(child_route(0, 0, 1), None);
    assert_eq!(child_route(0x1234, 5, 9), Some(0x9_1234));
}

#[test]
fn which_hubs_translator_serves_a_device() {
    let hs = ParentHub { slot: 2, speed: speed::HIGH, tt: None };
    // A low or full speed device on port 3 of a high speed hub: that hub, that port.
    assert_eq!(tt_for(speed::LOW, Some(hs), 3), Some((2, 3)));
    assert_eq!(tt_for(speed::FULL, Some(hs), 1), Some((2, 1)));
    // High speed devices have none; neither does anything on a root port.
    assert_eq!(tt_for(speed::HIGH, Some(hs), 3), None);
    assert_eq!(tt_for(speed::LOW, None, 3), None);
    // Behind a full speed hub that sits on port 4 of the high speed hub: still that hub, at port 4.
    let fs = ParentHub { slot: 3, speed: speed::FULL, tt: Some((2, 4)) };
    assert_eq!(tt_for(speed::LOW, Some(fs), 2), Some((2, 4)));
    // A full speed hub on a root port has no translator above it.
    let fs_root = ParentHub { slot: 3, speed: speed::FULL, tt: None };
    assert_eq!(tt_for(speed::LOW, Some(fs_root), 2), None);
}

#[test]
fn slot_context_of_a_low_speed_keyboard_behind_a_high_speed_hub() {
    // Root port 1 holds a high speed hub (slot 1); its port 3 a low speed keyboard.
    let mut s = [0xdead_beefu32; 8];
    fill_slot_full(
        &mut s,
        &SlotParams {
            route: child_route(0, 1, 3).unwrap(),
            speed: speed::LOW,
            entries: 3,
            root_port: 1,
            hub: None,
            tt: Some((1, 3)),
        },
    );
    assert_eq!(s[0], 3 | 2 << 20 | 3 << 27);
    assert_eq!(s[1], 1 << 16);
    assert_eq!(s[2], 1 | 3 << 8);
    assert_eq!(s[3], 0);
    // The rest is untouched.
    assert_eq!(s[4], 0xdead_beef);
}

#[test]
fn slot_context_of_a_hub() {
    let mut s = [0u32; 8];
    fill_slot_full(
        &mut s,
        &SlotParams {
            route: 0,
            speed: speed::HIGH,
            entries: 1,
            root_port: 1,
            hub: Some((4, true, 2)),
            tt: None,
        },
    );
    // Hub (bit 26), multi-TT (bit 25), 4 ports (31:24 of dword 1), TT think time 2 (17:16 of dword 2).
    assert_eq!(s[0], 3 << 20 | 1 << 25 | 1 << 26 | 1 << 27);
    assert_eq!(s[1], 1 << 16 | 4 << 24);
    assert_eq!(s[2], 2 << 16);
    // A hub two tiers down on a full speed link with a single translator: route 0x4, a parent (slot 2, port 4).
    fill_slot_full(
        &mut s,
        &SlotParams {
            route: 0x4,
            speed: speed::FULL,
            entries: 1,
            root_port: 2,
            hub: Some((7, false, 0)),
            tt: Some((2, 4)),
        },
    );
    assert_eq!(s[0], 4 | 1 << 20 | 1 << 26 | 1 << 27);
    assert_eq!(s[1], 2 << 16 | 7 << 24);
    assert_eq!(s[2], 2 | 4 << 8);
}

#[test]
fn slot_context_matches_the_root_port_one_the_driver_already_used() {
    // For a device on a root port nothing of the hub fields is set: the old `fill_slot` layout.
    for spd in [speed::LOW, speed::FULL, speed::HIGH, speed::SUPER] {
        for entries in [1u8, 3, 31] {
            for port in [1u8, 2, 5] {
                let mut a = [0u32; 8];
                let mut b = [0u32; 8];
                usb::fill_slot(&mut a, spd, entries, port);
                fill_slot_full(
                    &mut b,
                    &SlotParams { route: 0, speed: spd, entries, root_port: port, hub: None, tt: None },
                );
                assert_eq!(a, b);
            }
        }
    }
}

#[test]
fn port_and_tier_caps() {
    assert_eq!(usable_ports(4, 0), 4);
    assert_eq!(usable_ports(255, 0), MAX_PORTS_PER_HUB);
    assert_eq!(usable_ports(15, 25), 7);
    assert_eq!(usable_ports(4, 32), 0);
    assert_eq!(usable_ports(4, 200), 0);
    assert!(hub_tier_allowed(1) && hub_tier_allowed(3));
    assert!(!hub_tier_allowed(4) && !hub_tier_allowed(0));
}

// ---- the port state machine ----------------------------------------------------------------------

const GONE: PortStatus = PortStatus { status: status::POWER, change: 0 };
fn here(change: u16) -> PortStatus {
    PortStatus { status: status::POWER | status::CONNECTION, change }
}
fn reset_done(low: bool) -> PortStatus {
    PortStatus {
        status: status::POWER | status::CONNECTION | status::ENABLE | if low { status::LOW_SPEED } else { 0 },
        change: status::C_RESET,
    }
}

/// Drive a steady port (status `st`) at 20 ms steps from `t` for `span` ms, returning the first
/// action that is not `None`.
fn run_until(m: &mut PortMachine, st: PortStatus, t: &mut u64, span: u64) -> Option<Action> {
    let until = *t + span;
    while *t < until {
        let a = m.step(st, *t);
        *t += 20;
        if a != Action::None {
            return Some(a);
        }
    }
    None
}

#[test]
fn a_low_speed_keyboard_goes_from_connected_to_enumerated_and_back() {
    let mut m = PortMachine::new();
    let mut t = 1000;
    assert_eq!(m.step(GONE, t), Action::None);
    // It connects (with the change bit, which the driver then clears).
    assert_eq!(m.step(here(status::C_CONNECTION), t), Action::None);
    assert!(m.busy());
    // Not reset before the debounce time.
    assert_eq!(m.step(here(0), t + DEBOUNCE_MS - 1), Action::None);
    assert_eq!(m.step(here(0), t + DEBOUNCE_MS), Action::Reset);
    t += DEBOUNCE_MS;
    // Still in reset: nothing.
    let in_reset = PortStatus { status: status::POWER | status::CONNECTION | status::RESET, change: 0 };
    assert_eq!(m.step(in_reset, t + 10), Action::None);
    // Reset done, low speed.
    assert_eq!(m.step(reset_done(true), t + 20), Action::Enumerate { speed: speed::LOW });
    assert_eq!(m.enumerated(Outcome::Ok, t + 40), Action::None);
    assert_eq!(m.phase, Phase::Active);
    assert!(!m.busy());
    // Steady for a long while.
    let steady = PortStatus { status: status::POWER | status::CONNECTION | status::ENABLE | status::LOW_SPEED, change: 0 };
    for i in 0..100 {
        assert_eq!(m.step(steady, t + 100 + i * 20), Action::None);
    }
    // Unplugged.
    assert_eq!(m.step(GONE, t + 5000), Action::Remove);
    assert_eq!(m.phase, Phase::Empty);
    assert_eq!(m.step(GONE, t + 5020), Action::None);
}

#[test]
fn a_bouncing_connection_restarts_the_debounce() {
    let mut m = PortMachine::new();
    assert_eq!(m.step(here(1), 0), Action::None);
    assert_eq!(m.step(here(1), 90), Action::None, "the change bit says it bounced: count again from here");
    assert_eq!(m.step(here(0), 150), Action::None, "only 60 ms since the bounce");
    assert_eq!(m.step(here(0), 190), Action::Reset);
    // Or it goes away during the debounce.
    let mut m = PortMachine::new();
    m.step(here(1), 0);
    assert_eq!(m.step(GONE, 50), Action::None);
    assert_eq!(m.phase, Phase::Empty);
}

#[test]
fn a_reset_that_never_finishes_is_tried_three_times_then_given_up() {
    let mut m = PortMachine::new();
    let mut t = 0;
    let mut resets = 0;
    let mut gave_up = None;
    while t < 20_000 {
        let a = m.step(PortStatus { status: status::POWER | status::CONNECTION | status::RESET, change: 0 }, t);
        match a {
            Action::Reset => resets += 1,
            Action::GiveUp { why, remove } => {
                gave_up = Some((t, why, remove));
            }
            Action::None => {}
            other => panic!("{other:?}"),
        }
        t += 20;
    }
    assert_eq!(resets, 3);
    let (_, why, remove) = gave_up.unwrap();
    assert_eq!(why, Why::Reset);
    assert!(!remove);
    assert_eq!(m.phase, Phase::Failed { gone: None });
}

#[test]
fn a_reset_that_leaves_the_port_disabled_counts_as_a_failed_try() {
    let mut m = PortMachine::new();
    let mut t = 0;
    assert_eq!(run_until(&mut m, here(0), &mut t, 1000), Some(Action::Reset));
    // C_RESET set but not enabled.
    let st = PortStatus { status: status::POWER | status::CONNECTION, change: status::C_RESET };
    assert_eq!(m.step(st, t), Action::None);
    assert!(matches!(m.phase, Phase::Debounce { .. }));
    // A second and third time, then it is given up.
    assert_eq!(run_until(&mut m, here(0), &mut t, 1000), Some(Action::Reset));
    m.step(st, t);
    assert_eq!(run_until(&mut m, here(0), &mut t, 1000), Some(Action::Reset));
    let a = m.step(st, t);
    assert_eq!(a, Action::GiveUp { why: Why::Reset, remove: false });
}

#[test]
fn enumeration_failures_retry_and_then_give_up() {
    let mut m = PortMachine::new();
    let mut t = 0;
    for round in 0..MAX_ATTEMPTS {
        assert_eq!(run_until(&mut m, here(0), &mut t, 1000), Some(Action::Reset), "round {round}");
        assert_eq!(m.step(reset_done(false), t), Action::Enumerate { speed: speed::FULL });
        let a = m.enumerated(Outcome::Failed, t);
        if round + 1 < MAX_ATTEMPTS {
            assert_eq!(a, Action::None);
            assert!(matches!(m.phase, Phase::Debounce { .. }));
        } else {
            assert_eq!(a, Action::GiveUp { why: Why::Enumerate, remove: false });
        }
    }
    // Given up: the device sits there and nothing more happens.
    for i in 0..500 {
        assert_eq!(m.step(here(0), t + i * 20), Action::None);
    }
    // Unplugged for half a second: it is tried again with a fresh device.
    assert_eq!(m.step(GONE, t + 20_000), Action::None);
    assert_eq!(m.step(GONE, t + 20_000 + UNPLUG_FORGIVE_MS), Action::None);
    assert_eq!(m.phase, Phase::Empty);
    let mut t = t + 21_000;
    m.step(here(1), t);
    assert_eq!(run_until(&mut m, here(0), &mut t, 1000), Some(Action::Reset));
}

#[test]
fn a_device_we_do_not_drive_is_left_alone_quietly() {
    let mut m = PortMachine::new();
    let mut t = 0;
    run_until(&mut m, here(0), &mut t, 1000);
    m.step(reset_done(false), t);
    assert_eq!(m.enumerated(Outcome::Ignored, t), Action::GiveUp { why: Why::Ignored, remove: false });
    // Not retried 3 times.
    assert_eq!(run_until(&mut m, here(0), &mut t, 10_000), None);
    // The give-up is forgiven only after an unplug of at least UNPLUG_FORGIVE_MS: a brief glitch is not enough.
    m.step(GONE, t);
    m.step(here(0), t + 100);
    m.step(GONE, t + 200);
    assert!(matches!(m.phase, Phase::Failed { gone: Some(_) }));
}

#[test]
fn over_current_removes_the_device_and_stays_given_up_while_it_lasts() {
    let mut m = PortMachine::new();
    let mut t = 0;
    run_until(&mut m, here(0), &mut t, 1000);
    m.step(reset_done(false), t);
    m.enumerated(Outcome::Ok, t);
    let oc = PortStatus { status: status::OVER_CURRENT, change: status::C_OVER_CURRENT };
    assert_eq!(m.step(oc, t + 100), Action::GiveUp { why: Why::OverCurrent, remove: true });
    // The port has no power now, so it reads not connected; still over-current: no forgiving, no second line.
    for i in 0..200 {
        assert_eq!(m.step(oc, t + 200 + i * 20), Action::None);
    }
    // Over-current gone and the port empty: tried again later.
    assert_eq!(m.step(GONE, t + 10_000), Action::None);
    assert_eq!(m.step(GONE, t + 10_000 + UNPLUG_FORGIVE_MS), Action::None);
    assert_eq!(m.phase, Phase::Empty);
    // Over-current on an empty port gives up without a removal.
    let mut m = PortMachine::new();
    assert_eq!(m.step(oc, 0), Action::GiveUp { why: Why::OverCurrent, remove: false });
}

#[test]
fn a_hub_that_disables_the_port_makes_the_device_go_and_come_back() {
    let mut m = PortMachine::new();
    let mut t = 0;
    run_until(&mut m, here(0), &mut t, 1000);
    m.step(reset_done(true), t);
    m.enumerated(Outcome::Ok, t);
    let disabled = PortStatus { status: status::POWER | status::CONNECTION, change: status::C_ENABLE };
    assert_eq!(m.step(disabled, t + 20), Action::Remove);
    assert_eq!(m.phase, Phase::Empty);
    assert_eq!(run_until(&mut m, here(0), &mut t, 1000), Some(Action::Reset));
}

#[test]
fn a_connect_disconnect_storm_is_quarantined_then_given_up() {
    let mut m = PortMachine::new();
    let mut t = 0u64;
    let mut storms = 0;
    let mut gave_up = false;
    let mut resets = 0;
    // A cable that connects and disconnects every 40 ms, for a minute.
    for i in 0..1500u64 {
        let st = if i % 2 == 0 { here(1) } else { GONE };
        match m.step(st, t) {
            Action::Storm => storms += 1,
            Action::GiveUp { why: Why::Storm, .. } => gave_up = true,
            Action::Reset => resets += 1,
            _ => {}
        }
        t += 20;
    }
    assert_eq!(resets, 0, "it never stays long enough to be reset");
    assert!(storms >= 1, "{storms}");
    assert!(gave_up);
    assert!(matches!(m.phase, Phase::Failed { .. }));
    // It is given up on well before the minute is over, and stays so while it keeps flapping.
    let mut m = PortMachine::new();
    let mut t = 0u64;
    let mut quiet_since = None;
    for i in 0..1500u64 {
        let st = if i % 2 == 0 { here(1) } else { GONE };
        m.step(st, t);
        if matches!(m.phase, Phase::Failed { .. }) && quiet_since.is_none() {
            quiet_since = Some(t);
        }
        t += 20;
    }
    assert!(quiet_since.unwrap() < 40_000);
}

#[test]
fn a_few_plug_ins_are_not_a_storm() {
    let mut m = PortMachine::new();
    let mut t = 0u64;
    // Eight people plug something in, one every 12 seconds.
    for _ in 0..8 {
        m.step(here(1), t);
        let a = run_until(&mut m, here(0), &mut t, 1000);
        assert_eq!(a, Some(Action::Reset));
        assert_eq!(m.step(reset_done(false), t), Action::Enumerate { speed: speed::FULL });
        m.enumerated(Outcome::Ok, t);
        t += 5000;
        assert_eq!(m.step(GONE, t), Action::Remove);
        t += 7000;
    }
}

#[test]
fn quarantine_ends_by_itself() {
    let mut m = PortMachine::new();
    let mut t = 0u64;
    // Nine fast connects in a row: the ninth is a storm.
    let mut storm_at = None;
    for _ in 0..12 {
        if m.step(here(1), t) == Action::Storm {
            storm_at = Some(t);
            break;
        }
        m.step(GONE, t + 20);
        t += 40;
    }
    let at = storm_at.expect("a storm");
    assert!(matches!(m.phase, Phase::Quarantine { .. }));
    assert_eq!(m.step(here(1), at + QUARANTINE_MS - 1), Action::None);
    assert!(matches!(m.phase, Phase::Quarantine { .. }));
    m.step(here(1), at + QUARANTINE_MS);
    assert_eq!(m.phase, Phase::Empty);
    m.step(here(1), at + QUARANTINE_MS + 20);
    assert!(matches!(m.phase, Phase::Debounce { .. }));
}

// ---- fuzzing -----------------------------------------------------------------------------------

#[test]
fn fuzz_hub_descriptors_never_panic_and_obey_their_limits() {
    let mut st = 0x1234_5678_9abc_def1u64;
    for _ in 0..300_000 {
        let n = (rng(&mut st) % 24) as usize;
        let mut d: Vec<u8> = (0..n).map(|_| rng(&mut st) as u8).collect();
        // Bias towards plausible: right type often.
        if n > 2 && rng(&mut st) % 2 == 0 {
            d[1] = 0x29;
        }
        if let Some(h) = parse_hub_descriptor(&d) {
            assert!(h.nports >= 1 && d.len() >= 7 && d[1] == 0x29 && d[0] >= 7);
            assert!(h.settle_ms() >= 100 && h.settle_ms() <= 510);
            assert!(h.think_time_code() < 4 && (8..=32).contains(&h.think_time_bits()));
            let _ = (h.power_switching(), h.compound(), h.over_current_mode());
            // Whatever it claims, we drive a bounded number of ports.
            for used in [0u8, 17, 32, 255] {
                let u = usable_ports(h.nports, used);
                assert!(u <= MAX_PORTS_PER_HUB && (used as u16 + u as u16) <= MAX_PORTS_TOTAL.max(used) as u16);
            }
        }
        if let Some(p) = parse_port_status(&d) {
            let _ = (p.connected(), p.enabled(), p.powered(), p.in_reset(), p.over_current());
            assert!(matches!(p.speed(), speed::LOW | speed::FULL | speed::HIGH));
            assert!(p.change_features().count() <= 5);
        }
    }
}

#[test]
fn fuzz_routes_and_slot_contexts_stay_in_their_fields() {
    let mut st = 0x0fed_cba9_8765_4321u64;
    for _ in 0..200_000 {
        let route = rng(&mut st) as u32 & 0xf_ffff;
        let tier = (rng(&mut st) % 9) as u8;
        let port = rng(&mut st) as u8;
        match child_route(route, tier, port) {
            Some(r) => {
                assert!((1..=5).contains(&tier));
                assert_eq!(r >> 20, 0, "five nibbles");
                // The nibble of this tier carries the port (capped); the lower ones are as they were.
                let sh = 4 * (tier as u32 - 1);
                assert_eq!(r & ((1 << sh) - 1), route & ((1 << sh) - 1));
            }
            None => assert!(tier == 0 || tier > 5),
        }
        let p = SlotParams {
            route: rng(&mut st) as u32,
            speed: rng(&mut st) as u8,
            entries: rng(&mut st) as u8,
            root_port: rng(&mut st) as u8,
            hub: if rng(&mut st) % 2 == 0 { Some((rng(&mut st) as u8, rng(&mut st) % 2 == 0, rng(&mut st) as u8)) } else { None },
            tt: if rng(&mut st) % 2 == 0 { Some((rng(&mut st) as u8, rng(&mut st) as u8)) } else { None },
        };
        let mut s = [0u32; 8];
        fill_slot_full(&mut s, &p);
        // Entries is a 5-bit field in the top, speed 4 bits at 20, hub bit 26 only if it is a hub.
        assert_eq!(s[0] >> 27, p.entries as u32 & 0x1f);
        assert_eq!(s[0] >> 26 & 1, p.hub.is_some() as u32);
        assert_eq!(s[0] & 0xf_ffff, p.route & 0xf_ffff);
        assert_eq!(s[1] >> 16 & 0xff, p.root_port as u32);
        assert_eq!(s[2] >> 18, 0, "interrupter target stays 0");
        let t = tt_for(p.speed, Some(ParentHub { slot: 1, speed: speed::HIGH, tt: None }), 1);
        assert!(t.is_none() || p.speed == speed::LOW || p.speed == speed::FULL);
    }
}

#[test]
fn fuzz_the_port_machine_is_bounded_and_consistent() {
    let mut st = 0xabcd_ef01_2345_6789u64;
    for run in 0..300 {
        let mut m = PortMachine::new();
        let mut t = 0u64;
        let mut resets = 0u64;
        let mut active = false;
        let sticky = run % 3 == 0; // a third of the runs hold one status for a long stretch
        let mut cur = PortStatus::default();
        for i in 0..20_000u64 {
            if !sticky || i % 500 == 0 {
                let r = rng(&mut st);
                cur = PortStatus {
                    status: (r as u16) & (status::CONNECTION | status::ENABLE | status::OVER_CURRENT | status::RESET | status::POWER | status::LOW_SPEED | status::HIGH_SPEED),
                    change: (r >> 32) as u16 & 0x1f,
                };
                if rng(&mut st) % 4 != 0 {
                    // Mostly connected and fine.
                    cur.status |= status::CONNECTION | status::POWER;
                    cur.status &= !status::OVER_CURRENT;
                }
            }
            t += 1 + rng(&mut st) % 40;
            match m.step(cur, t) {
                Action::Reset => {
                    resets += 1;
                    assert!(cur.connected());
                    assert!(!active, "reset while a device is enumerated");
                }
                Action::Enumerate { speed: s } => {
                    assert!(cur.connected() && cur.enabled() && !cur.in_reset());
                    assert!(matches!(s, speed::LOW | speed::FULL | speed::HIGH));
                    assert!(!active);
                    active = true;
                    // The driver's answer, at random.
                    let o = match rng(&mut st) % 4 {
                        0 | 1 => Outcome::Ok,
                        2 => Outcome::Failed,
                        _ => Outcome::Ignored,
                    };
                    match m.enumerated(o, t) {
                        Action::GiveUp { remove, .. } => assert!(!remove),
                        Action::None => {}
                        a => panic!("{a:?}"),
                    }
                    if o != Outcome::Ok {
                        active = false;
                    }
                }
                Action::Remove => {
                    assert!(active);
                    active = false;
                }
                Action::GiveUp { remove, .. } => {
                    if remove {
                        assert!(active);
                    }
                    active = false;
                }
                Action::Storm | Action::None => {}
            }
            if matches!(m.phase, Phase::Active) != active {
                // `Active` is only entered through Enumerate (and left through Remove or GiveUp).
                assert!(!matches!(m.phase, Phase::Active) || active, "{:?} {run} {i}", m.phase);
            }
        }
        // Every reset needs a debounce of its own: never more than one per DEBOUNCE_MS of fake time.
        assert!(resets <= t / DEBOUNCE_MS + 1, "{resets} resets in {t} ms");
    }
}

#[test]
fn a_hub_reporting_the_same_garbage_forever_costs_a_bounded_number_of_resets() {
    // Every status the hub can report, held for ten simulated minutes at 20 ms steps.
    for status_bits in 0u16..0x800 {
        for change in [0u16, 0x1f] {
            let mut m = PortMachine::new();
            let st = PortStatus { status: status_bits, change };
            let mut resets = 0;
            let mut enumerates = 0;
            let mut t = 0u64;
            while t < 600_000 {
                match m.step(st, t) {
                    Action::Reset => resets += 1,
                    Action::Enumerate { .. } => {
                        enumerates += 1;
                        m.enumerated(Outcome::Failed, t);
                    }
                    _ => {}
                }
                t += 20;
            }
            assert!(resets <= 3 + 3 * MAX_QUARANTINES as u32 * 4, "status {status_bits:#x}: {resets} resets");
            assert!(enumerates <= MAX_ATTEMPTS as u32 * 4, "status {status_bits:#x}: {enumerates} enumerations");
        }
    }
}
