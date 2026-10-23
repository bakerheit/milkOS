//! The interop test cases of `scripts/tls-interop.sh`, shared by the three clients that run
//! them against OpenSSL's `s_server`: the host probe (tls/probe), the kernel
//! (kernel/src/net/tls.rs) and the user-space tool. Nothing here is used outside tests.
//!
//! A case is one word: `LABEL,HOST:PORT,NAME[,key=value...]` with keys
//! * `expect=ok` (default) or `expect=TEXT`: the start of the expected error message,
//!   `_` for spaces;
//! * `suite=aes|chacha`: offer only that suite (and expect it);
//! * `group=x25519|secp256r1|secp384r1`: expect that group;
//! * `roots=mozilla`: verify with the Mozilla roots alone instead of the test CA;
//! * `get=PATH:SIZE:SHA256`: GET PATH from `s_server -WWW` and check the body; without it,
//!   GET / from `s_server -www` and check its status page names TLS 1.3 and the suite;
//! * `alpn=PROTO`: expect that ALPN answer.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::{CipherSuite, Client, Clock, Config, NamedGroup, RootStore, Rng, TlsError, Transport};

pub struct Case {
    pub label: String,
    pub addr: String,
    pub name: String,
    pub expect: String,
    pub suite: Option<CipherSuite>,
    pub group: Option<NamedGroup>,
    pub system_roots: bool,
    pub get: Option<(String, usize, [u8; 32])>,
    pub alpn: Option<String>,
}

pub fn parse_case(word: &str) -> Result<Case, String> {
    let bad = || format!("bad case '{word}'");
    let mut p = word.split(',');
    let mut c = Case {
        label: p.next().ok_or_else(bad)?.to_string(),
        addr: p.next().ok_or_else(bad)?.to_string(),
        name: p.next().ok_or_else(bad)?.to_string(),
        expect: String::from("ok"),
        suite: None,
        group: None,
        system_roots: false,
        get: None,
        alpn: None,
    };
    for kv in p {
        let (k, v) = kv.split_once('=').ok_or_else(bad)?;
        match k {
            "expect" => c.expect = v.replace('_', " "),
            "suite" => {
                c.suite = Some(match v {
                    "aes" => CipherSuite::Aes128GcmSha256,
                    "chacha" => CipherSuite::Chacha20Poly1305Sha256,
                    _ => return Err(bad()),
                })
            }
            "group" => {
                c.group = Some(match v {
                    "x25519" => NamedGroup::X25519,
                    "secp256r1" => NamedGroup::Secp256r1,
                    "secp384r1" => NamedGroup::Secp384r1,
                    _ => return Err(bad()),
                })
            }
            "roots" => c.system_roots = v == "mozilla",
            "alpn" => c.alpn = Some(v.to_string()),
            "get" => {
                let mut g = v.split(':');
                let path = g.next().ok_or_else(bad)?.to_string();
                let size = g.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
                let sha = g.next().and_then(milkcrypto::unhex::<32>).ok_or_else(bad)?;
                c.get = Some((path, size, sha));
            }
            _ => return Err(bad()),
        }
    }
    Ok(c)
}

/// Run one case. `connect` opens the TCP connection to `case.addr`; `now_ms` times the
/// handshake. Ok: a line describing what was negotiated (or the expected refusal).
#[allow(clippy::too_many_arguments)]
pub fn run<T: Transport>(
    c: &Case,
    test_roots: &Arc<RootStore>,
    system_roots: &Arc<RootStore>,
    clock: Arc<dyn Clock>,
    rng: Arc<dyn Rng>,
    connect: &mut dyn FnMut(&str) -> Result<T, String>,
    now_ms: &dyn Fn() -> u64,
) -> Result<String, String> {
    let roots = if c.system_roots { system_roots } else { test_roots };
    let mut cfg = Config::new(roots.clone(), clock, rng);
    if let Some(s) = c.suite {
        cfg.cipher_suites = vec![s];
    }
    let client = Client::new(cfg);
    let t0 = now_ms();
    let tcp = connect(&c.addr)?;
    let mut s = match client.connect(&c.name, tcp) {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            return if c.expect != "ok" && msg.starts_with(&c.expect) {
                Ok(format!("refused as expected: {msg}"))
            } else {
                Err(format!("handshake: {msg}"))
            };
        }
    };
    let hs_ms = now_ms().saturating_sub(t0);
    if c.expect != "ok" {
        return Err(format!("connected, but expected '{}'", c.expect));
    }
    let info = s.info().clone();
    if c.suite.is_some_and(|w| w != info.suite) {
        return Err(format!("negotiated {}", info.suite.name()));
    }
    if c.group.is_some_and(|g| g != info.group) {
        return Err(format!("negotiated group {}", info.group.name()));
    }
    let alpn = info.alpn.as_deref().map(|a| String::from_utf8_lossy(a).into_owned());
    if c.alpn.is_some() && c.alpn != alpn {
        return Err(format!("ALPN {alpn:?}"));
    }
    let path = c.get.as_ref().map(|g| g.0.as_str()).unwrap_or("/");
    s.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut page = Vec::new();
    let mut buf = vec![0u8; 16384];
    let t1 = now_ms();
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => page.extend_from_slice(&buf[..n]),
            // s_server ends without close_notify: fine once we have the page.
            Err(TlsError::Truncated) if !page.is_empty() => break,
            Err(e) => return Err(format!("read: {e}")),
        }
        if page.len() > 8 << 20 {
            return Err(String::from("page too big"));
        }
    }
    let read_ms = now_ms().saturating_sub(t1).max(1);
    let _ = s.close();
    let what = match &c.get {
        None => {
            let text = String::from_utf8_lossy(&page);
            if !text.contains("TLSv1.3") || !text.contains(&format!("Cipher is {}", info.suite.name())) {
                return Err(format!("unexpected status page ({} bytes)", page.len()));
            }
            format!("{} bytes of status page", page.len())
        }
        Some((_, size, sha)) => {
            let at = page
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .ok_or_else(|| String::from("no HTTP header"))?;
            let body = &page[at + 4..];
            if body.len() != *size || milkcrypto::sha256::sha256(body) != *sha {
                return Err(format!("body of {} bytes doesn't match", body.len()));
            }
            format!("{} bytes intact, {} KiB/s", body.len(), body.len() as u64 / read_ms * 1000 / 1024)
        }
    };
    Ok(format!(
        "{}{}, ALPN {}, {} tickets, {}, handshake {} ms",
        info.summary(),
        if info.hello_retry { " after HelloRetryRequest" } else { "" },
        alpn.as_deref().unwrap_or("none"),
        s.tickets,
        what,
        hs_ms
    ))
}
