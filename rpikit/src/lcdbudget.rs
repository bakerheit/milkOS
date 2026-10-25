//! Time limits for the 3.5-inch display (docs/raspberry-pi.md, "3.5-inch SPI display"): nothing
//! the display does may hold up the boot or the boot log. All of it is arithmetic on microsecond
//! readings handed in by the caller, so the host tests run it with a fake clock:
//!
//! * [`Budget`] and [`BudgetBus`]: the early bring-up (reset pulse, init sequence, first full
//!   picture) gets about a second in all. Past it every write fails at once and every delay is
//!   skipped, and the first failure is remembered (how long, which step) for the log line
//!   `lcd: gave up after N ms at step X`. A single transfer never runs longer than what is left.
//! * [`Throttle`]: how often the print path may send a few rows to the panel, and [`pump_rows`],
//!   how many rows fit in the few milliseconds one `kprintln!` may spend.
//! * [`ProbeGate`]: when `milk.lcd=probe` may start: after the boot is over and the log is saved
//!   to the card (or, if there is no log file, after a grace period).

use crate::lcd::Bus;

/// A reading of a microsecond clock.
pub trait Clock {
    fn now_us(&self) -> u64;
}

/// What a bus operation was, for the "at step" text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Reset,
    Command,
    Data,
    Delay,
}

impl Op {
    pub const fn name(self) -> &'static str {
        match self {
            Op::Reset => "reset pin",
            Op::Command => "command",
            Op::Data => "data",
            Op::Delay => "delay",
        }
    }
}

/// Why the first operation was refused or failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// The time was used up.
    OutOfTime,
    /// The transfer itself failed (it timed out or the controller is not there).
    Transfer,
}

/// Where the bring-up gave up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GaveUp {
    /// Milliseconds since the budget started.
    pub ms: u64,
    /// Which operation of the phase (counting from 1).
    pub step: u32,
    pub op: Op,
    pub why: Why,
}

/// A time allowance that started at `start_us` and lasts `limit_us`.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    start_us: u64,
    limit_us: u64,
    steps: u32,
    gave_up: Option<GaveUp>,
}

impl Budget {
    pub const fn new(start_us: u64, limit_us: u64) -> Budget {
        Budget {
            start_us,
            limit_us,
            steps: 0,
            gave_up: None,
        }
    }

    /// Microseconds left at `now_us` (0 when used up).
    pub fn remaining_us(&self, now_us: u64) -> u64 {
        self.limit_us
            .saturating_sub(now_us.saturating_sub(self.start_us))
    }

    pub fn elapsed_ms(&self, now_us: u64) -> u64 {
        now_us.saturating_sub(self.start_us) / 1000
    }

    /// Start counting steps again (a new phase) without moving the clock.
    pub fn next_phase(&mut self) {
        self.steps = 0;
    }

    /// The first refusal or failure, if there was one.
    pub fn gave_up(&self) -> Option<GaveUp> {
        self.gave_up
    }

    /// Note a step before it is done: `Some(left)` microseconds it may take, or `None` (and the
    /// give-up is recorded) if the time is used up or the budget already gave up.
    pub fn admit(&mut self, now_us: u64, op: Op) -> Option<u64> {
        self.steps += 1;
        if self.gave_up.is_some() {
            return None;
        }
        let left = self.remaining_us(now_us);
        if left == 0 {
            self.fail(now_us, op, Why::OutOfTime);
            return None;
        }
        Some(left)
    }

    /// Note that the step just admitted failed.
    pub fn fail(&mut self, now_us: u64, op: Op, why: Why) {
        if self.gave_up.is_none() {
            self.gave_up = Some(GaveUp {
                ms: self.elapsed_ms(now_us),
                step: self.steps,
                op,
                why,
            });
        }
    }
}

/// A [`Bus`] that keeps another within a [`Budget`].
pub struct BudgetBus<'a, B: Bus, C: Clock> {
    pub inner: &'a mut B,
    pub budget: &'a mut Budget,
    pub clock: &'a C,
}

impl<B: Bus, C: Clock> Bus for BudgetBus<'_, B, C> {
    fn write(&mut self, data: bool, bytes: &[u8]) -> bool {
        let op = if data { Op::Data } else { Op::Command };
        let Some(left) = self.budget.admit(self.clock.now_us(), op) else {
            return false;
        };
        self.inner.set_cap_us(left);
        let ok = self.inner.write(data, bytes);
        let now = self.clock.now_us();
        if !ok {
            self.budget.fail(now, op, Why::Transfer);
        } else if self.budget.remaining_us(now) == 0 {
            // It finished, but only just: the next step is the one refused.
        }
        ok
    }
    fn reset_pin(&mut self, high: bool) {
        // Setting a pin takes no time; it is refused only once the budget has given up.
        if self.budget.admit(self.clock.now_us(), Op::Reset).is_some() {
            self.inner.reset_pin(high);
        }
    }
    fn delay_ms(&mut self, ms: u32) {
        let Some(left) = self.budget.admit(self.clock.now_us(), Op::Delay) else {
            return;
        };
        // A wait longer than what is left is cut short: the next write then gives up.
        let ms = u64::from(ms).min(left.div_ceil(1000)) as u32;
        self.inner.delay_ms(ms);
    }
}

// ---- the print path ---------------------------------------------------------------------------

/// The most one `kprintln!` may spend sending to the panel, microseconds.
pub const PUMP_BUDGET_US: u64 = 3_000;
/// At least this long between two sends from the print path.
pub const PUMP_GAP_US: u64 = 20_000;

/// How many panel rows fit in `budget_us` on the wire: `row_bytes` bytes per row, `bit_us_x1000`
/// thousandths of a microsecond per bit (1000 * divider / core MHz). At least 1.
pub fn pump_rows(row_bytes: u64, bit_us_x1000: u64, budget_us: u64) -> usize {
    let per_row = (row_bytes * 8 * bit_us_x1000 / 1000).max(1);
    (budget_us / per_row).max(1) as usize
}

/// Rate limit for the print path's sends: one every [`PUMP_GAP_US`], the rest are dropped (and
/// counted; what they changed stays marked and goes out with the next send or the panel thread).
#[derive(Clone, Copy, Debug, Default)]
pub struct Throttle {
    last_us: u64,
    sent_once: bool,
    pub dropped: u32,
}

impl Throttle {
    pub const fn new() -> Throttle {
        Throttle {
            last_us: 0,
            sent_once: false,
            dropped: 0,
        }
    }

    /// May the print path send now? If not, it is counted as dropped.
    pub fn allow(&mut self, now_us: u64) -> bool {
        if self.sent_once && now_us.saturating_sub(self.last_us) < PUMP_GAP_US {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.sent_once = true;
        self.last_us = now_us;
        true
    }

    /// A send did not happen after all (the bus was busy): count it.
    pub fn note_dropped(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }
}

// ---- when the probe may start -----------------------------------------------------------------

/// `milk.lcd=probe` waits until the boot is over (the desktop is up, the LED solid) and the boot
/// log is on the card; with no log file to wait for, `grace_ms` after the boot is over.
#[derive(Clone, Copy, Debug)]
pub struct ProbeGate {
    grace_ms: u64,
    over_at_ms: Option<u64>,
}

impl ProbeGate {
    pub const fn new(grace_ms: u64) -> ProbeGate {
        ProbeGate {
            grace_ms,
            over_at_ms: None,
        }
    }

    /// Poll with the time, whether the boot is over and whether the log is saved on the card.
    pub fn ready(&mut self, now_ms: u64, boot_over: bool, log_saved: bool) -> bool {
        if !boot_over {
            return false;
        }
        let at = *self.over_at_ms.get_or_insert(now_ms);
        log_saved || now_ms.saturating_sub(at) >= self.grace_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lcd::{self, Config, Controller, Rect, Solid};
    use crate::lcdsim::SimPanel;
    use core::cell::Cell;

    /// A clock the fake buses advance.
    struct Fake(Cell<u64>);
    impl Clock for Fake {
        fn now_us(&self) -> u64 {
            self.0.get()
        }
    }

    /// A bus that costs time: `wire_us` per write (capped by what the caller allows), `stall`
    /// makes every write eat its whole cap and fail.
    struct Slow<'a> {
        clock: &'a Fake,
        wire_us: u64,
        stall: bool,
        cap: u64,
        writes: u32,
    }
    impl Bus for Slow<'_> {
        fn write(&mut self, _: bool, _: &[u8]) -> bool {
            self.writes += 1;
            if self.stall {
                self.clock
                    .0
                    .set(self.clock.0.get() + self.cap.min(2_000_000));
                return false;
            }
            self.clock
                .0
                .set(self.clock.0.get() + self.wire_us.min(self.cap));
            true
        }
        fn reset_pin(&mut self, _: bool) {}
        fn delay_ms(&mut self, ms: u32) {
            self.clock.0.set(self.clock.0.get() + u64::from(ms) * 1000);
        }
        fn set_cap_us(&mut self, us: u64) {
            self.cap = us;
        }
    }

    fn slow(clock: &Fake, wire_us: u64, stall: bool) -> Slow<'_> {
        Slow {
            clock,
            wire_us,
            stall,
            cap: u64::MAX,
            writes: 0,
        }
    }

    #[test]
    fn a_fast_bring_up_fits() {
        let cfg = Config::new(Controller::Ili9486);
        let clock = Fake(Cell::new(5_000_000));
        let mut inner = slow(&clock, 300, false);
        let mut budget = Budget::new(5_000_000, 1_000_000);
        let ok = lcd::init(
            &cfg,
            &mut BudgetBus {
                inner: &mut inner,
                budget: &mut budget,
                clock: &clock,
            },
        );
        assert!(ok);
        assert!(budget.gave_up().is_none());
        assert!(budget.remaining_us(clock.0.get()) > 0);
    }

    #[test]
    fn a_stalled_bus_is_cut_off_within_the_budget() {
        let cfg = Config::new(Controller::Ili9486);
        let clock = Fake(Cell::new(1_000));
        // Every write eats its whole allowance (the controller never answers).
        let mut inner = slow(&clock, 0, true);
        let mut budget = Budget::new(1_000, 1_000_000);
        let ok = lcd::init(
            &cfg,
            &mut BudgetBus {
                inner: &mut inner,
                budget: &mut budget,
                clock: &clock,
            },
        );
        assert!(!ok);
        let g = budget.gave_up().unwrap();
        assert_eq!(g.why, Why::Transfer);
        assert!(g.ms <= 1000 + 150 + 1, "{g:?}");
        // Nothing ran past the budget: the clock is at most one delay run past the limit.
        assert!(clock.0.get() - 1_000 <= 1_000_000 + 130_000);
        assert_eq!(inner.writes, 1, "the first failure ends the bring-up");
    }

    #[test]
    fn a_slow_bus_runs_out_of_time_at_a_step() {
        let cfg = Config::new(Controller::Ili9486);
        let clock = Fake(Cell::new(0));
        // 90 ms a write: the init sequence's waits plus a few dozen writes pass a second.
        let mut inner = slow(&clock, 90_000, false);
        let mut budget = Budget::new(0, 1_000_000);
        let ok = lcd::init(
            &cfg,
            &mut BudgetBus {
                inner: &mut inner,
                budget: &mut budget,
                clock: &clock,
            },
        );
        assert!(!ok);
        let g = budget.gave_up().unwrap();
        assert_eq!(g.why, Why::OutOfTime);
        assert!(g.ms >= 1000, "{g:?}");
        assert!(g.step > 3, "{g:?}");
        // Once given up, no write reaches the bus and no delay is spent.
        let (w, t) = (inner.writes, clock.0.get());
        let mut b = BudgetBus {
            inner: &mut inner,
            budget: &mut budget,
            clock: &clock,
        };
        assert!(!b.write(true, &[1, 2, 3]));
        b.delay_ms(500);
        assert_eq!((inner.writes, clock.0.get()), (w, t));
    }

    #[test]
    fn a_single_write_never_gets_more_than_what_is_left() {
        let clock = Fake(Cell::new(0));
        let mut inner = slow(&clock, 400_000, false);
        let mut budget = Budget::new(0, 1_000_000);
        // Three writes of 400 ms each: the third is allowed only the 200 ms that are left.
        let mut b = BudgetBus {
            inner: &mut inner,
            budget: &mut budget,
            clock: &clock,
        };
        assert!(b.write(false, &[1]));
        assert!(b.write(false, &[1]));
        assert!(b.write(false, &[1]));
        assert_eq!(clock.0.get(), 1_000_000);
        assert!(!b.write(false, &[1]));
        assert_eq!(budget.gave_up().unwrap().why, Why::OutOfTime);
        assert_eq!(budget.gave_up().unwrap().step, 4);
    }

    #[test]
    fn a_delay_is_cut_to_what_is_left() {
        let clock = Fake(Cell::new(0));
        let mut inner = slow(&clock, 0, false);
        let mut budget = Budget::new(0, 100_000);
        let mut b = BudgetBus {
            inner: &mut inner,
            budget: &mut budget,
            clock: &clock,
        };
        b.delay_ms(500);
        assert_eq!(clock.0.get(), 100_000);
    }

    #[test]
    fn the_first_full_picture_fits_at_16_mhz_not_at_1_mhz() {
        // 480x320 at 3 bytes a pixel: the wire time decides.
        let cfg = Config::new(Controller::Ili9488);
        let full = Rect::new(0, 0, cfg.width(), cfg.height());
        let bytes = (cfg.width() * cfg.height() * cfg.controller.bytes_per_pixel()) as u64;
        // A 2048-byte chunk takes 1 ms at 16 MHz and 16 ms at 1 MHz.
        for (chunk_us, fits) in [(1_024u64, true), (16_384, false)] {
            let clock = Fake(Cell::new(0));
            let mut inner = slow(&clock, chunk_us, false);
            let mut budget = Budget::new(0, 1_000_000);
            let ok = lcd::draw_rect(
                &cfg,
                &mut BudgetBus {
                    inner: &mut inner,
                    budget: &mut budget,
                    clock: &clock,
                },
                full,
                &mut Solid(0),
            );
            assert_eq!(ok, fits, "chunk {chunk_us} us, {bytes} bytes");
            if !fits {
                assert_eq!(budget.gave_up().unwrap().why, Why::OutOfTime);
            }
        }
    }

    #[test]
    fn the_bring_up_still_draws_the_same_bytes() {
        // Inside the budget the wrapper changes nothing the panel sees.
        let cfg = Config::new(Controller::St7796);
        let clock = Fake(Cell::new(0));
        let mut direct = SimPanel::new(cfg.controller);
        assert!(lcd::init(&cfg, &mut direct));
        let mut wrapped = SimPanel::new(cfg.controller);
        let mut budget = Budget::new(0, u64::MAX / 4);
        assert!(lcd::init(
            &cfg,
            &mut BudgetBus {
                inner: &mut wrapped,
                budget: &mut budget,
                clock: &clock,
            },
        ));
        assert_eq!(direct.log, wrapped.log);
    }

    #[test]
    fn rows_per_pump_fit_the_budget() {
        // 16 MHz from a 500 MHz core: divider 32, 1 core cycle = 2 ns, a bit = 64 ns = 0.064 us.
        let bit = 64;
        let rows = pump_rows(480 * 3, bit, PUMP_BUDGET_US);
        assert!((1..=32).contains(&rows), "{rows}");
        assert!(rows as u64 * 480 * 3 * 8 * bit / 1000 <= PUMP_BUDGET_US);
        // A very slow clock still sends one row.
        assert_eq!(pump_rows(480 * 3, 4000, 3_000), 1);
        // Never zero, never a division by zero.
        assert_eq!(pump_rows(0, 0, 0), 1);
    }

    #[test]
    fn the_throttle_lets_one_send_through_per_gap() {
        let mut t = Throttle::new();
        assert!(t.allow(0), "the first send is always allowed");
        assert!(!t.allow(1_000));
        assert!(!t.allow(19_999));
        assert_eq!(t.dropped, 2);
        assert!(t.allow(20_000));
        t.note_dropped();
        assert_eq!(t.dropped, 3);
        // A hundred lines in a millisecond cost one send.
        let mut t = Throttle::new();
        let sent = (0..100).filter(|i| t.allow(*i * 10)).count();
        assert_eq!(sent, 1);
        assert_eq!(t.dropped, 99);
    }

    #[test]
    fn the_probe_waits_for_the_boot_and_the_log() {
        let mut g = ProbeGate::new(8_000);
        // Boot not over: never, whatever the log says.
        assert!(!g.ready(0, false, false));
        assert!(!g.ready(50_000, false, true));
        // Boot over at 60 s, log not saved: waits out the grace period.
        assert!(!g.ready(60_000, true, false));
        assert!(!g.ready(67_999, true, false));
        assert!(g.ready(68_000, true, false));
        // Boot over and the log saved: at once.
        let mut g = ProbeGate::new(8_000);
        assert!(g.ready(10_000, true, true));
        // The log saved before the boot is over does not start it early.
        let mut g = ProbeGate::new(8_000);
        assert!(!g.ready(1_000, false, true));
        assert!(g.ready(2_000, true, true));
    }
}
