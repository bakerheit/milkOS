//! tlsprobe: run tls::interop cases from the Mac against OpenSSL's s_server on 127.0.0.1
//! (scripts/tls-interop.sh). Usage:
//!
//!     tlsprobe ROOT.der CASE...
//!
//! ROOT.der is the throwaway test CA (the only root trusted, except for roots=mozilla
//! cases, which use the compiled-in Mozilla roots). Prints one line per case; exits 1 if
//! any case failed.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tls::{IoError, RootStore, Transport};

struct Tcp(TcpStream);

impl Transport for Tcp {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.0.read(buf).map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => IoError::TimedOut,
            _ => IoError::Failed(e.to_string()),
        })
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), IoError> {
        self.0.write_all(data).map_err(|e| IoError::Failed(e.to_string()))
    }
}

fn urandom(out: &mut [u8]) {
    let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom");
    f.read_exact(out).expect("/dev/urandom");
}

fn now() -> Option<u64> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((root, cases)) = args.split_first() else {
        eprintln!("usage: tlsprobe ROOT.der CASE...");
        std::process::exit(2);
    };
    let der = std::fs::read(root).unwrap_or_else(|e| {
        eprintln!("tlsprobe: {root}: {e}");
        std::process::exit(2)
    });
    let mut test = RootStore::empty();
    test.add_der(&der).expect("the test root parses");
    let test = Arc::new(test);
    let system = Arc::new(RootStore::mozilla());
    let start = Instant::now();
    let now_ms = || start.elapsed().as_millis() as u64;
    let mut connect = |a: &str| -> Result<Tcp, String> {
        let s = TcpStream::connect(a).map_err(|e| format!("connect {a}: {e}"))?;
        s.set_read_timeout(Some(Duration::from_secs(15))).ok();
        s.set_nodelay(true).ok();
        Ok(Tcp(s))
    };
    let mut failed = 0;
    for word in cases {
        let r = tls::interop::parse_case(word).and_then(|c| {
            tls::interop::run(&c, &test, &system, Arc::new(now), Arc::new(urandom), &mut connect, &now_ms)
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
        if failed == 0 { "ok" } else { "FAILED" },
        cases.len() - failed,
        cases.len()
    );
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
