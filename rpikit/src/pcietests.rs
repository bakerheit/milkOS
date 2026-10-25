//! Host tests for `pcie`: the step table, the handshake orders, the register field maths.

use crate::pcie::*;

#[test]
fn steps_are_unique_and_labelled() {
    for (i, s) in STEPS.iter().enumerate() {
        assert_eq!(s.label, ["pcie-", s.name].concat(), "step {i}");
        assert!(!s.what.is_empty());
        assert_eq!(step_index(s.name), Some(i));
        for t in &STEPS[i + 1..] {
            assert_ne!(s.name, t.name);
        }
    }
    assert_eq!(step_index("no-such-step"), None);
}

#[test]
fn steps_run_in_the_order_the_hardware_needs() {
    let at = |n: &str| step_index(n).unwrap_or_else(|| panic!("no step {n}"));
    let chain = [
        "bridge-reset",
        "bridge-release",
        "serdes-power",
        "misc-ctrl",
        "inbound-window",
        "intr-mask",
        "perst-release",
        "link-wait",
        "outbound-window",
        "root-port",
        "enumerate",
        "vl805-handshake",
        "enable",
    ];
    for w in chain.windows(2) {
        assert!(at(w[0]) < at(w[1]), "{} before {}", w[0], w[1]);
    }
    assert_eq!(chain.len(), STEPS.len(), "the chain names every step");
    // The windows the device's DMA and the CPU's reach depend on are set before the link is released,
    // and nothing talks to the device before the link is up.
    assert!(at("inbound-window") < at("perst-release"));
    assert!(at("link-wait") < at("enumerate"));
    // The handshake comes after the device is found and before it may decode memory or master the bus.
    assert!(at("enumerate") < at("vl805-handshake") && at("vl805-handshake") < at("enable"));
}

#[test]
fn orders_start_with_the_default_and_end_with_no_call() {
    let first = ORDERS[0];
    assert_eq!(first.name, "notify-first");
    assert!(first.notify && !first.memory_before && !first.master_before);
    let last = ORDERS[ORDERS.len() - 1];
    assert!(!last.notify);
    assert_eq!(ORDERS.iter().filter(|o| !o.notify).count(), 1);
    for (i, o) in ORDERS.iter().enumerate() {
        assert_eq!(o.label, ["pcie-order-", o.name].concat());
        // Bus mastering without memory decode makes no sense for a device that is reached through its BARs.
        assert!(!o.master_before || o.memory_before, "{}", o.name);
        for p in &ORDERS[i + 1..] {
            assert_ne!(o.name, p.name);
            assert_ne!((o.notify, o.memory_before, o.master_before), (p.notify, p.memory_before, p.master_before));
        }
    }
    // Ordered from the least to the most the command register has on.
    let on = |o: &Order| o.memory_before as u8 + o.master_before as u8;
    let called: std::vec::Vec<u8> = ORDERS.iter().filter(|o| o.notify).map(on).collect();
    assert!(called.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn notify_word_is_the_config_index() {
    assert_eq!(vl805_notify_word(1, 0, 0), 0x0010_0000);
    assert_eq!(vl805_notify_word(0, 1, 0), 0x0000_8000);
    assert_eq!(vl805_notify_word(0, 0, 1), 0x0000_1000);
    assert_eq!(vl805_notify_word(0xff, 0x1f, 7), 0x0fff_f000);
    // Out-of-range device and function numbers cannot spill into the bus field.
    assert_eq!(vl805_notify_word(0, 0xff, 0xff), 0x000f_f000);
}

#[test]
fn mode_words() {
    assert_eq!(parse_mode(b"probe"), Some(Mode::Probe));
    assert_eq!(parse_mode(b""), Some(Mode::Normal));
    assert_eq!(parse_mode(b"normal"), Some(Mode::Normal));
    assert_eq!(parse_mode(b"Probe"), None);
    assert_eq!(parse_mode(b"probe,x"), None);
}

#[test]
fn inbound_size_encoding() {
    assert_eq!(encode_ibar_size(0), 0);
    assert_eq!(encode_ibar_size(4 << 10), 0x1c);
    assert_eq!(encode_ibar_size(32 << 10), 0x1f);
    assert_eq!(encode_ibar_size(64 << 10), 1);
    assert_eq!(encode_ibar_size(1 << 30), 15);
    assert_eq!(encode_ibar_size(1 << 32), 17);
    assert_eq!(encode_ibar_size(8 << 30), 18);
    assert_eq!(encode_ibar_size(1 << 35), 20);
    assert_eq!(encode_ibar_size(1 << 36), 0);
    assert_eq!(encode_ibar_size(2 << 10), 0);
    assert_eq!(scb_size_field(1 << 32), 17);
    assert_eq!(scb_size_field(1 << 30), 15);
    assert_eq!(scb_size_field(1 << 10), 0);
}

#[test]
fn outbound_window_of_the_stock_tree() {
    // CPU 0x6_0000_0000 = PCI 0xc000_0000, 1 GiB.
    let w = outbound(0xc000_0000, 0x6_0000_0000, 0x4000_0000);
    assert_eq!(w.pci_lo, 0xc000_0000);
    assert_eq!(w.pci_hi, 0);
    // base 0x6_0000_0000 >> 20 = 0x6000 (low 12 bits 0x000, high bits 0x6); limit 0x6_3fff_ffff >> 20 = 0x63ff.
    assert_eq!(w.base_limit, 0x000 << 4 | 0x3ff << 20);
    assert_eq!(w.base_hi, 6);
    assert_eq!(w.limit_hi, 6);
    assert_eq!(w.base_limit & !OUTBOUND_BASE_LIMIT_MASK, 0);
}

#[test]
fn outbound_window_low_addresses() {
    let w = outbound(0x1000_0000, 0x4000_0000, 0x1000_0000);
    assert_eq!(w.base_limit, 0x400 << 4 | 0x4ff << 20 & 0xfff0_0000);
    assert_eq!((w.base_hi, w.limit_hi), (0, 0));
    // A zero size does not underflow.
    let z = outbound(0, 0x1_0000_0000, 0);
    assert_eq!(z.base_hi, z.limit_hi);
}

#[test]
fn survival_of_the_device() {
    let id = 0x3483_1106;
    assert_eq!(survival(id, id), Survival::Same);
    assert_eq!(survival(id, !0), Survival::Gone);
    assert_eq!(survival(id, 0), Survival::Gone);
    assert_eq!(survival(id, 0x1234_5678), Survival::Changed);
}

#[test]
fn xhci_capability_word() {
    assert!(plausible_xhci_caps(0x0100_0020));
    assert!(!plausible_xhci_caps(!0));
    assert!(!plausible_xhci_caps(0));
    assert!(!plausible_xhci_caps(0x0100_0000));
}
