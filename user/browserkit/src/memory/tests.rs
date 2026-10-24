//! The budget calculation with fake memory inputs: several tabs, floors, ceilings, the
//! pressure ladder, and random inputs that must never panic or overflow.

use super::*;
use alloc::vec;

const METER: &str = "MemTotal:           524288 kB\nMemFree:            452552 kB\nMemAvailable:       450000 kB\nShmem: 0 kB\n";
const PRESS: &str = "level:          normal\nMemFree:            452552 kB\nLowMark:             65536 kB\nCriticalMark:        32768 kB\nMinFree:            400000 kB\nLowEvents:                 2\nCriticalEvents:            1\nAllocFailures:             3\n";

fn page(key: u64, fg: bool, used: usize) -> Realm {
    Realm { key, kind: Kind::Page, foreground: fg, used }
}
fn frame(key: u64, fg: bool, used: usize) -> Realm {
    Realm { key, kind: Kind::Frame, foreground: fg, used }
}
fn worker(key: u64, fg: bool, used: usize) -> Realm {
    Realm { key, kind: Kind::Worker, foreground: fg, used }
}

/// A machine of `total` MiB with `avail` MiB free.
fn mach(total: usize, avail: usize) -> Machine {
    Machine::new(total * MIB, avail * MIB)
}

#[test]
fn parses_the_kernel_files() {
    let m = Machine::parse(METER, PRESS).unwrap();
    assert_eq!(m.total, 512 * MIB);
    assert_eq!(m.available, 450_000 * KIB);
    assert_eq!(m.low, 64 * MIB);
    assert_eq!(m.critical, 32 * MIB);
    assert_eq!((m.low_events, m.critical_events, m.failures), (2, 1, 3));
    // No pressure file: the usual marks. No MemAvailable: MemFree.
    let m = Machine::parse("MemTotal: 1048576 kB\nMemFree: 1000 kB\n", "").unwrap();
    assert_eq!((m.total, m.available, m.low, m.critical), (1024 * MIB, 1000 * KIB, 128 * MIB, 64 * MIB));
    // Garbage.
    assert!(Machine::parse("", "").is_none());
    assert!(Machine::parse("MemTotal: lots kB\n", "").is_none());
    assert!(Machine::parse("MemTotal: 0 kB\nMemFree: 0 kB\n", "").is_none());
    assert!(Machine::parse("MemFree: 5 kB\n", "level: low\n").is_none());
    // Free memory above the total, marks above the total: all clamped.
    let m = Machine::parse("MemTotal: 100 kB\nMemAvailable: 999 kB\n", "LowMark: 5000 kB\nCriticalMark: 9000 kB\n").unwrap();
    assert!(m.available <= m.total && m.low <= m.total && m.critical <= m.low);
    let m = Machine::parse(&format!("MemTotal: {} kB\nMemFree: {} kB\n", usize::MAX, usize::MAX), "");
    assert!(m.is_none() || m.unwrap().available > 0);
}

#[test]
fn floor_and_room_scale_with_ram() {
    for (total, floor, room) in [(512, 96, 354), (1024, 192, 832), (2048, 384, 1600)] {
        let m = Machine::new(total * MIB, (total - 70) * MIB);
        assert_eq!(m.floor(), floor * MIB, "floor of {total} MiB");
        assert_eq!(m.room(), (total - 70 - floor) * MIB, "room of {total} MiB");
        let _ = room;
    }
    // A small machine still keeps 32 MiB in reserve.
    assert_eq!(Machine::new(128 * MIB, 100 * MIB).reserve(), 32 * MIB);
    // No room when little is available.
    assert_eq!(Machine::new(512 * MIB, 90 * MIB).room(), 0);
    assert_eq!(Machine::new(512 * MIB, 0).room(), 0);
}

#[test]
fn levels() {
    let t = 512 * MIB;
    assert_eq!(Machine::new(t, 400 * MIB).level(), Level::Normal);
    // Room under a sixty-fourth of RAM (at least 8 MiB): tight.
    assert_eq!(Machine::new(t, 96 * MIB + 7 * MIB).level(), Level::Tight);
    assert_eq!(Machine::new(t, 96 * MIB + 20 * MIB).level(), Level::Normal);
    assert_eq!(Machine::new(t, 80 * MIB).level(), Level::Tight);
    assert_eq!(Machine::new(t, 63 * MIB).level(), Level::Low);
    assert_eq!(Machine::new(t, 31 * MIB).level(), Level::Critical);
    assert_eq!(Machine::new(t, 0).level(), Level::Critical);
    assert!(Level::Critical > Level::Low && Level::Low > Level::Tight && Level::Tight > Level::Normal);
    assert_eq!(Machine::default().level(), Level::Normal);
}

#[test]
fn one_page_gets_most_of_the_room() {
    let m = mach(512, 440);
    let b = plan(&m, &[page(1, true, 0)]);
    let l = b.limit_of(1).unwrap();
    // Far past the old 64 MB cap, and well within what is free.
    assert!(l > 100 * MIB, "limit {} MiB", l / MIB);
    assert!(to_real(l) < m.available - m.floor() + MIB);
    // It grows on a roomy machine and the same page gets less on a tight one.
    let big = plan(&mach(2048, 1900), &[page(1, true, 0)]).limit_of(1).unwrap();
    let small = plan(&mach(512, 160), &[page(1, true, 0)]).limit_of(1).unwrap();
    assert!(big > l && l > small, "{big} {l} {small}");
    assert!(big >= 500 * MIB, "a roomy machine: {} MiB", big / MIB);
    // No room: the minimum, and no more.
    assert_eq!(plan(&mach(512, 90), &[page(1, true, 0)]).limit_of(1), Some(16 * MIB));
}

#[test]
fn a_realm_keeps_what_it_holds() {
    let m = mach(512, 90); // no room
    let b = plan(&m, &[page(1, true, 40 * MIB), frame(2, true, 2 * MIB), worker(3, false, 30 * MIB)]);
    // Holding 40 MiB with no room: that and an eighth, not a byte of growth beyond.
    assert_eq!(b.limit_of(1), Some(40 * MIB + 5 * MIB + 256 * KIB));
    // A small one gets the minimum.
    assert_eq!(b.limit_of(2), Some(8 * MIB));
    assert_eq!(b.limit_of(3), Some(30 * MIB + 30 * MIB / 8 + 256 * KIB));
    assert_eq!(b.limit_of(99), None);
    assert_eq!(b.len(), 3);
}

#[test]
fn more_tabs_mean_less_each() {
    let m = mach(1024, 900);
    let one = plan(&m, &[page(1, true, 0)]).limit_of(1).unwrap();
    let mut realms = vec![page(1, true, 0)];
    for k in 0..8u64 {
        realms.push(page(10 + k, false, 0));
    }
    let nine = plan(&m, &realms);
    let fg = nine.limit_of(1).unwrap();
    let bg = nine.limit_of(10).unwrap();
    assert!(fg < one, "the page in front gets less with eight tabs behind it");
    assert!(bg < fg, "a background tab gets less than the one in front");
    assert!(bg >= min_limit(Kind::Page));
    // A page with its frames and a worker shares with them.
    let shared = plan(&m, &[page(1, true, 0), frame(2, true, 0), worker(3, true, 0)]);
    assert!(shared.limit_of(1).unwrap() < one);
    assert!(shared.limit_of(1) > shared.limit_of(2));
    assert_eq!(shared.limit_of(2), shared.limit_of(3));
    // A realm that is new now gets what the plan would give it.
    let new = nine.for_new(Kind::Page, true);
    assert!(new <= fg && new >= min_limit(Kind::Page));
}

#[test]
fn growth_never_adds_up_to_more_than_the_room() {
    // However the realms are spread, what they could still grow by (above what they hold,
    // for the ones that have more than the minimum) stays inside the room.
    let mut s = 0x1234_5678_9abc_def0u64;
    let mut rnd = |n: usize| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s % n as u64) as usize
    };
    for _ in 0..2000 {
        let total = (64 + rnd(4000)) * MIB;
        let m = Machine::new(total, rnd(total / MIB + 1) * MIB);
        let n = rnd(12);
        let realms: Vec<Realm> = (0..n)
            .map(|k| Realm {
                key: k as u64,
                kind: [Kind::Page, Kind::Frame, Kind::Worker][rnd(3)],
                foreground: rnd(3) == 0,
                used: rnd(300) * MIB / 4 + 20 * MIB,
            })
            .collect();
        let b = plan(&m, &realms);
        let mut grow = 0usize;
        for r in &realms {
            let l = b.limit_of(r.key).unwrap();
            assert!(l >= r.used.min(ceiling(&m, r.kind)), "never under what it holds");
            assert!(l <= ceiling(&m, r.kind).max(min_limit(r.kind)));
            // Growth beyond the slack every realm is given.
            let slack = r.used / 8 + 256 * KIB;
            grow += l.saturating_sub(r.used).saturating_sub(slack);
        }
        assert!(grow <= b.room_engine, "grow {grow} room {}", b.room_engine);
    }
}

#[test]
fn ceilings_come_from_ram() {
    for total in [256usize, 512, 1024, 4096] {
        let m = mach(total, total); // everything free
        let b = plan(&m, &[page(1, true, 0), worker(2, true, 0)]);
        assert!(b.limit_of(1).unwrap() <= ceiling(&m, Kind::Page));
        assert!(b.limit_of(2).unwrap() <= ceiling(&m, Kind::Worker));
        assert!(to_real(ceiling(&m, Kind::Page)) <= m.total, "a page can never be given more than the machine");
        assert!(ceiling(&m, Kind::Worker) <= ceiling(&m, Kind::Page));
    }
    // A page that holds more than the ceiling is given the ceiling: it will run out.
    let m = mach(512, 100);
    let c = ceiling(&m, Kind::Page);
    assert_eq!(plan(&m, &[page(1, true, c + 50 * MIB)]).limit_of(1), Some(c));
}

#[test]
fn units_convert() {
    assert_eq!(to_real(100), OVERHEAD_PCT);
    assert_eq!(to_engine(OVERHEAD_PCT), 100);
    for x in [0usize, 1, 99, 100, 12345, 1 << 30, usize::MAX / 1000] {
        assert!(to_engine(to_real(x)) <= x && to_engine(to_real(x)) + 2 >= x);
    }
    // No overflow at the extremes.
    let _ = to_real(usize::MAX);
    let _ = to_engine(usize::MAX);
}

#[test]
fn caps_follow_the_room() {
    let none = Caps::from_room(0);
    let some = Caps::from_room(300 * MIB);
    let lots = Caps::from_room(4096 * MIB);
    for (a, b) in [(none.script, some.script), (some.script, lots.script), (none.response, some.response), (some.nodes, lots.nodes), (none.style, some.style)] {
        assert!(a <= b);
    }
    // The old fixed caps are the smallest, so a roomy machine is never worse off.
    let old = Caps::default();
    assert!(none.script >= old.script && none.style >= old.style && none.image >= old.image);
    assert!(some.nodes >= old.nodes / 5);
    assert!(lots.nodes <= 8_000_000 && lots.script <= 128 * MIB);
    let b = plan(&mach(512, 440), &[]);
    assert_eq!(b.caps(), Caps::from_room(b.room));
    assert!(b.is_empty());
}

#[test]
fn the_ladder_climbs_one_step_at_a_time() {
    let mut l = Ladder::new();
    assert!(!l.active());
    assert_eq!(l.next(Level::Normal, 0), None);
    // Short of memory: caches first, at once.
    assert_eq!(l.next(Level::Tight, 100), Some(Step::DropCaches));
    assert!(l.active());
    // Then a second's wait between steps.
    assert_eq!(l.next(Level::Tight, 600), None);
    assert_eq!(l.next(Level::Tight, 1200), Some(Step::Collect));
    assert_eq!(l.next(Level::Tight, 1300), None);
    assert_eq!(l.next(Level::Tight, 2300), Some(Step::DiscardTab));
    // Tabs keep going while there are some to discard...
    assert_eq!(l.next(Level::Tight, 3400), Some(Step::DiscardTab));
    // ... and when there are none the ladder moves on without waiting.
    l.skip(Step::DiscardTab);
    assert_eq!(l.next(Level::Tight, 3500), Some(Step::AskStop));
    assert_eq!(l.next(Level::Tight, 4600), Some(Step::AskStop));
    // After ten seconds still short: caches again.
    assert_eq!(l.next(Level::Tight, 11_500), Some(Step::DropCaches));
    assert_eq!(l.next(Level::Tight, 12_600), Some(Step::Collect));
    // Fine for three seconds: all over, and the next time starts at the bottom.
    assert_eq!(l.next(Level::Normal, 13_000), None);
    assert!(l.active());
    assert_eq!(l.next(Level::Normal, 16_100), None);
    assert!(!l.active());
    assert_eq!(l.next(Level::Low, 16_200), Some(Step::DropCaches));
}

#[test]
fn critical_does_not_wait() {
    let mut l = Ladder::new();
    assert_eq!(l.next(Level::Critical, 0), Some(Step::DropCaches));
    assert_eq!(l.next(Level::Critical, 0), Some(Step::Collect));
    assert_eq!(l.next(Level::Critical, 0), Some(Step::DiscardTab));
    l.skip(Step::DiscardTab);
    assert_eq!(l.next(Level::Critical, 0), Some(Step::AskStop));
    // A dip back to normal and short again before three calm seconds: no starting over.
    assert_eq!(l.next(Level::Normal, 100), None);
    assert_eq!(l.next(Level::Critical, 200), Some(Step::AskStop));
}

#[test]
fn a_blip_resets_nothing() {
    let mut l = Ladder::new();
    assert_eq!(l.next(Level::Tight, 0), Some(Step::DropCaches));
    assert_eq!(l.next(Level::Normal, 500), None);
    assert_eq!(l.next(Level::Tight, 1500), Some(Step::Collect));
}

fn tab(index: usize, fg: bool, bytes: usize) -> TabInfo {
    TabInfo { index, foreground: fg, busy: false, touched: false, bytes, idle_ms: 5_000, discarded: false }
}

#[test]
fn which_tab_to_discard() {
    let tabs = [tab(0, true, 900 * MIB), tab(1, false, 20 * MIB), tab(2, false, 80 * MIB), tab(3, false, MIB / 4)];
    // Never the one in front; the biggest one behind.
    assert_eq!(pick_discard(&tabs, false), Some(2));
    // Not ones already discarded, still loading, or just left.
    let mut t = tabs;
    t[2].discarded = true;
    assert_eq!(pick_discard(&t, false), Some(1));
    t[1].busy = true;
    assert_eq!(pick_discard(&t, false), None);
    let mut t = tabs;
    t[2].idle_ms = 100;
    assert_eq!(pick_discard(&t, false), Some(1));
    // A tab with typing in it is kept, unless the machine is desperate.
    let mut t = tabs;
    t[2].touched = true;
    assert_eq!(pick_discard(&t, false), Some(1));
    assert_eq!(pick_discard(&t, true), Some(2));
    // Nothing worth discarding.
    assert_eq!(pick_discard(&[tab(0, true, 1 << 30), tab(1, false, 100 * KIB)], false), None);
    assert_eq!(pick_discard(&[], true), None);
}

#[test]
fn which_page_to_ask_about() {
    assert_eq!(pick_heaviest(&[]), None);
    assert_eq!(pick_heaviest(&[page(1, true, 5 * MIB)]), None, "a page that holds little is no one's problem");
    let rs = [page(1, true, 20 * MIB), frame(2, false, 90 * MIB), worker(3, false, 60 * MIB)];
    assert_eq!(pick_heaviest(&rs), Some(2));
    let rs = [page(1, true, 90 * MIB), frame(2, false, 90 * MIB)];
    assert_eq!(pick_heaviest(&rs), Some(1), "the one in front first among equals");
}

#[test]
fn words() {
    assert_eq!(human(0), "0 KB");
    assert_eq!(human(812 * KIB), "812 KB");
    assert_eq!(human(37 * MIB + 5), "37 MB");
    assert_eq!(human((1.2 * (1u64 << 30) as f64) as usize), "1.1 GB");
    assert_eq!(keys(&[page(7, true, 0), page(9, false, 0)]), vec![7, 9]);
}

#[test]
fn nothing_panics_on_nonsense() {
    let mut s = 0xdead_beef_cafe_f00du64;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for _ in 0..3000 {
        let pick = |r: u64| match r % 5 {
            0 => 0usize,
            1 => usize::MAX,
            2 => (r >> 8) as usize,
            3 => (r >> 40) as usize,
            _ => (r >> 20) as usize % (4 << 30),
        };
        let m = Machine {
            total: pick(rnd()),
            available: pick(rnd()),
            low: pick(rnd()),
            critical: pick(rnd()),
            ..Machine::default()
        };
        let realms: Vec<Realm> = (0..(rnd() % 20))
            .map(|k| Realm {
                key: k,
                kind: [Kind::Page, Kind::Frame, Kind::Worker][(rnd() % 3) as usize],
                foreground: rnd() % 2 == 0,
                used: pick(rnd()),
            })
            .collect();
        let b = plan(&m, &realms);
        let _ = (b.level, b.caps(), b.for_new(Kind::Worker, false), m.level(), m.floor(), m.room());
        for r in &realms {
            assert!(b.limit_of(r.key).is_some());
        }
        let _ = pick_heaviest(&realms);
        let mut l = Ladder::new();
        for t in 0..20u64 {
            let lv = [Level::Normal, Level::Tight, Level::Low, Level::Critical][(rnd() % 4) as usize];
            if let Some(st) = l.next(lv, t * (rnd() % 2000)) {
                l.skip(st);
            }
        }
    }
    let _ = Machine::parse(&"MemTotal: 9999999999999999999999 kB".repeat(3), "LowMark: -1 kB");
}
