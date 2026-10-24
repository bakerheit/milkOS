//! The heap budget moves while a page runs: the host answers `heap_allowance` from what the
//! machine has, a page can grow far past the default budget when it is given room, and a page
//! that holds more than it is allowed is stopped alone, with a reason.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::script::{Host, doc_with};
use crate::api::*;

/// A page whose first script holds `n` objects (a few hundred bytes each).
fn hog_page(n: usize) -> String {
    format!(
        "<script>var held = []; for (var i = 0; i < {n}; i++) {{ held.push({{ id: i, name: 'item-' + i, tags: [i, i + 1] }}); for (var j = 0; j < 12; j++) {{}} }} console.log('HELD ' + held.length);</script>\
         <script>console.log('AFTER');</script>"
    )
}

fn console(d: &mut Document) -> Vec<String> {
    d.take_console().into_iter().map(|l| l.text).collect()
}

fn small_limits(memory: usize) -> ScriptLimits {
    ScriptLimits {
        memory,
        ..ScriptLimits::default()
    }
}

#[test]
fn a_page_over_its_small_budget_is_stopped() {
    let mut h = Host::new();
    let mut d = doc_with(&hog_page(60_000), small_limits(8 << 20));
    d.run(&mut h);
    let c = console(&mut d);
    assert!(
        c.iter()
            .any(|l| l.contains("ran out of memory") || l.contains("out of memory")),
        "{c:?}"
    );
    assert!(!c.iter().any(|l| l.contains("AFTER")), "{c:?}");
    assert!(!d.scripting());
    assert_eq!(d.script_stop_reason(), "out of memory");
    let st = d.script_stats();
    assert!(st.stopped);
    // The heap is given back when the scripts stop; what it held is remembered for the message.
    assert_eq!(st.heap_bytes, 0);
    assert!(
        st.final_heap > 4 << 20 && st.heap_limit == 8 << 20,
        "{st:?}"
    );
    // The page itself is still there.
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
}

#[test]
fn the_host_gives_it_room_as_it_grows() {
    let mut h = Host::new();
    // 16 MiB above what is held, up to 400 MiB: the page's own 8 MiB start does not matter.
    h.allowance = Some(Box::new(|used| {
        Allowance::Limit((used + (16 << 20)).min(400 << 20))
    }));
    let mut d = doc_with(&hog_page(60_000), small_limits(8 << 20));
    d.run(&mut h);
    let c = console(&mut d);
    assert!(c.iter().any(|l| l == "HELD 60000"), "{c:?}");
    assert!(c.iter().any(|l| l == "AFTER"), "{c:?}");
    assert!(d.scripting());
    let st = d.script_stats();
    assert!(st.heap_limit > 8 << 20, "limit {}", st.heap_limit);
    assert!(
        st.heap_bytes > 8 << 20,
        "the page holds more than its start budget: {}",
        st.heap_bytes
    );
    assert!(h.allowance_calls >= 2);
}

#[test]
fn the_host_takes_room_back() {
    let mut h = Host::new();
    // Room at first, none after the page holds 12 MiB.
    h.allowance = Some(Box::new(|used| {
        if used < 12 << 20 {
            Allowance::Limit(64 << 20)
        } else {
            Allowance::Limit(used.min(10 << 20))
        }
    }));
    let mut d = doc_with(&hog_page(60_000), small_limits(64 << 20));
    d.run(&mut h);
    let c = console(&mut d);
    assert!(
        !c.iter().any(|l| l == "HELD 60000"),
        "it did not get to keep growing: {c:?}"
    );
    assert!(!d.scripting());
    assert_eq!(d.script_stop_reason(), "out of memory");
}

#[test]
fn the_host_can_stop_the_scripts_with_a_reason() {
    let mut h = Host::new();
    h.allowance = Some(Box::new(|used| {
        if used > 4 << 20 {
            Allowance::Stop(String::from("the page used too much memory"))
        } else {
            Allowance::Keep
        }
    }));
    let mut d = doc_with(&hog_page(60_000), small_limits(64 << 20));
    d.run(&mut h);
    let c = console(&mut d);
    assert!(
        c.iter()
            .any(|l| l.contains("scripts were stopped (the page used too much memory)")),
        "{c:?}"
    );
    assert!(!c.iter().any(|l| l == "AFTER"));
    assert!(!d.scripting());
    assert_eq!(d.script_stop_reason(), "the page used too much memory");
}

#[test]
fn a_stop_before_any_script_runs_stops_cleanly() {
    let mut h = Host::new();
    h.allowance = Some(Box::new(|_| {
        Allowance::Stop(String::from("no memory at all"))
    }));
    let mut d = doc_with(
        "<script>console.log('never');</script><p>text</p>",
        ScriptLimits::default(),
    );
    assert!(!d.run(&mut h));
    let c = console(&mut d);
    assert!(!c.iter().any(|l| l == "never"), "{c:?}");
    assert!(!d.scripting());
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    assert!(d.text().contains("text"), "the page itself stays");
}

#[test]
fn hostile_pages_still_stop_with_roomy_budgets() {
    // The hostile tests of the script engine, with a host that is generous: a runaway
    // allocation loop, a huge string, a pile of timers are still stopped or refused.
    let mut h = Host::new();
    h.allowance = Some(Box::new(|used| {
        Allowance::Limit((used + (8 << 20)).min(48 << 20))
    }));
    let mut d = doc_with(
        "<script>var a = []; while (true) a.push(new Array(100000).join('x'));</script><script>console.log('after the bomb');</script>",
        small_limits(8 << 20),
    );
    d.run(&mut h);
    let c = console(&mut d);
    assert!(c.iter().any(|l| l.contains("out of memory")), "{c:?}");
    assert!(!c.iter().any(|l| l.contains("after the bomb")), "{c:?}");
    assert!(!d.scripting());
    assert!(d.script_stats().heap_limit <= 48 << 20 + 1);
    let mut h = Host::new();
    let mut d = doc_with(
        "<script>var s = 'x'; try { for (var i = 0; i < 40; i++) s = s + s; console.log('LEN ' + s.length); } catch (e) { console.log('E ' + e.name); }</script>",
        small_limits(32 << 20),
    );
    d.run(&mut h);
    let c = console(&mut d);
    assert!(
        c.iter()
            .any(|l| l.starts_with("E ") || l.contains("out of memory")),
        "{c:?}"
    );
}
