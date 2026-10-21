//! `/sys/fs/milkfs/info` and the list of locked volumes.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    pub label: String,
    pub device: String,
    pub generation: u64,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub snapshots: u32,
    pub commits: u64,
    /// "ok", or "BROKEN (read-only)".
    pub state: String,
    /// Whether the volume is encrypted, if the system says (an `encryption:` or `encrypted:` line
    /// in `info`); `None` when it does not.
    pub encrypted: Option<bool>,
    /// "xchacha20-siv (per block; tag HMAC-SHA256/64)" and friends, when encrypted.
    pub cipher: String,
    pub key_wrap: String,
    pub kdf: String,
    /// "1 passphrase, 1 recovery".
    pub key_slots: String,
}

pub const BLOCK_KB: u64 = 4;

impl VolumeInfo {
    pub fn total_kb(&self) -> u64 {
        self.total_blocks * BLOCK_KB
    }

    pub fn free_kb(&self) -> u64 {
        self.free_blocks * BLOCK_KB
    }

    pub fn used_kb(&self) -> u64 {
        self.total_blocks.saturating_sub(self.free_blocks) * BLOCK_KB
    }

    /// Used space in tenths of a percent.
    pub fn used_pct10(&self) -> u32 {
        (self.total_blocks.saturating_sub(self.free_blocks) * 1000)
            .checked_div(self.total_blocks)
            .unwrap_or(0) as u32
    }

    pub fn healthy(&self) -> bool {
        self.state == "ok"
    }
}

/// Parse `info`. `None` when there is no volume ("no milkfs volume mounted").
pub fn parse_info(text: &str) -> Option<VolumeInfo> {
    let mut v = VolumeInfo::default();
    let mut any = false;
    for l in text.lines() {
        let Some((k, val)) = l.split_once(':') else {
            continue;
        };
        let val = val.trim();
        match k {
            "label" => {
                v.label = val.to_string();
                any = true;
            }
            "device" => v.device = val.to_string(),
            "generation" => v.generation = val.parse().unwrap_or(0),
            "blocks" => {
                let mut it = val.split_whitespace();
                v.total_blocks = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                it.next();
                v.free_blocks = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            }
            "snapshots" => v.snapshots = val.parse().unwrap_or(0),
            "commits" => {
                v.commits = val
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            }
            "state" => v.state = val.to_string(),
            "cipher" => v.cipher = val.to_string(),
            "key wrap" => v.key_wrap = val.to_string(),
            "kdf" => v.kdf = val.to_string(),
            "key slots" => v.key_slots = val.to_string(),
            "encryption" | "encrypted" => {
                v.encrypted = match val.split_whitespace().next() {
                    Some("on" | "yes" | "true" | "encrypted") => Some(true),
                    Some("off" | "no" | "false" | "none") => Some(false),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    any.then_some(v)
}

/// A volume that is encrypted and not yet unlocked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locked {
    pub device: String,
    pub label: String,
}

/// Parse `/sys/fs/milkfs/unlock` ("vda Milk locked" per line).
pub fn parse_locked(text: &str) -> Vec<Locked> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (d, label, state) = (it.next()?, it.next()?, it.next()?);
            (state == "locked").then(|| Locked {
                device: d.to_string(),
                label: label.to_string(),
            })
        })
        .collect()
}

/// One line of `/sys/fs/milkfs/lock`: an encrypted volume and its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    pub device: String,
    pub label: String,
    pub locked: bool,
    /// Where it is mounted, when unlocked.
    pub at: Option<String>,
    /// The volume the session lives on: it can not be locked while logged in.
    pub home: bool,
}

/// Parse `/sys/fs/milkfs/lock` ("vdc Milk unlocked at=/Volumes/Milk (home: can't be locked)", "vdd Old locked").
pub fn parse_lock(text: &str) -> Vec<LockEntry> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (device, label, state) = (it.next()?, it.next()?, it.next()?);
            let locked = match state {
                "locked" => true,
                "unlocked" => false,
                _ => return None,
            };
            let at = l
                .split_whitespace()
                .find_map(|w| w.strip_prefix("at="))
                .map(|a| a.to_string());
            Some(LockEntry {
                device: device.to_string(),
                label: label.to_string(),
                locked,
                at,
                home: l.contains("(home:"),
            })
        })
        .collect()
}

/// What a failed control write says: `/sys/fs/milkfs/error` is one line `CONTROL: ERRNO: reason`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlError {
    pub control: String,
    pub errno_name: String,
    pub reason: String,
}

pub fn parse_error(text: &str) -> Option<ControlError> {
    let line = text.lines().next()?.trim();
    let (control, rest) = line.split_once(": ")?;
    let (errno_name, reason) = rest.split_once(": ")?;
    if control.contains(' ') || reason.is_empty() {
        return None;
    }
    Some(ControlError {
        control: control.to_string(),
        errno_name: errno_name.to_string(),
        reason: reason.to_string(),
    })
}

/// Who is using a volume, from a lock refusal's reason:
/// "vdb (Vault) is in use: pid 16 (sleep): 1 open file; pid 10 (msh): working directory".
pub fn holders(reason: &str) -> Vec<String> {
    match reason.split_once(" is in use: ") {
        Some((_, list)) => list
            .split("; ")
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// The text to write to `lock`: the device name.
pub fn lock_request(device: &str) -> String {
    device.to_string()
}

/// The text to write to `unlock` for a passphrase or recovery key (a device name picks which one).
pub fn unlock_request(device: Option<&str>, secret: &str) -> String {
    match device {
        Some(d) => alloc::format!("dev={d} {secret}"),
        None => secret.to_string(),
    }
}

/// The text to write to `passwd`: the current secret, a newline, the new passphrase.
pub fn passwd_request(old: &str, new: &str) -> String {
    alloc::format!("{old}\n{new}")
}

/// Why a passphrase can not be used, if so.
pub fn check_passphrase(new: &str, again: &str) -> Result<(), &'static str> {
    if new.chars().count() < 8 {
        return Err("Use at least 8 characters");
    }
    if new.contains('\n') || new.contains('\r') {
        return Err("A passphrase is one line");
    }
    if new != again {
        return Err("The two passphrases differ");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "label:       Milk\ndevice:      vda\ngeneration:  241\nblocks:      16384 total, 15847 free (4 KiB)\nsnapshots:   2\nnext inode:  262\ncommits:     185 this boot\nopen txn:    0 op(s)\nstate:       ok\nbcache:      512/512 buffers, 0 dirty, 573 hits, 938 misses, 927 writebacks\n";

    #[test]
    fn info() {
        let v = parse_info(INFO).unwrap();
        assert_eq!(
            (v.label.as_str(), v.device.as_str(), v.generation),
            ("Milk", "vda", 241)
        );
        assert_eq!(
            (v.total_blocks, v.free_blocks, v.snapshots, v.commits),
            (16384, 15847, 2, 185)
        );
        assert_eq!(
            (v.total_kb(), v.free_kb(), v.used_kb()),
            (65536, 63388, 2148)
        );
        assert_eq!(v.used_pct10(), 32);
        assert!(v.healthy());
        assert_eq!(v.encrypted, None);
        let enc = parse_info(&alloc::format!("{INFO}encryption:  on\n")).unwrap();
        assert_eq!(enc.encrypted, Some(true));
        assert!(parse_info("no milkfs volume mounted\n").is_none());
        let mut b = v.clone();
        b.state = "BROKEN (read-only)".into();
        assert!(!b.healthy());
        assert_eq!(VolumeInfo::default().used_pct10(), 0);
    }

    #[test]
    fn encryption_details() {
        let t = alloc::format!(
            "{INFO}encryption:  on\nlocked:      no\ncipher:      xchacha20-siv (per block; tag HMAC-SHA256/64)\nkey wrap:    xchacha20-poly1305\nkdf:         scrypt N=2^16 r=8 p=4\nkey slots:   1 passphrase, 1 recovery\n"
        );
        let v = parse_info(&t).unwrap();
        assert_eq!(v.encrypted, Some(true));
        assert_eq!(v.kdf, "scrypt N=2^16 r=8 p=4");
        assert_eq!(v.key_slots, "1 passphrase, 1 recovery");
        assert!(v.cipher.starts_with("xchacha20-siv"));
        let off = parse_info(&alloc::format!("{INFO}encryption:  off\n")).unwrap();
        assert_eq!((off.encrypted, off.cipher.as_str()), (Some(false), ""));
    }

    #[test]
    fn lock_file_and_errors() {
        let l = parse_lock(
            "vdc Milk unlocked at=/Volumes/Milk (home: can't be locked)\nvdb Vault unlocked at=/Volumes/Vault\nvdd Old locked\njunk\n",
        );
        assert_eq!(l.len(), 3);
        assert!(l[0].home && !l[0].locked);
        assert_eq!(
            (l[1].at.as_deref(), l[1].home),
            (Some("/Volumes/Vault"), false)
        );
        assert!(l[2].locked && l[2].at.is_none());
        let e = parse_error("lock: EBUSY: vdb (Vault) is in use: pid 16 (sleep): 1 open file; pid 10 (msh): working directory\n").unwrap();
        assert_eq!(
            (e.control.as_str(), e.errno_name.as_str()),
            ("lock", "EBUSY")
        );
        assert_eq!(
            holders(&e.reason),
            [
                "pid 16 (sleep): 1 open file",
                "pid 10 (msh): working directory"
            ]
        );
        assert!(holders("no such thing").is_empty());
        assert_eq!(parse_error(""), None);
        assert_eq!(parse_error("garbage"), None);
        assert_eq!(lock_request("vdb"), "vdb");
    }

    #[test]
    fn locked() {
        let l = parse_locked("vda Milk locked\nvdb Work locked\njunk\nvdc Open unlocked\n");
        assert_eq!(l.len(), 2);
        assert_eq!((l[1].device.as_str(), l[1].label.as_str()), ("vdb", "Work"));
        assert!(parse_locked("").is_empty());
    }

    #[test]
    fn requests_and_passphrases() {
        assert_eq!(unlock_request(None, "correct horse"), "correct horse");
        assert_eq!(unlock_request(Some("vda"), "a b"), "dev=vda a b");
        assert_eq!(passwd_request("old one", "new one"), "old one\nnew one");
        assert!(check_passphrase("longenough", "longenough").is_ok());
        assert_eq!(
            check_passphrase("short", "short"),
            Err("Use at least 8 characters")
        );
        assert_eq!(
            check_passphrase("longenough", "different1"),
            Err("The two passphrases differ")
        );
        assert_eq!(
            check_passphrase("line\nbreak!", "line\nbreak!"),
            Err("A passphrase is one line")
        );
    }
}
