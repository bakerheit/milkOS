//! A fetch response read in pieces: `response.body` is a stream whose pieces come from the app
//! as they arrive, the app is told how much the page has used (so it can send more, and no
//! more), `.text()` and its kin keep everything up to a limit that follows the machine's memory,
//! and the page's own aborts, errors and clones work over it.

use alloc::string::String;
use alloc::vec::Vec;

use super::nettest::{Reply, fail, head, run, run_limits, run_with, t_lines};
use crate::api::*;

/// [`run_with`] for a few seconds.
fn run_with_until(js: &str, serve: impl FnMut(&NetRequest) -> Vec<Reply>) -> super::nettest::Run {
    run_with(js, 150, serve)
}

/// Head, `n` chunks of `size` bytes (a counting pattern), then the end.
fn chunks(req: &NetRequest, n: usize, size: usize) -> Vec<Reply> {
    let mut h = head(200, "OK", &[("content-type", "application/octet-stream")]);
    h.url = req.url.clone();
    let mut out = alloc::vec![Reply { after: 0, ev: NetEvent::Head(h) }];
    for k in 0..n {
        let piece: Vec<u8> = (0..size).map(|i| ((k * size + i) & 255) as u8).collect();
        out.push(Reply { after: 0, ev: NetEvent::Chunk(piece) });
    }
    out.push(Reply { after: 0, ev: NetEvent::Done(Vec::new()) });
    out
}

const READ_ALL: &str = "fetch('/big').then(async r => {
    const rd = r.body.getReader(); let total = 0, pieces = 0, sum = 0;
    for (;;) { const x = await rd.read(); if (x.done) break; pieces++; total += x.value.length; for (let i = 0; i < x.value.length; i += 997) sum += x.value[i]; }
    console.log('T read ' + total + ' in ' + pieces + ' pieces sum ' + sum);
 }).catch(e => console.log('T error ' + e.message));";

#[test]
fn the_body_is_a_stream_of_the_pieces_the_app_sent() {
    let r = run(READ_ALL, |req| chunks(req, 5, 1000));
    let t = t_lines(&r);
    assert!(t.iter().any(|l| l.starts_with("read 5000 in 5 pieces sum ")), "{:?}", r.lines);
    assert!(r.requests[0].stream, "fetch asks for pieces");
    // Every piece the page read was acknowledged, in all and in order.
    let last = r.acks.last().copied();
    assert_eq!(last, Some((r.requests[0].id, 5000)), "{:?}", r.acks);
    assert!(r.acks.windows(2).all(|w| w[0].1 <= w[1].1));
    assert_eq!(r.pending, 0);
}

#[test]
fn a_body_the_page_does_not_read_is_not_acknowledged() {
    // The page looks at the status and leaves the body: the app is told nothing, so it stops a
    // window ahead and the connection waits.
    let r = run(
        "fetch('/big').then(r => console.log('T status ' + r.status));",
        |req| chunks(req, 5, 1000),
    );
    assert!(t_lines(&r).contains(&"status 200"), "{:?}", r.lines);
    assert!(r.acks.is_empty(), "{:?}", r.acks);
}

#[test]
fn a_page_that_reads_slowly_is_acknowledged_slowly() {
    // The pieces come at once and stay in the queue, the page reads one every 300 ms: the app is
    // told about each piece the page takes (and one ahead, the stream's own place), as long as
    // the request is still going; after its end there is nothing to send more of.
    let r = run_with_until(
        "var n = 0;
         fetch('/big').then(r => {
            const rd = r.body.getReader();
            const next = () => rd.read().then(x => { if (x.done) { console.log('T done ' + n); return; } n += x.value.length; setTimeout(next, 300); });
            next();
         });",
        // The end of the response comes late, after a few reads.
        |req| {
            let mut v = chunks(req, 6, 500);
            let end = v.pop().unwrap();
            v.push(Reply { after: 30, ev: end.ev });
            v
        },
    );
    assert!(t_lines(&r).contains(&"done 3000"), "{:?}", r.lines);
    // One acknowledgement per piece moved to the reader, in order, while the request was going:
    // the reading set the pace (six pieces came at once; the end came after 600 ms, when the
    // page had taken four).
    assert!(r.acks.len() >= 3 && r.acks.len() < 6, "{:?}", r.acks);
    assert_eq!(r.acks[0].1, 500);
    assert!(r.acks.windows(2).all(|w| w[1].1 - w[0].1 == 500));
}

#[test]
fn text_and_json_keep_everything_and_acknowledge_at_once() {
    let r = run(
        "fetch('/big').then(r => r.arrayBuffer()).then(b => console.log('T bytes ' + b.byteLength + ' ' + new Uint8Array(b)[1500]));",
        |req| chunks(req, 4, 1000),
    );
    // Byte 1500 of the counting pattern is 1500 & 255.
    assert!(t_lines(&r).contains(&"bytes 4000 220"), "{:?}", r.lines);
    assert_eq!(r.acks.last().map(|a| a.1), Some(4000));
    // A body that has no pieces (an app that does not stream) still works, as it did.
    let r = run(
        "fetch('/x').then(r => r.text()).then(t => console.log('T text ' + t));",
        |req| super::nettest::reply(req, 200, &[], "whole body"),
    );
    assert!(t_lines(&r).contains(&"text whole body"), "{:?}", r.lines);
}

#[test]
fn whole_body_reads_stop_at_the_memory_limit_but_pieces_do_not() {
    let small = ScriptLimits { buffer: 3000, ..ScriptLimits::default() };
    // text() over the limit: an error the page can catch, and the request is cancelled.
    let r = run_limits(
        "fetch('/big').then(r => r.text()).then(() => console.log('T text ok'), e => console.log('T error ' + e.name + ': ' + e.message.slice(0, 40)));",
        40,
        small,
        |req| chunks(req, 10, 1000),
    );
    assert!(t_lines(&r).iter().any(|l| l.starts_with("error TypeError: The response body is bigger than this pa")), "{:?}", r.lines);
    assert!(!r.cancelled.is_empty(), "the rest is not sent: {:?}", r.cancelled);
    // Reading by pieces: no limit.
    let r = run_limits(READ_ALL, 40, small, |req| chunks(req, 10, 1000));
    assert!(t_lines(&r).iter().any(|l| l.starts_with("read 10000 in 10 pieces")), "{:?}", r.lines);
    // Just under the limit: fine.
    let r = run_limits(
        "fetch('/big').then(r => r.text()).then(t => console.log('T text ' + t.length));",
        40,
        small,
        |req| chunks(req, 2, 1000),
    );
    assert!(t_lines(&r).contains(&"text 2000"), "{:?}", r.lines);
}

#[test]
fn a_response_can_be_cancelled_and_an_abort_ends_the_stream() {
    let r = run(
        "fetch('/big').then(async r => {
            const rd = r.body.getReader();
            await rd.read();
            await rd.cancel();
            console.log('T cancelled');
         });",
        |req| chunks(req, 5, 1000),
    );
    assert!(t_lines(&r).contains(&"cancelled"), "{:?}", r.lines);
    assert!(!r.cancelled.is_empty(), "the app is told to stop: {:?}", r.cancelled);
    let r = run(
        "const ac = new AbortController();
         fetch('/big', { signal: ac.signal }).then(async r => {
            const rd = r.body.getReader();
            await rd.read();
            ac.abort();
            try { await rd.read(); console.log('T no error'); } catch (e) { console.log('T aborted ' + e.name); }
         });",
        |req| {
            let mut v = chunks(req, 5, 1000);
            // Nothing after the first piece until the page has aborted.
            v.truncate(2);
            v
        },
    );
    assert!(t_lines(&r).iter().any(|l| l.starts_with("aborted")), "{:?}", r.lines);
}

#[test]
fn an_error_in_the_middle_reaches_the_reader() {
    let r = run(
        "fetch('/big').then(async r => {
            const rd = r.body.getReader(); let n = 0;
            try { for (;;) { const x = await rd.read(); if (x.done) break; n += x.value.length; } console.log('T finished ' + n); }
            catch (e) { console.log('T failed after ' + n + ' ' + e.name); }
         });",
        |req| {
            let mut v = chunks(req, 3, 800);
            v.truncate(3); // head and two pieces
            v.extend(fail(NetFailureKind::Network, "the connection broke"));
            v
        },
    );
    let t = t_lines(&r);
    assert!(t.iter().any(|l| l.starts_with("failed after 1600 TypeError")), "{:?}", r.lines);
    let _ = String::new();
}

#[test]
fn a_clone_keeps_the_whole_body_for_both_readers() {
    let r = run(
        "fetch('/big').then(async r => {
            const c = r.clone();
            const a = await r.text();
            const b = await c.arrayBuffer();
            console.log('T both ' + a.length + ' ' + b.byteLength);
         });",
        |req| chunks(req, 3, 1000),
    );
    assert!(t_lines(&r).contains(&"both 3000 3000"), "{:?}", r.lines);
}

#[test]
fn xhr_and_the_other_requests_do_not_stream() {
    let r = run(
        "const x = new XMLHttpRequest(); x.open('GET', '/x'); x.onload = () => console.log('T xhr ' + x.responseText); x.send();
         navigator.sendBeacon('/b', 'x');",
        |req| super::nettest::reply(req, 200, &[], "xhr body"),
    );
    assert!(t_lines(&r).contains(&"xhr xhr body"), "{:?}", r.lines);
    assert!(r.requests.iter().all(|q| !q.stream), "only fetch asks for pieces");
}
