//! Accounts for user programs: /System/Settings/passwd, /System/Settings/group, /System/Settings/shadow and the `$5m$`
//! password hash (the same scheme as the kernel's kernel/src/proc/login.rs):
//! h = SHA-256(salt ‖ password), then `rounds - 1` times h = SHA-256(h ‖ salt ‖ password),
//! stored as `$5m$<rounds>$<salt hex>$<digest hex>`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::sha256::Sha256;
use crate::sys;

pub const ROUNDS: u32 = 5000;
pub const ADMIN_GID: u32 = 80;
pub const SHADOW: &str = "/System/Settings/shadow";
/// passwd also writes here so a changed password survives a reboot (the kernel restores it).
pub const PERSISTENT_SHADOW: &str = "/Volumes/Milk/.milkos/shadow";

#[derive(Clone, Debug)]
pub struct Passwd {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn parse(line: &str) -> Option<Passwd> {
    let f: Vec<&str> = line.split(':').collect();
    if f.len() < 7 || line.starts_with('#') {
        return None;
    }
    Some(Passwd {
        name: f[0].to_string(),
        uid: f[2].parse().ok()?,
        gid: f[3].parse().ok()?,
        gecos: f[4].to_string(),
        home: f[5].to_string(),
        shell: f[6].to_string(),
    })
}

pub fn all() -> Vec<Passwd> {
    text("/System/Settings/passwd")
        .lines()
        .filter_map(parse)
        .collect()
}

pub fn by_name(name: &str) -> Option<Passwd> {
    all().into_iter().find(|p| p.name == name)
}

pub fn by_uid(uid: u32) -> Option<Passwd> {
    all().into_iter().find(|p| p.uid == uid)
}

/// User name for `uid`, or the number.
pub fn user_name(uid: u32) -> String {
    by_uid(uid).map_or_else(|| alloc::format!("{uid}"), |p| p.name)
}

/// (name, gid, members) for every group.
pub fn groups() -> Vec<(String, u32, Vec<String>)> {
    text("/System/Settings/group")
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 4 {
                return None;
            }
            let members = f[3]
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(String::from)
                .collect();
            Some((f[0].to_string(), f[2].parse().ok()?, members))
        })
        .collect()
}

pub fn group_name(gid: u32) -> String {
    groups()
        .into_iter()
        .find(|g| g.1 == gid)
        .map_or_else(|| alloc::format!("{gid}"), |g| g.0)
}

pub fn group_id(name: &str) -> Option<u32> {
    groups().into_iter().find(|g| g.0 == name).map(|g| g.1)
}

/// The primary group first, then every group listing `user` as a member.
pub fn groups_of(user: &str, gid: u32) -> Vec<u32> {
    let mut out = alloc::vec![gid];
    for (_, g, members) in groups() {
        if members.iter().any(|m| m == user) && !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

// ---- hashes ----------------------------------------------------------------------------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| alloc::format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

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

/// A fresh `$5m$` string for `password` (random 16-byte salt).
pub fn make_hash(password: &str) -> String {
    let mut salt = [0u8; 16];
    let _ = sys::getrandom(&mut salt);
    let h = hash_password(password.as_bytes(), &salt, ROUNDS);
    alloc::format!("$5m${ROUNDS}${}${}", hex(&salt), hex(&h))
}

/// Does `password` match `stored`? Constant time in the digest compare.
pub fn check_hash(stored: &str, password: &str) -> bool {
    let f: Vec<&str> = stored.split('$').collect();
    if f.len() != 5 || f[1] != "5m" {
        return false;
    }
    let (Ok(rounds), Some(salt), Some(want)) = (f[2].parse::<u32>(), unhex(f[3]), unhex(f[4]))
    else {
        return false;
    };
    if want.len() != 32 || !(1..=1_000_000).contains(&rounds) {
        return false;
    }
    let got = hash_password(password.as_bytes(), &salt, rounds);
    got.iter().zip(&want).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// `user`'s hash from /System/Settings/shadow (needs root to read).
pub fn shadow_hash(user: &str) -> Option<String> {
    text(SHADOW).lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == user).then(|| f.next().unwrap_or("").to_string())
    })
}

/// Check `user`'s password (as root, e.g. from a set-user-ID program).
pub fn verify(user: &str, password: &str) -> bool {
    shadow_hash(user).is_some_and(|h| check_hash(&h, password))
}

fn write_atomic(path: &str, data: &[u8], mode: u32) -> sys::Result<()> {
    let tmp = alloc::format!("{path}.new");
    let fd = sys::open_mode(
        &tmp,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        mode,
    )?;
    let r = sys::write_all(fd, data);
    let _ = sys::close(fd);
    r?;
    let _ = sys::chmod(&tmp, mode);
    sys::rename(&tmp, path)
}

/// Replace `user`'s hash in /System/Settings/shadow (and its persistent copy, if the disk is there).
pub fn set_shadow_hash(user: &str, hash: &str) -> sys::Result<()> {
    let mut found = false;
    let mut out = String::new();
    for l in text(SHADOW).lines() {
        let mut f: Vec<&str> = l.split(':').collect();
        if f.first() == Some(&user) {
            found = true;
            if f.len() < 2 {
                f.resize(2, "");
            }
            f[1] = hash;
            out.push_str(&f.join(":"));
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !found {
        out.push_str(&alloc::format!("{user}:{hash}:\n"));
    }
    write_atomic(SHADOW, out.as_bytes(), 0o600)?;
    if sys::stat_path("/Volumes/Milk", true).is_ok() {
        let _ = sys::mkdir("/Volumes/Milk/.milkos", 0o700);
        let _ = write_atomic(PERSISTENT_SHADOW, out.as_bytes(), 0o600);
    }
    Ok(())
}

// ---- terminal + identity -------------------------------------------------------------

/// Prompt on stderr and read one line from stdin with echo off (if stdin is a terminal).
pub fn read_password(prompt: &str) -> Option<String> {
    let _ = sys::write_all(2, prompt.as_bytes());
    let echo = sys::set_echo(0, false);
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    let ok = loop {
        match sys::read(0, &mut b) {
            Ok(0) => break !line.is_empty(),
            Ok(_) if b[0] == b'\n' || b[0] == b'\r' => break true,
            Ok(_) => line.push(b[0]),
            Err(_) => break false,
        }
    };
    if echo == Some(true) {
        let _ = sys::set_echo(0, true);
    }
    if echo.is_some() {
        let _ = sys::write_all(2, b"\n");
    }
    ok.then(|| String::from_utf8_lossy(&line).into_owned())
}

/// Become `p` for good (real, effective and saved ids, groups). Needs root.
pub fn become_user(p: &Passwd) -> sys::Result<()> {
    sys::setgroups(&groups_of(&p.name, p.gid))?;
    sys::setresgid(p.gid, p.gid, p.gid)?;
    sys::setresuid(p.uid, p.uid, p.uid)
}

/// Environment for a login shell of `p`.
pub fn login_env(p: &Passwd) -> Vec<String> {
    alloc::vec![
        String::from("PATH=/Commands"),
        alloc::format!("HOME={}", p.home),
        alloc::format!("USER={}", p.name),
        alloc::format!("LOGNAME={}", p.name),
        String::from("TERM=dumb"),
    ]
}
