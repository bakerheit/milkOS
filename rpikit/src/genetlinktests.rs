//! Host tests of the GENET link-up path (the step table, the `milk.genet=` options) and of the power manager watchdog's
//! rules (a fake clock).

mod options {
    use crate::genet::init::*;
    use crate::genet::regs::*;
    use crate::genet::{Negotiated, umac_cmd_for};

    fn words(o: &Options) -> std::string::String {
        let mut s = std::string::String::new();
        o.write_words(&mut s).unwrap();
        s
    }

    #[test]
    fn nothing_asked_is_the_normal_driver() {
        for v in [None, Some(&b""[..]), Some(b"normal"), Some(b"off"), Some(b"0")] {
            let (o, unknown) = Options::parse(v);
            assert_eq!(o, Options::default(), "{v:?}");
            assert_eq!(unknown, None);
            assert!(!o.degraded());
            assert_eq!(words(&o), "normal");
        }
    }

    #[test]
    fn every_word_and_lists_of_them() {
        assert_eq!(Options::parse(Some(b"probe")).0, Options { probe: true, ..Options::default() });
        assert_eq!(Options::parse(Some(b"nolink")).0, Options { nolink: true, ..Options::default() });
        assert_eq!(Options::parse(Some(b"norx")).0, Options { norx: true, ..Options::default() });
        assert_eq!(Options::parse(Some(b"notx")).0, Options { notx: true, ..Options::default() });
        assert_eq!(Options::parse(Some(b"polled")).0, Options { polled: true, ..Options::default() });
        let (o, u) = Options::parse(Some(b"probe,norx,notx"));
        assert_eq!((o.probe, o.norx, o.notx, o.nolink, o.polled), (true, true, true, false, false));
        assert_eq!(u, None);
        assert_eq!(words(&o), "probe,norx,notx");
        // probe alone does not change what the driver does.
        assert!(!Options::parse(Some(b"probe")).0.degraded());
        assert!(Options::parse(Some(b"polled")).0.degraded());
    }

    #[test]
    fn a_typo_is_reported_and_the_rest_still_applies() {
        let (o, u) = Options::parse(Some(b"probe,nrox,polled,bogus"));
        assert!(o.probe && o.polled && !o.norx);
        assert_eq!(u, Some(&b"nrox"[..]), "the first unknown word");
        let (o, u) = Options::parse(Some(b"proeb"));
        assert_eq!(o, Options::default());
        assert_eq!(u, Some(&b"proeb"[..]));
    }

    #[test]
    fn gates() {
        let all = [Gate::Always, Gate::RxDma, Gate::TxDma, Gate::Irq];
        let runs = |o: Options| all.map(|g| o.runs(g));
        assert_eq!(runs(Options::default()), [true; 4]);
        assert_eq!(runs(Options { norx: true, ..Options::default() }), [true, false, true, false], "norx: no receive DMA, so no interrupt");
        assert_eq!(runs(Options { notx: true, ..Options::default() }), [true, true, false, true]);
        assert_eq!(runs(Options { polled: true, ..Options::default() }), [true, true, true, false]);
        assert_eq!(runs(Options { probe: true, nolink: true, ..Options::default() }), [true; 4], "probe and nolink gate no step of the tables");
    }

    #[test]
    fn dma_on_follows_the_options() {
        let names = |o: Options| DMA_ON.iter().filter(|s| o.runs(s.gate)).map(|s| s.name).collect::<std::vec::Vec<_>>();
        assert_eq!(names(Options::default()), ["rdma-ring-cfg", "tdma-ring-cfg", "rdma-on", "tdma-on"]);
        assert_eq!(names(Options { norx: true, ..Options::default() }), ["tdma-ring-cfg", "tdma-on"]);
        assert_eq!(names(Options { notx: true, ..Options::default() }), ["rdma-ring-cfg", "rdma-on"]);
        assert_eq!(Options { norx: true, notx: true, ..Options::default() }.count(DMA_ON), 0);
        // The group's checkpoint is on the last step of the table whatever is left out (the driver runs it for a skipped step).
        assert_eq!(DMA_ON.last().unwrap().group_end, Some("genet-dma-on"));
    }

    fn pos(name: &str) -> usize {
        LINK_UP.iter().position(|s| s.name == name).unwrap_or_else(|| panic!("no step {name}"))
    }

    #[test]
    fn link_up_runs_in_linux_order_and_the_mac_goes_live_last() {
        let names: std::vec::Vec<_> = LINK_UP.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            ["link-resolve", "link-rgmii-link", "link-umac-speed", "link-mac-rx-en", "link-irq-unmask", "link-mac-tx-en", "link-announce"]
        );
        // The RGMII block learns the link before the speed is set, and the speed before either direction is enabled.
        assert!(pos("link-rgmii-link") < pos("link-umac-speed"));
        assert!(pos("link-umac-speed") < pos("link-mac-rx-en") && pos("link-umac-speed") < pos("link-mac-tx-en"));
        // The interrupt is unmasked after the receiver, and the stack hears about the link last.
        assert!(pos("link-mac-rx-en") < pos("link-irq-unmask"));
        assert_eq!(pos("link-announce"), LINK_UP.len() - 1);
        assert_eq!(LINK_UP.iter().filter(|s| s.group_end.is_some()).count(), 1, "one checkpoint, at the end");
    }

    #[test]
    fn only_the_link_up_path_enables_the_mac() {
        // Nothing in the set-up tables turns the transmitter or receiver on: the cable (a link) does.
        for s in INIT {
            match s.op {
                Op::Write(v) if s.reg == UMAC_CMD => assert_eq!(v & (CMD_TX_EN | CMD_RX_EN), 0, "{}", s.name),
                Op::Set(b) => assert_eq!(b & (CMD_TX_EN | CMD_RX_EN) != 0 && s.reg == UMAC_CMD, false, "{}", s.name),
                _ => {}
            }
        }
        let en = |n: &str| LINK_UP[pos(n)];
        assert_eq!((en("link-mac-rx-en").reg, en("link-mac-rx-en").op), (UMAC_CMD, Op::Set(CMD_RX_EN)));
        assert_eq!((en("link-mac-tx-en").reg, en("link-mac-tx-en").op), (UMAC_CMD, Op::Set(CMD_TX_EN)));
        assert_eq!((en("link-rgmii-link").reg, en("link-rgmii-link").op), (EXT_RGMII_OOB_CTRL, Op::Set(RGMII_LINK)));
        // The interrupt step is the only gated one and it unmasks exactly the receive interrupts.
        assert_eq!(LINK_UP.iter().filter(|s| s.gate != Gate::Always).count(), 1);
        assert_eq!(en("link-irq-unmask").gate, Gate::Irq);
        assert_eq!(en("link-irq-unmask").op, Op::Write(IRQ_RXDMA_DONE));
    }

    #[test]
    fn steps_that_run_with_each_option() {
        let n = |o: Options| o.count(LINK_UP);
        assert_eq!(n(Options::default()), 7);
        assert_eq!(n(Options { polled: true, ..Options::default() }), 6);
        assert_eq!(n(Options { norx: true, ..Options::default() }), 6, "norx: the MAC receiver is on, the DMA and the interrupt are not");
        assert_eq!(n(Options { notx: true, ..Options::default() }), 7);
        let f = |o: Options| o.count(FIRST_FRAMES);
        assert_eq!(f(Options::default()), 2);
        assert_eq!(f(Options { norx: true, ..Options::default() }), 1);
        assert_eq!(f(Options { notx: true, ..Options::default() }), 1);
    }

    #[test]
    fn the_first_frames_come_after_the_link() {
        assert_eq!(FIRST_FRAMES.iter().map(|s| s.name).collect::<std::vec::Vec<_>>(), ["first-rx-frame", "first-tx-kick"]);
        assert!(FIRST_FRAMES.iter().all(|s| matches!(s.op, Op::Event(_)) && s.reg == 0 && s.group_end.is_none()));
    }

    #[test]
    fn step_text_for_the_log() {
        let d = |name| std::format!("{}", LINK_UP[pos(name)]);
        assert_eq!(d("link-rgmii-link"), "genet+0x8c |= 0x10");
        assert_eq!(d("link-mac-rx-en"), "genet+0x808 |= 0x2");
        assert_eq!(d("link-mac-tx-en"), "genet+0x808 |= 0x1");
        assert_eq!(d("link-irq-unmask"), "write genet+0x214 <- 0xe000");
        assert_eq!(d("link-umac-speed"), "genet+0x808: speed and duplex bits <- the negotiated mode");
        assert!(d("link-announce").contains("network stack"));
        assert!(d("link-resolve").contains("MDIO"));
    }

    #[test]
    fn speed_and_duplex_bits() {
        let full = |mbps| Negotiated { mbps, full: true };
        // From the run value (promiscuous, pad, pause ignore): only the speed and duplex bits change.
        let run = UMAC_CMD_RUN;
        assert_eq!(umac_cmd_for(run, full(1000)), run | 2 << 2);
        assert_eq!(umac_cmd_for(run, full(100)), run | 1 << 2);
        assert_eq!(umac_cmd_for(run, full(10)), run);
        assert_eq!(umac_cmd_for(run, Negotiated { mbps: 100, full: false }), run | 1 << 2 | CMD_HD_EN);
        // Stale bits of an earlier mode are cleared, the enables stay.
        let was = run | 3 << 2 | CMD_HD_EN | CMD_TX_EN | CMD_RX_EN;
        assert_eq!(umac_cmd_for(was, full(1000)), run | 2 << 2 | CMD_TX_EN | CMD_RX_EN);
        // The real board's first link: 1000 Mb/s full duplex.
        assert_eq!(umac_cmd_for(0x1000_0130, full(1000)), 0x1000_0138);
    }
}

mod watchdog {
    use crate::watchdog::*;

    #[test]
    fn register_values() {
        assert_eq!(MAX_TIMEOUT_MS, 15_999);
        assert_eq!(wdog_value(15_000), 0x5a0f_0000, "15 s is 983040 ticks of 1/65536 s");
        assert_eq!(wdog_value(1000), 0x5a01_0000);
        assert_eq!(wdog_value(0), 0x5a00_0001, "never zero (zero would reset at once)");
        assert_eq!(wdog_value(60_000), 0x5a0f_ffff, "clamped to what the 20-bit register holds");
        assert_eq!(rstc_full_reset(0), 0x5a00_0020);
        assert_eq!(rstc_full_reset(0x0000_0102), 0x5a00_0122, "other bits kept, the configuration field set to a full reset");
        assert_eq!(rstc_full_reset(0x0000_0030), 0x5a00_0020, "the old configuration bits are replaced");
        assert_eq!(rstc_full_reset(0xffff_ffff), 0xffff_ffef);
        assert_eq!(RSTC_STOP, 0x5a00_0102);
    }

    #[test]
    fn setting_option() {
        assert_eq!(Setting::parse(None), Ok(Setting::On));
        assert_eq!(Setting::parse(Some(b"on")), Ok(Setting::On));
        assert_eq!(Setting::parse(Some(b"off")), Ok(Setting::Off));
        assert_eq!(Setting::parse(Some(b"sim")), Ok(Setting::Sim));
        assert_eq!(Setting::parse(Some(b"of")), Err(Setting::On), "a typo keeps the safety net on and is reported");
    }

    #[test]
    fn not_armed_never_kicks_or_expires() {
        let mut w = Watchdog::new(15_000, 1000);
        for t in (0..100_000).step_by(10) {
            assert_eq!(w.tick(t), Action::Nothing);
            assert!(!w.expired(t));
        }
        assert_eq!(w.disarm(), Action::Nothing);
    }

    #[test]
    fn armed_after_the_boot_kicked_once_a_second_from_a_100_hz_tick() {
        let mut w = Watchdog::new(15_000, 1000);
        assert_eq!(w.arm(40_000), Action::Arm);
        assert_eq!(w.arm(40_010), Action::Nothing, "arming twice does nothing");
        let mut kicks = 0;
        for t in (40_000..70_000u64).step_by(10) {
            match w.tick(t) {
                Action::Kick => kicks += 1,
                Action::Nothing => {}
                other => panic!("{other:?}"),
            }
            assert!(!w.expired(t), "a ticking kernel is never reset (t={t})");
        }
        assert_eq!(kicks, 29, "once a second for the 29 s after the first");
        assert_eq!(w.kicks(), 29);
        assert_eq!(w.max_gap_ms(), 1000);
    }

    #[test]
    fn a_kernel_that_stops_ticking_is_reset_at_the_timeout() {
        let mut w = Watchdog::new(15_000, 1000);
        w.arm(0);
        for t in (0..=5_000u64).step_by(10) {
            w.tick(t);
        }
        // The last kick was at 5 s; the tick stops (a hung bus, interrupts off for good).
        assert!(!w.expired(5_000 + 14_999));
        assert!(w.expired(5_000 + 15_000));
        assert!(w.expired(100_000));
    }

    #[test]
    fn a_slow_but_alive_kernel_is_not_reset_and_the_gap_is_remembered() {
        let mut w = Watchdog::new(15_000, 1000);
        w.arm(0);
        // Interrupts off for 9 s (a long copy), then the tick is back: kicked, not expired, and the gap is on record.
        assert!(!w.expired(9_000));
        assert_eq!(w.tick(9_000), Action::Kick);
        assert!(!w.expired(9_001));
        assert_eq!(w.max_gap_ms(), 9_000);
        assert_eq!(w.tick(9_500), Action::Nothing);
        assert_eq!(w.tick(10_000), Action::Kick);
        assert_eq!(w.max_gap_ms(), 9_000, "the longest stays");
    }

    #[test]
    fn disarmed_before_power_off_a_crash_dump_or_restart() {
        let mut w = Watchdog::new(15_000, 1000);
        w.arm(0);
        assert_eq!(w.disarm(), Action::Stop);
        assert!(!w.armed());
        assert_eq!(w.disarm(), Action::Nothing, "once");
        assert_eq!(w.tick(5_000), Action::Nothing);
        assert!(!w.expired(1_000_000), "a stopped watchdog does not expire while the crash screen is read");
        // A new boot phase may arm it again (the test never does, but the state machine allows it).
        assert_eq!(w.arm(2_000_000), Action::Arm);
    }
}
