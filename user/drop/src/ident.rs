//! This machine's Milk Drop identity (an Ed25519 key pair and a name) and the peers it
//! trusts (TOFU), all in /etc/milk/drop (persistent: the kernel binds it onto the Milk
//! volume, see `milkfs::setup_persistent_etc`).

use alloc::string::String;
use alloc::vec::Vec;
use rt::sha512::Sha512;
use rt::{ed25519, sys};

pub const DIR: &str = "/etc/milk/drop";
const SECRET: &str = "/etc/milk/drop/identity.secret";
const PUBLIC: &str = "/etc/milk/drop/identity.pub";
const NAME: &str = "/etc/milk/drop/name";
const KNOWN: &str = "/etc/milk/drop/known_peers";

pub struct Identity {
    pub seed: [u8; 32],
    pub public: [u8; 32],
    pub name: String,
}

pub fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

/// The fingerprint people compare: the first 8 bytes of SHA-256(public key), as four
/// groups ("3fa2 91bc 07de 44a1").
pub fn fingerprint(pk: &[u8; 32]) -> String {
    let h = hex(&rt::sha256::sha256(pk)[..8]);
    let mut s = String::new();
    for (i, c) in h.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            s.push(' ');
        }
        s.push(c);
    }
    s
}

/// The same 16 hex digits without spaces (what announcements carry).
pub fn fp_compact(pk: &[u8; 32]) -> String {
    hex(&rt::sha256::sha256(pk)[..8])
}

/// A name peers may use: 1-32 of `[A-Za-z0-9._-]`.
pub fn name_ok(n: &str) -> bool {
    (1..=32).contains(&n.len())
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn text(p: &str) -> Option<String> {
    sys::read_file(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Write `data` to `path` via a temporary file and a rename (never half a key file).
pub fn write_atomic(path: &str, data: &[u8], mode: u32) -> Result<(), String> {
    let tmp = alloc::format!("{path}.new");
    let fd = sys::open_mode(
        &tmp,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        mode,
    )
    .map_err(|e| alloc::format!("{tmp}: {e}"))?;
    let r = sys::write_all(fd, data);
    let _ = sys::close(fd);
    r.map_err(|e| alloc::format!("{tmp}: {e}"))?;
    sys::rename(&tmp, path).map_err(|e| alloc::format!("{path}: {e}"))
}

/// Random bytes for keys and nonces: /dev/urandom (a xorshift seeded from the boot
/// counter: weak on its own) stirred with clock jitter, the wall clock, uname and the
/// machine's MAC addresses through SHA-512. Fine for a hobby OS demo; not a CSPRNG.
pub fn random<const N: usize>() -> [u8; N] {
    let mut h = Sha512::new();
    let mut u = [0u8; 64];
    if let Ok(fd) = sys::open("/dev/urandom", sys::O_RDONLY | sys::O_CLOEXEC) {
        let _ = sys::read(fd, &mut u);
        let _ = sys::close(fd);
    }
    h.update(&u);
    h.update(&sys::clock_ns(sys::CLOCK_REALTIME).to_le_bytes());
    for _ in 0..64 {
        let a = sys::clock_ns(sys::CLOCK_MONOTONIC);
        let _ = sys::getpid();
        let b = sys::clock_ns(sys::CLOCK_MONOTONIC);
        h.update(&(b.wrapping_sub(a) ^ b).to_le_bytes());
    }
    if let Some(t) = text("/proc/net/ifaddr") {
        h.update(t.as_bytes());
    }
    if let Ok(u) = sys::uname() {
        for f in &u {
            h.update(f.as_bytes());
        }
    }
    h.update(&sys::getpid().to_le_bytes());
    let d = h.finish();
    let mut out = [0u8; N];
    let n = N.min(d.len());
    out[..n].copy_from_slice(&d[..n]);
    out
}

fn hostname() -> String {
    sys::uname()
        .map(|u| u[1].clone())
        .ok()
        .filter(|n| name_ok(n))
        .unwrap_or_else(|| String::from("milk"))
}

/// The identity, if there is one (never creates it).
pub fn load() -> Option<Identity> {
    let t = text(SECRET)?;
    let seed = t.split_whitespace().nth(1).and_then(ed25519::unhex::<32>)?;
    let public = ed25519::public_key(&seed);
    let name = text(NAME)
        .map(|n| String::from(n.trim()))
        .filter(|n| name_ok(n))
        .unwrap_or_else(|| default_name(&public));
    Some(Identity { seed, public, name })
}

/// Load the identity, creating it on first use (true = just created).
pub fn load_or_create() -> Result<(Identity, bool), String> {
    if let Some(t) = text(SECRET) {
        let seed = t
            .split_whitespace()
            .nth(1)
            .and_then(ed25519::unhex::<32>)
            .ok_or_else(|| alloc::format!("{SECRET} is damaged (move it away for a new one)"))?;
        let public = ed25519::public_key(&seed);
        let name = text(NAME)
            .map(|n| String::from(n.trim()))
            .filter(|n| name_ok(n))
            .unwrap_or_else(|| default_name(&public));
        return Ok((Identity { seed, public, name }, false));
    }
    let _ = sys::mkdir(DIR, 0o700);
    let seed = random::<32>();
    let public = ed25519::public_key(&seed);
    // A new key keeps the machine's name if it has one (peers then see a changed key).
    let name = text(NAME)
        .map(|n| String::from(n.trim()))
        .filter(|n| name_ok(n))
        .unwrap_or_else(|| default_name(&public));
    write_atomic(
        SECRET,
        alloc::format!(
            "ed25519-seed {}\n# Milk Drop identity of this machine: never copy it anywhere.\n",
            hex(&seed)
        )
        .as_bytes(),
        0o600,
    )?;
    let id = Identity { seed, public, name };
    save_public(&id)?;
    Ok((id, true))
}

fn default_name(pk: &[u8; 32]) -> String {
    alloc::format!("{}-{}", hostname(), &fp_compact(pk)[..4])
}

fn save_public(id: &Identity) -> Result<(), String> {
    write_atomic(NAME, alloc::format!("{}\n", id.name).as_bytes(), 0o644)?;
    write_atomic(
        PUBLIC,
        alloc::format!("ed25519 {} {}\n", id.name, hex(&id.public)).as_bytes(),
        0o644,
    )
}

pub fn set_name(id: &mut Identity, name: &str) -> Result<(), String> {
    if !name_ok(name) {
        return Err(alloc::format!(
            "bad name {name:?}: 1-32 letters, digits, '.', '_' or '-'"
        ));
    }
    id.name = String::from(name);
    save_public(id)
}

// ---- trust on first use ----------------------------------------------------------------

#[derive(Clone)]
pub struct Known {
    pub name: String,
    pub key: [u8; 32],
    pub since: u64,
}

pub fn known() -> Vec<Known> {
    text(KNOWN)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some(Known {
                name: String::from(w.next()?),
                key: ed25519::unhex::<32>(w.next()?)?,
                since: w.next().and_then(|t| t.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

fn save_known(k: &[Known]) -> Result<(), String> {
    let mut s = String::from("# Milk Drop peers trusted on first use: name key first-seen\n");
    for e in k {
        s.push_str(&alloc::format!("{} {} {}\n", e.name, hex(&e.key), e.since));
    }
    write_atomic(KNOWN, s.as_bytes(), 0o644)
}

pub enum Trust {
    /// This key was trusted before (under `as_name`, maybe a different name).
    Known { as_name: String },
    /// Never seen: trust on first use.
    New,
    /// The name is known with a different key: refuse.
    Changed { old: [u8; 32] },
}

pub fn trust(name: &str, key: &[u8; 32]) -> Trust {
    let k = known();
    if let Some(e) = k.iter().find(|e| &e.key == key) {
        return Trust::Known {
            as_name: e.name.clone(),
        };
    }
    if let Some(e) = k.iter().find(|e| e.name == name) {
        return Trust::Changed { old: e.key };
    }
    Trust::New
}

/// Trust `key` as `name` from now on (a known key that changed its name is renamed).
pub fn remember(name: &str, key: &[u8; 32]) -> Result<(), String> {
    let mut k = known();
    if let Some(e) = k.iter_mut().find(|e| &e.key == key) {
        if e.name == name {
            return Ok(());
        }
        e.name = String::from(name);
    } else {
        k.push(Known {
            name: String::from(name),
            key: *key,
            since: sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000,
        });
    }
    save_known(&k)
}

/// Stop trusting `name` (after a legitimate key change). False if it wasn't known.
pub fn forget(name: &str) -> Result<bool, String> {
    let mut k = known();
    let n = k.len();
    k.retain(|e| e.name != name);
    if k.len() == n {
        return Ok(false);
    }
    save_known(&k).map(|_| true)
}
