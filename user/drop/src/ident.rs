//! This machine's Milk Drop identity (an Ed25519 key pair and a name) and the peers it
//! trusts (TOFU), all in /etc/milk/drop (persistent: the kernel binds it onto the Milk
//! volume, see `milkfs::setup_persistent_etc`).

use alloc::string::String;
use alloc::vec::Vec;
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

/// Random bytes for keys and nonces: the kernel CSPRNG (ChaCha20, seeded from virtio-rng;
/// docs/crypto.md) through /dev/urandom.
pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let fd = sys::open("/dev/urandom", sys::O_RDONLY | sys::O_CLOEXEC).expect("drop: /dev/urandom");
    let mut got = 0;
    while got < N {
        match sys::read(fd, &mut out[got..]) {
            Ok(n) if n > 0 => got += n,
            _ => panic!("drop: reading /dev/urandom failed"),
        }
    }
    let _ = sys::close(fd);
    out
}

/// Which RNG made the key in identity.secret: `rng=kernel-csprng` from this release on;
/// no tag = the old weak generator (rotate it: `drop id --rotate`).
pub const RNG_TAG: &str = "rng=kernel-csprng";

/// Was the current key made by the old, weak RNG (no `rng=` tag in identity.secret)?
pub fn legacy_key() -> bool {
    text(SECRET).is_some_and(|t| !t.contains(RNG_TAG))
}

fn write_secret(seed: &[u8; 32]) -> Result<(), String> {
    write_atomic(
        SECRET,
        alloc::format!(
            "ed25519-seed {} {RNG_TAG} made={}\n# Milk Drop identity of this machine: never copy it anywhere.\n",
            hex(seed),
            sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000
        )
        .as_bytes(),
        0o600,
    )
}

const ROTATION: &str = "/etc/milk/drop/rotation";

/// A rotation statement: the previous key and its signature over "old -> new".
#[derive(Clone)]
pub struct Rotation {
    pub old: [u8; 32],
    pub sig: [u8; 64],
}

/// The statement for the current key, if it replaced an older one.
pub fn rotation(current: &[u8; 32]) -> Option<Rotation> {
    let t = text(ROTATION)?;
    let mut w = t.split_whitespace();
    if w.next()? != "ed25519-rotate" {
        return None;
    }
    let old = ed25519::unhex::<32>(w.next()?)?;
    let new = ed25519::unhex::<32>(w.next()?)?;
    let sig = ed25519::unhex::<64>(w.next()?)?;
    (&new == current).then_some(Rotation { old, sig })
}

/// Replace this machine's key with a fresh one from the kernel CSPRNG. The old key signs
/// the new one (so peers that trusted it move over without a "key changed" refusal) and
/// is then wiped. Returns (old public key, new identity).
pub fn rotate(id: &Identity) -> Result<([u8; 32], Identity), String> {
    let mut seed = random::<32>();
    let public = ed25519::public_key(&seed);
    let sig = ed25519::sign(
        &id.seed,
        crate::proto::rotation_msg(&id.public, &public).as_bytes(),
    );
    write_atomic(
        ROTATION,
        alloc::format!(
            "ed25519-rotate {} {} {}\n# The old key above signed the new one (drop id --rotate).\n",
            hex(&id.public),
            hex(&public),
            hex(&sig)
        )
        .as_bytes(),
        0o644,
    )?;
    write_secret(&seed)?;
    let new = Identity {
        seed,
        public,
        name: id.name.clone(),
    };
    save_public(&new)?;
    rt::crypto::wipe(&mut seed);
    Ok((id.public, new))
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
    write_secret(&seed)?;
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
    /// The name's trusted key signed this new key (`drop id --rotate` on the peer).
    Rotated { old: [u8; 32] },
    /// This key was trusted before (under `as_name`, maybe a different name).
    Known { as_name: String },
    /// Never seen: trust on first use.
    New,
    /// The name is known with a different key: refuse.
    Changed { old: [u8; 32] },
}

/// Trust, taking a rotation statement into account: a changed key is accepted when the
/// key we trusted signed it.
pub fn trust_rotated(name: &str, key: &[u8; 32], rot: Option<&Rotation>) -> Trust {
    match trust(name, key) {
        Trust::Changed { old }
            if rot.is_some_and(|r| {
                r.old == old
                    && ed25519::verify(
                        &old,
                        crate::proto::rotation_msg(&old, key).as_bytes(),
                        &r.sig,
                    )
            }) =>
        {
            Trust::Rotated { old }
        }
        t => t,
    }
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

/// Trust `key` as `name` from now on (a known key that changed its name is renamed; a
/// rotated key replaces the name's old one).
pub fn remember(name: &str, key: &[u8; 32]) -> Result<(), String> {
    let mut k = known();
    k.retain(|e| e.name != name || &e.key == key);
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
