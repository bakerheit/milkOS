//! User accounts and the desktop session's user: the kernel side of `login`.
//!
//! The account database is plain files, as on Unix:
//! * `/etc/passwd` `name:x:uid:gid:full name:home:shell` (world readable)
//! * `/etc/group`  `name:x:gid:member,member` (world readable)
//! * `/etc/shadow` `name:hash:` (root only, 0600). `hash` is
//!   `$5m$<rounds>$<salt hex>$<digest hex>`: h = SHA-256(salt ‖ password), then
//!   `rounds - 1` times h = SHA-256(h ‖ salt ‖ password). Never plain text. The same
//!   scheme is in user/rt (`rt::users`), which `su`, `sudo` and `passwd` use.
//!
//! **API for the desktop** (the lock screen can become a login screen with it):
//! * [`authenticate`]`(name, password) -> Result<Account, LoginError>`: check a password.
//! * [`login`]`(name, password)`: authenticate, then make that account the session user.
//!   Every process the desktop starts from then on (`proc::spawn`, `spawn_with_caps`,
//!   `spawn_on`: Terminal, apps) runs with its credentials, HOME and USER.
//! * [`session`]`() -> Account`: who is logged in (boot default: `milk`).
//! * [`lookup`] / [`lookup_uid`]: account by name / uid.
//!
//! Each try is audited (`login.ok` / `login.fail`); a failure costs 500 ms.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::cred::{self, Cred};
use crate::audit::sha256::Sha256;
use crate::fs;
use crate::sync::SpinLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    /// Primary group.
    pub gid: u32,
    /// Every group the account is in (the primary one first).
    pub groups: Vec<u32>,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

impl Account {
    pub fn cred(&self) -> Cred {
        Cred::user(self.uid, self.gid, &self.groups)
    }

    pub fn is_admin(&self) -> bool {
        self.uid == 0 || self.groups.contains(&cred::ADMIN_GID)
    }

    /// Environment for a process started for this account.
    pub fn env(&self) -> Vec<String> {
        alloc::vec![
            String::from("PATH=/bin"),
            alloc::format!("HOME={}", self.home),
            alloc::format!("USER={}", self.name),
            alloc::format!("LOGNAME={}", self.name),
            String::from("TERM=dumb"),
        ]
    }

    /// Built in, for a system with no /etc/passwd (or a broken one).
    fn fallback_milk() -> Account {
        Account {
            name: String::from("milk"),
            uid: cred::MILK_UID,
            gid: cred::STAFF_GID,
            groups: alloc::vec![cred::STAFF_GID, cred::ADMIN_GID],
            gecos: String::from("Milk"),
            home: String::from("/home/milk"),
            shell: String::from("/bin/msh"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginError {
    NoSuchUser,
    BadPassword,
    /// The account has no usable password hash (locked: `*` or `!`).
    Locked,
}

fn file(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn parse_passwd(line: &str) -> Option<Account> {
    let f: Vec<&str> = line.split(':').collect();
    if f.len() < 7 || line.starts_with('#') {
        return None;
    }
    Some(Account {
        name: f[0].to_string(),
        uid: f[2].parse().ok()?,
        gid: f[3].parse().ok()?,
        groups: Vec::new(),
        gecos: f[4].to_string(),
        home: f[5].to_string(),
        shell: f[6].to_string(),
    })
}

/// Groups `name` (primary group `gid`) is in, from /etc/group.
fn groups_of(name: &str, gid: u32) -> Vec<u32> {
    let mut out = alloc::vec![gid];
    for line in file("/etc/group").lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 4 {
            continue;
        }
        let Ok(g) = f[2].parse::<u32>() else { continue };
        if f[3].split(',').any(|m| m.trim() == name) && !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

fn find(pred: impl Fn(&Account) -> bool) -> Option<Account> {
    let mut a = file("/etc/passwd")
        .lines()
        .filter_map(parse_passwd)
        .find(|a| pred(a))?;
    a.groups = groups_of(&a.name, a.gid);
    Some(a)
}

pub fn lookup(name: &str) -> Option<Account> {
    find(|a| a.name == name)
}

pub fn lookup_uid(uid: u32) -> Option<Account> {
    find(|a| a.uid == uid)
}

/// Name of group `gid` from /etc/group.
pub fn group_name(gid: u32) -> Option<String> {
    file("/etc/group").lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 3 && f[2].parse() == Ok(gid)).then(|| f[0].to_string())
    })
}

// ---- password hashes -------------------------------------------------------------------

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The `$5m$` digest of `password` with `salt` and `rounds`.
pub fn hash_password(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut h = Sha256::new().update(salt).update(password).finish();
    for _ in 1..rounds.max(1) {
        h = Sha256::new()
            .update(&h)
            .update(salt)
            .update(password)
            .finish();
    }
    h
}

/// Does `password` match the `$5m$...` string `stored`? Constant time in the digest.
pub fn check_hash(stored: &str, password: &str) -> bool {
    let f: Vec<&str> = stored.split('$').collect();
    // "", "5m", rounds, salt, digest
    if f.len() != 5 || f[1] != "5m" {
        return false;
    }
    let (Ok(rounds), Some(salt), Some(want)) =
        (f[2].parse::<u32>(), hex_decode(f[3]), hex_decode(f[4]))
    else {
        return false;
    };
    if want.len() != 32 || !(1..=1_000_000).contains(&rounds) {
        return false;
    }
    let got = hash_password(password.as_bytes(), &salt, rounds);
    got.iter().zip(&want).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

fn shadow_hash(name: &str) -> Option<String> {
    // /etc/shadow is 0600 root: read it as the kernel, whoever asked (an admin ticket
    // request comes from an ordinary user's process).
    crate::fs::perm::as_kernel(|| file("/etc/shadow"))
        .lines()
        .find_map(|l| {
            let mut f = l.split(':');
            (f.next()? == name).then(|| f.next().unwrap_or("").to_string())
        })
}

/// Check `password` for `name`. Kernel context reads /etc/shadow as root.
pub fn authenticate(name: &str, password: &str) -> Result<Account, LoginError> {
    let r = (|| {
        let acct = lookup(name).ok_or(LoginError::NoSuchUser)?;
        let h = shadow_hash(name).ok_or(LoginError::Locked)?;
        if !h.starts_with("$5m$") {
            return Err(LoginError::Locked);
        }
        if !check_hash(&h, password) {
            return Err(LoginError::BadPassword);
        }
        Ok(acct)
    })();
    match &r {
        Ok(_) => crate::audit!("login.ok", "user={name}"),
        Err(e) => {
            crate::audit!("login.fail", "user={name} reason={e:?}");
            crate::sched::sleep_ms(500);
        }
    }
    r
}

// ---- the session user ------------------------------------------------------------------

static SESSION: SpinLock<Option<Account>> = SpinLock::new(None);

/// The logged-in desktop user.
pub fn session() -> Account {
    if let Some(a) = SESSION.lock().clone() {
        return a;
    }
    let a = lookup("milk").unwrap_or_else(Account::fallback_milk);
    *SESSION.lock() = Some(a.clone());
    a
}

/// Make `acct` the desktop session's user (no password check: see [`login`]).
pub fn set_session(acct: Account) {
    crate::audit!("login.session", "user={} uid={}", acct.name, acct.uid);
    *SESSION.lock() = Some(acct);
}

/// Authenticate and switch the session user. Processes already running keep their ids.
/// Then try the login password on any locked (encrypted) milkfs volume, so a disk whose
/// passphrase is the login password opens at login, FileVault style (docs/security.md).
pub fn login(name: &str, password: &str) -> Result<Account, LoginError> {
    let a = authenticate(name, password)?;
    set_session(a.clone());
    match fs::milkfs::unlock(password, None) {
        Ok(paths) => {
            crate::kprintln!("[login] {}: unlocked {}", a.name, paths.join(" "));
            fix_layout(&a); // the primary volume brings its own home and /Applications
        }
        Err(crate::fs::Errno::ENODEV) => {} // nothing locked
        Err(e) => crate::kprintln!(
            "[login] {}: no disk opened with that password ({e})",
            a.name
        ),
    }
    Ok(a)
}

/// Credentials for processes the desktop starts.
pub fn session_cred() -> Cred {
    session().cred()
}

// ---- boot ------------------------------------------------------------------------------

/// Where `passwd` keeps a copy of /etc/shadow that survives reboots (on the milkfs disk).
pub const PERSISTENT_SHADOW: &str = "/Volumes/Milk/.milkos/shadow";

/// Boot: restore a persisted /etc/shadow, fix the ownership and modes the account and
/// permission model depends on, resolve the session user. After the file systems are up.
pub fn init() {
    if let Ok(s) = fs::read_file(PERSISTENT_SHADOW)
        && !s.is_empty()
    {
        let _ = fs::write_file("/etc/shadow", &s);
    }
    chmod_own("/etc/shadow", 0o600, 0, 0);
    chmod_own("/etc/passwd", 0o644, 0, 0);
    chmod_own("/etc/group", 0o644, 0, 0);
    let a = session();
    fix_layout(&a);
    crate::kprintln!(
        "[login] session user {} (uid {}, gid {}, groups {:?}), home {}",
        a.name,
        a.uid,
        a.gid,
        a.groups,
        a.home
    );
}

fn chmod_own(path: &str, perm: u32, uid: u32, gid: u32) {
    let _ = fs::setattr_at(
        None,
        path,
        true,
        &fs::node::SetAttr {
            perm: Some(perm),
            uid: Some(uid),
            gid: Some(gid),
            ..Default::default()
        },
    );
}

/// The layout the permission model expects. At boot, and again after a login unlocked
/// the primary volume (which binds its own /home/milk and /Applications).
fn fix_layout(a: &Account) {
    // docs/security.md: /root 0700 root; /tmp 1777; /bin and /Applications root:admin
    // 0775 (admins install packages without sudo, like macOS), /etc/milk/repos root:admin
    // 0664; a milkfs volume root root:admin 0775; the session user's home owned by them (a one-time migration for disks made before
    //  users existed, where everything belongs to root).
    let _ = fs::mkdir("/root", 0o700);
    chmod_own("/root", 0o700, 0, 0);
    chmod_own("/tmp", 0o1777, 0, 0);
    // Service sockets (AF_UNIX, tmpfs): root's, readable by all.
    let _ = fs::mkdir("/run", 0o755);
    chmod_own("/bin", 0o775, 0, cred::ADMIN_GID);
    chmod_own("/Applications", 0o775, 0, cred::ADMIN_GID);
    // Package sources are an admin setting (the Store and `milk repo` edit it); the
    // trusted signing keys in /etc/milk/keys stay root's.
    chmod_own("/etc/milk/repos", 0o664, 0, cred::ADMIN_GID);
    if let Ok(st) = fs::stat("/Volumes/Milk")
        && st.uid == 0
        && st.gid == 0
    {
        chmod_own(
            "/Volumes/Milk",
            (st.perm | 0o775) & 0o7777,
            0,
            cred::ADMIN_GID,
        );
    }
    if let Ok(st) = fs::stat(&a.home)
        && st.uid != a.uid
    {
        let n = fs::perm::chown_tree(&a.home, a.uid, a.gid);
        crate::kprintln!("[login] {}: gave {n} files to {}", a.home, a.name);
    }
}

/// Boot self-test of the hash scheme against a fixed vector (scripts/mkpasswd.py's
/// hash_pw(b"milk", bytes(range(16)), 5000)), and a look at the account files. It never
/// depends on the actual passwords: those change (`passwd`, restored at boot).
pub fn selftest() {
    let salt: [u8; 16] = core::array::from_fn(|i| i as u8);
    let want = "6558b3e8bb6ca30bb763d6c23f0e992c80b74f4ca0f21300e8d9e9bc25e1c89d";
    let got: String = hash_password(b"milk", &salt, 5000)
        .iter()
        .map(|b| alloc::format!("{b:02x}"))
        .collect();
    assert!(got == want, "login: $5m$ hash mismatch: {got}");
    let stored = alloc::format!("$5m$5000$000102030405060708090a0b0c0d0e0f${want}");
    assert!(check_hash(&stored, "milk") && !check_hash(&stored, "milk "));
    // The accounts are configuration: report, don't panic.
    match lookup("milk") {
        Some(m) if m.uid == cred::MILK_UID && m.is_admin() => {}
        other => crate::kprintln!("[login] warning: unexpected milk account: {other:?}"),
    }
    if file("/etc/shadow").lines().any(|l| {
        let h = l.split(':').nth(1).unwrap_or("");
        !h.is_empty() && !h.starts_with("$5m$") && !h.starts_with('*') && !h.starts_with('!')
    }) {
        crate::kprintln!("[login] warning: /etc/shadow has an entry that is not a $5m$ hash");
    }
}
