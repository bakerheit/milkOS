//! TLS 1.3 for kernel code (the Browser's https:// hook): the shared `tls` crate
//! (`<repo>/tls`) over kernel TCP sockets, with the compiled-in Mozilla roots, the kernel
//! CSPRNG and the RTC-backed wall clock. See docs/tls.md.
//!
//! ```ignore
//! let mut s = net::tls::connect("example.org", addr)?;   // TCP + handshake, verified
//! s.write_all(b"GET / HTTP/1.1\r\n...")?;
//! let n = s.read(&mut buf)?;
//! ```
//!
//! Test boots: when a test script (scripts/tls-interop.sh) passes fw_cfg
//! `opt/milkos/tlstest-root`, [`init`] says so loudly and that test CA is trusted on top of
//! the Mozilla roots for this boot (by the Browser, and by user space through `/proc/net/tls`),
//! so a page from a local test server loads. Normal boots have no such file and trust the
//! Mozilla roots only. [`selftest`] runs the cases of fw_cfg `opt/milkos/tlstest` (see
//! `tls::interop`) with the test CA alone, and `roots=mozilla` cases with the Mozilla roots
//! alone.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::errno::{self, EAGAIN, EALREADY, EINPROGRESS};
use super::socket::{self, Family, POLLIN, POLLOUT, SockType, SocketId};
use super::wire::{Ipv4, SockAddr};
use crate::sync::SpinLock;
use crate::{fwcfg, kprintln, timer};

#[allow(unused_imports)] // for the Browser
pub use tls::{Client, Config, RootStore, Stream, TlsError, Transport};

/// How long a read or write may stall before the connection is given up.
const TIMEOUT_NS: u64 = 15_000_000_000;

static ROOTS: SpinLock<Option<Arc<RootStore>>> = SpinLock::new(None);

/// A test CA this boot trusts on top of the Mozilla roots: only when a test script passed
/// fw_cfg `opt/milkos/tlstest-root` (scripts/tls-interop.sh). Read once by [`init`].
static TEST_ROOT: SpinLock<Option<Vec<u8>>> = SpinLock::new(None);

/// Boot: look for a test root in fw_cfg (normal boots have none, and trust only Mozilla's).
pub fn init() {
    let Some(der) = fw_file("opt/milkos/tlstest-root", 16384) else {
        return;
    };
    let mut probe = RootStore::empty();
    match probe.add_der(&der) {
        Ok(()) => {
            kprintln!(
                "[tls] WARNING: this boot trusts a TEST root from fw_cfg (opt/milkos/tlstest-root, {} bytes): a test run, not for real use",
                der.len()
            );
            *TEST_ROOT.lock() = Some(der);
        }
        Err(e) => kprintln!("[tls] the fw_cfg test root doesn't parse ({e}): ignored"),
    }
}

/// The test root of this boot, if a test passed one.
pub fn test_root() -> Option<Vec<u8>> {
    TEST_ROOT.lock().clone()
}

/// The system root store: the Mozilla roots (plus the test root of a test boot), parsed once.
pub fn roots() -> Arc<RootStore> {
    if let Some(r) = ROOTS.lock().as_ref() {
        return r.clone();
    }
    // Parse outside the lock (a few ms), then publish; a racing parse is just dropped.
    let mut store = RootStore::mozilla();
    if let Some(t) = test_root() {
        let _ = store.add_der(&t);
    }
    let parsed = Arc::new(store);
    let mut g = ROOTS.lock();
    g.get_or_insert(parsed).clone()
}

/// `/proc/net/tls`: the root store, and a test boot's extra root (base64 DER) for user space
/// (rt::tls trusts it too, so `network get` and the Browser agree).
pub fn proc_file(_st: &super::Stack, w: &mut dyn core::fmt::Write) -> core::fmt::Result {
    writeln!(w, "mozilla_roots={}", tls::x509::split_der(tls::roots::MOZILLA_ROOTS_DER).count())?;
    match TEST_ROOT.try_lock().and_then(|g| g.clone()) {
        Some(der) => writeln!(w, "test_root={}", base64(&der)),
        None => writeln!(w, "test_root=none"),
    }
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn kernel_rng(out: &mut [u8]) {
    crate::crypto::rng::fill(out);
}

fn wall_clock() -> Option<u64> {
    let s = crate::fs::clock::now().sec;
    (s > 0).then_some(s as u64)
}

/// A client config with the given roots and the kernel's RNG and clock.
pub fn config_with(roots: Arc<RootStore>) -> Config {
    Config::new(roots, Arc::new(wall_clock), Arc::new(kernel_rng))
}

/// The client everything kernel-side uses: Mozilla roots, ALPN http/1.1.
pub fn client() -> Client {
    Client::new(config_with(roots()))
}

/// A connected kernel TCP socket as a TLS transport. Closes the socket when dropped.
pub struct SocketTransport {
    pub socket: SocketId,
    pub timeout_ns: u64,
}

impl Drop for SocketTransport {
    fn drop(&mut self) {
        let _ = socket::close(self.socket);
    }
}

fn io_err(e: crate::fs::errno::Errno) -> tls::IoError {
    tls::IoError::Failed(String::from(errno::name(e)))
}

impl Transport for SocketTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, tls::IoError> {
        loop {
            match socket::recv(self.socket, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e == EAGAIN => {
                    if socket::wait(self.socket, POLLIN, self.timeout_ns) == 0 {
                        return Err(tls::IoError::TimedOut);
                    }
                }
                Err(e) => return Err(io_err(e)),
            }
        }
    }

    fn write_all(&mut self, mut data: &[u8]) -> Result<(), tls::IoError> {
        while !data.is_empty() {
            match socket::send(self.socket, data) {
                Ok(n) => data = &data[n.min(data.len())..],
                Err(e) if e == EAGAIN => {
                    if socket::wait(self.socket, POLLOUT, self.timeout_ns) == 0 {
                        return Err(tls::IoError::TimedOut);
                    }
                }
                Err(e) => return Err(io_err(e)),
            }
        }
        Ok(())
    }
}

/// A blocking TCP connect with a timeout (thread context).
pub fn tcp_connect(addr: SockAddr, timeout_ns: u64) -> Result<SocketTransport, String> {
    let fam = if addr.ip.is_v6() { Family::V6 } else { Family::V4 };
    let s = socket::socket_family(fam, SockType::Stream).map_err(|e| format!("socket: {}", errno::name(e)))?;
    let t = SocketTransport {
        socket: s,
        timeout_ns,
    };
    loop {
        match socket::connect(s, addr) {
            Ok(()) => return Ok(t),
            Err(e) if e == EINPROGRESS || e == EALREADY => {
                if socket::wait(s, POLLOUT, timeout_ns) == 0 {
                    return Err(format!("{addr}: timed out connecting"));
                }
            }
            Err(e) => return Err(format!("can't connect to {addr}: {}", errno::name(e))),
        }
    }
}

/// Connect to `addr` and run the TLS handshake for `host` (what the certificate must name)
/// with the system roots. Errors are sentences for the user.
pub fn connect(host: &str, addr: SockAddr) -> Result<Stream<SocketTransport>, String> {
    connect_with(&client(), host, addr)
}

pub fn connect_with(client: &Client, host: &str, addr: SockAddr) -> Result<Stream<SocketTransport>, String> {
    let t = tcp_connect(addr, TIMEOUT_NS)?;
    client.connect(host, t).map_err(|e| e.to_string())
}

// ---- the interop self-test (scripts/tls-interop.sh) ------------------------------------

fn fw_file(name: &str, max: usize) -> Option<Vec<u8>> {
    if !fwcfg::available() {
        return None;
    }
    let f = fwcfg::find_file(name)?;
    let n = (f.size as usize).min(max);
    let mut buf = vec![0u8; n];
    fwcfg::read_file(f, &mut buf).then_some(buf)
}

/// The guest half of scripts/tls-interop.sh (thread context, after the network is up).
pub fn selftest() {
    let Some(cfg) = fw_file("opt/milkos/tlstest", 4096) else {
        return;
    };
    let Some(root) = test_root() else {
        kprintln!("[tls] FAIL: opt/milkos/tlstest without a usable opt/milkos/tlstest-root");
        return;
    };
    // The test CA only: not the Mozilla roots, and never added to them.
    let mut store = RootStore::empty();
    if let Err(e) = store.add_der(&root) {
        kprintln!("[tls] FAIL: the test root doesn't parse: {e}");
        return;
    }
    let store = Arc::new(store);
    let t0 = timer::now_ns();
    let mozilla = Arc::new(RootStore::mozilla());
    kprintln!(
        "[tls] {} Mozilla roots parsed in {} ms; system store {} roots (with the test root)",
        mozilla.len(),
        (timer::now_ns() - t0) / 1_000_000,
        roots().len()
    );
    let text = String::from_utf8_lossy(&cfg).into_owned();
    let (mut pass, mut fail) = (0, 0);
    let mut connect = |a: &str| -> Result<SocketTransport, String> {
        let (ip, port) = a.split_once(':').ok_or_else(|| format!("bad address {a}"))?;
        let ip = Ipv4::parse(ip).ok_or_else(|| format!("bad address {a}"))?;
        let port = port.parse().map_err(|_| format!("bad address {a}"))?;
        tcp_connect(SockAddr::new(ip, port), TIMEOUT_NS)
    };
    let now_ms = || timer::now_ns() / 1_000_000;
    for word in text.split_whitespace() {
        let r = tls::interop::parse_case(word).and_then(|c| {
            let out = tls::interop::run(
                &c,
                &store,
                &mozilla,
                Arc::new(wall_clock),
                Arc::new(kernel_rng),
                &mut connect,
                &now_ms,
            );
            out.map(|m| (c.label.clone(), m)).map_err(|m| format!("{}: {m}", c.label))
        });
        match r {
            Ok((label, m)) => {
                kprintln!("[tls] {label}: ok: {m}");
                pass += 1;
            }
            Err(m) => {
                kprintln!("[tls] FAIL {m}");
                fail += 1;
            }
        }
    }
    if fail == 0 && pass > 0 {
        kprintln!("[tls] selftest ok: {pass} cases");
    } else {
        kprintln!("[tls] selftest FAILED: {fail} of {} cases", pass + fail);
    }
}
