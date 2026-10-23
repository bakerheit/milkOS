//! tlsprobe: run TLS interop cases from user space (rt::tls), for scripts/tls-interop.sh.
//!
//!   /System/Developer/tlsprobe ROOT CASES
//!
//! ROOT is a file with the test CA, as DER or base64 (whitespace ignored); it is the only
//! root trusted, except by `roots=mozilla` cases (the compiled-in Mozilla roots). CASES is
//! a file of cases separated by whitespace, in the form of tls::interop (one per line is
//! fine). Prints a line per case and a summary; exits 1 if any case failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use rt::net;
use rt::tls::{self, RootStore};
use rt::{eprintln, println, sys};

rt::main!(main);

fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

fn main(args: &rt::Args) -> i32 {
    let (Some(root), Some(cases)) = (args.argv.get(1), args.argv.get(2)) else {
        eprintln!("usage: tlsprobe ROOT CASES");
        return 2;
    };
    let read = |p: &str| sys::read_file(p).map_err(|e| format!("{p}: {}", e.name()));
    let root = match read(root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tlsprobe: {e}");
            return 2;
        }
    };
    let der = if root.first() == Some(&0x30) { Some(root) } else { tls::base64(&String::from_utf8_lossy(&root)) };
    let mut test = RootStore::empty();
    match der.map(|d| test.add_der(&d)) {
        Some(Ok(())) => {}
        _ => {
            eprintln!("tlsprobe: the root isn't a certificate");
            return 2;
        }
    }
    let test = Arc::new(test);
    let system = Arc::new(RootStore::mozilla());
    let text = match read(cases) {
        Ok(t) => String::from_utf8_lossy(&t).into_owned(),
        Err(e) => {
            eprintln!("tlsprobe: {e}");
            return 2;
        }
    };
    let mut connect = |a: &str| -> Result<tls::FdTransport, String> {
        let addr = net::lookup(a, 443)?;
        let (fd, _) = net::connect_any(&[addr], net::SOCK_STREAM)?;
        let _ = net::set_recv_timeout(fd, 15_000);
        Ok(tls::FdTransport { fd })
    };
    let (mut total, mut failed) = (0, 0);
    for word in text.split_whitespace() {
        total += 1;
        let r = tls::interop::parse_case(word).and_then(|c| {
            tls::interop::run(
                &c,
                &test,
                &system,
                Arc::new(tls::unix_seconds),
                Arc::new(tls::getrandom),
                &mut connect,
                &now_ms,
            )
            .map(|m| (c.label.clone(), m))
            .map_err(|m| format!("{}: {m}", c.label))
        });
        match r {
            Ok((label, m)) => println!("[tlsprobe] {label}: ok: {m}"),
            Err(m) => {
                println!("[tlsprobe] FAIL {m}");
                failed += 1;
            }
        }
    }
    println!(
        "[tlsprobe] {}: {} of {} cases passed",
        if failed == 0 && total > 0 { "ok" } else { "FAILED" },
        total - failed,
        total
    );
    if failed == 0 && total > 0 { 0 } else { 1 }
}
