//! Process credentials: real / effective / saved user and group ids plus supplementary
//! groups, Linux semantics (see docs/security.md "Users").
//!
//! * Shared by the threads of a process (in `Inner`), inherited across fork, kept across
//!   exec except for set-user-ID / set-group-ID binaries (see [`Cred::on_exec`]).
//! * uid 0 (effective) is root: it passes every permission check (`fs::perm`) and may
//!   change ids freely. Everyone else follows the setuid(2) family's rules below.
//! * `no_new_privs`: once set (by `chroot` or the capability sandbox) set-ID bits are
//!   ignored on exec, so a jailed or sandboxed process can't become root through `admin`.

use super::errno::*;

/// Most supplementary groups a process can have (Linux allows 65536; 16 is plenty here).
pub const NGROUPS_MAX: usize = 16;

pub const ROOT_UID: u32 = 0;
/// Group `wheel` (root's primary group).
pub const WHEEL_GID: u32 = 0;
/// Group `staff`: every regular user's primary group.
pub const STAFF_GID: u32 = 20;
/// Group `admin`: may write /Apps and the /proc and /System/Kernel control files, may use `admin`.
pub const ADMIN_GID: u32 = 80;
/// The default user, `milk`.
pub const MILK_UID: u32 = 501;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cred {
    pub ruid: u32,
    pub euid: u32,
    pub suid: u32,
    pub rgid: u32,
    pub egid: u32,
    pub sgid: u32,
    groups: [u32; NGROUPS_MAX],
    ngroups: u8,
    pub no_new_privs: bool,
}

impl Cred {
    /// The kernel and anything it starts as root.
    pub const fn root() -> Cred {
        let mut g = [0; NGROUPS_MAX];
        g[0] = WHEEL_GID;
        g[1] = ADMIN_GID;
        Cred {
            ruid: 0,
            euid: 0,
            suid: 0,
            rgid: 0,
            egid: 0,
            sgid: 0,
            groups: g,
            ngroups: 2,
            no_new_privs: false,
        }
    }

    /// A process running as `uid` / `gid` with supplementary `groups` (extra ones dropped).
    pub fn user(uid: u32, gid: u32, groups: &[u32]) -> Cred {
        let mut c = Cred {
            ruid: uid,
            euid: uid,
            suid: uid,
            rgid: gid,
            egid: gid,
            sgid: gid,
            groups: [0; NGROUPS_MAX],
            ngroups: 0,
            no_new_privs: false,
        };
        c.set_groups(groups);
        c
    }

    pub fn is_root(&self) -> bool {
        self.euid == ROOT_UID
    }

    pub fn groups(&self) -> &[u32] {
        &self.groups[..self.ngroups as usize]
    }

    fn set_groups(&mut self, groups: &[u32]) {
        let n = groups.len().min(NGROUPS_MAX);
        self.groups[..n].copy_from_slice(&groups[..n]);
        self.ngroups = n as u8;
    }

    /// Effective gid or one of the supplementary groups.
    pub fn in_group(&self, gid: u32) -> bool {
        self.egid == gid || self.groups().contains(&gid)
    }

    /// exec of a file with `perm` bits owned by `uid`:`gid`. `nosuid`: the file system
    /// ignores set-ID bits (FAT, the 9p share). Returns true if the ids changed.
    pub fn on_exec(&mut self, perm: u32, uid: u32, gid: u32, nosuid: bool) -> bool {
        let mut changed = false;
        if !self.no_new_privs && !nosuid {
            if perm & 0o4000 != 0 && self.euid != uid {
                self.euid = uid;
                changed = true;
            }
            if perm & 0o2000 != 0 && self.egid != gid {
                self.egid = gid;
                changed = true;
            }
        }
        // Linux: the saved ids become the effective ones on every exec.
        self.suid = self.euid;
        self.sgid = self.egid;
        changed
    }

    // ---- the setuid(2) family ---------------------------------------------------------

    /// setuid(2): root sets all three; others may set the effective id to the real or saved.
    pub fn setuid(&mut self, uid: u32) -> KResult<()> {
        if self.is_root() {
            (self.ruid, self.euid, self.suid) = (uid, uid, uid);
        } else if uid == self.ruid || uid == self.suid {
            self.euid = uid;
        } else {
            return Err(EPERM);
        }
        Ok(())
    }

    pub fn setgid(&mut self, gid: u32) -> KResult<()> {
        if self.is_root() {
            (self.rgid, self.egid, self.sgid) = (gid, gid, gid);
        } else if gid == self.rgid || gid == self.sgid {
            self.egid = gid;
        } else {
            return Err(EPERM);
        }
        Ok(())
    }

    /// setresuid(2); `u32::MAX` (-1) leaves a field alone. Unprivileged callers may only
    /// pick from their current real / effective / saved ids.
    pub fn setresuid(&mut self, r: u32, e: u32, s: u32) -> KResult<()> {
        let cur = [self.ruid, self.euid, self.suid];
        let ok = |v: u32| v == u32::MAX || cur.contains(&v);
        if !self.is_root() && !(ok(r) && ok(e) && ok(s)) {
            return Err(EPERM);
        }
        if r != u32::MAX {
            self.ruid = r;
        }
        if e != u32::MAX {
            self.euid = e;
        }
        if s != u32::MAX {
            self.suid = s;
        }
        Ok(())
    }

    pub fn setresgid(&mut self, r: u32, e: u32, s: u32) -> KResult<()> {
        let cur = [self.rgid, self.egid, self.sgid];
        let ok = |v: u32| v == u32::MAX || cur.contains(&v);
        if !self.is_root() && !(ok(r) && ok(e) && ok(s)) {
            return Err(EPERM);
        }
        if r != u32::MAX {
            self.rgid = r;
        }
        if e != u32::MAX {
            self.egid = e;
        }
        if s != u32::MAX {
            self.sgid = s;
        }
        Ok(())
    }

    /// setreuid(2): like Linux, the saved id follows the new effective id when the real id
    /// is set or the effective id changes to something other than the real id.
    pub fn setreuid(&mut self, r: u32, e: u32) -> KResult<()> {
        let old_ruid = self.ruid;
        if !self.is_root() {
            let r_ok = r == u32::MAX || r == self.ruid || r == self.euid;
            let e_ok = e == u32::MAX || e == self.ruid || e == self.euid || e == self.suid;
            if !(r_ok && e_ok) {
                return Err(EPERM);
            }
        }
        if r != u32::MAX {
            self.ruid = r;
        }
        if e != u32::MAX {
            self.euid = e;
        }
        if r != u32::MAX || (e != u32::MAX && e != old_ruid) {
            self.suid = self.euid;
        }
        Ok(())
    }

    pub fn setregid(&mut self, r: u32, e: u32) -> KResult<()> {
        let old_rgid = self.rgid;
        if !self.is_root() {
            let r_ok = r == u32::MAX || r == self.rgid || r == self.egid;
            let e_ok = e == u32::MAX || e == self.rgid || e == self.egid || e == self.sgid;
            if !(r_ok && e_ok) {
                return Err(EPERM);
            }
        }
        if r != u32::MAX {
            self.rgid = r;
        }
        if e != u32::MAX {
            self.egid = e;
        }
        if r != u32::MAX || (e != u32::MAX && e != old_rgid) {
            self.sgid = self.egid;
        }
        Ok(())
    }

    /// setgroups(2): root only.
    pub fn setgroups(&mut self, groups: &[u32]) -> KResult<()> {
        if !self.is_root() {
            return Err(EPERM);
        }
        if groups.len() > NGROUPS_MAX {
            return Err(EINVAL);
        }
        self.set_groups(groups);
        Ok(())
    }
}

/// Unit test of the id-change rules (part of the proc boot self-test).
pub fn selftest() {
    let mut c = Cred::user(501, 20, &[20, 80]);
    assert!(!c.is_root() && c.in_group(80) && !c.in_group(0));
    assert!(c.setuid(0) == Err(EPERM), "user became root");
    assert!(c.setgroups(&[0]) == Err(EPERM));
    // A set-user-ID root binary: effective root, real stays 501, saved root.
    assert!(c.on_exec(0o4755, 0, 0, false));
    assert!((c.ruid, c.euid, c.suid) == (501, 0, 0));
    // Drop to the user temporarily and back (saved id), then for good.
    c.setresuid(u32::MAX, 501, u32::MAX).unwrap();
    assert!(c.euid == 501 && c.suid == 0);
    c.setresuid(u32::MAX, 0, u32::MAX).unwrap();
    assert!(c.is_root());
    c.setuid(501).unwrap();
    assert!((c.ruid, c.euid, c.suid) == (501, 501, 501) && c.setuid(0) == Err(EPERM));
    // no_new_privs and nosuid ignore set-ID bits.
    let mut j = Cred::user(501, 20, &[20]);
    j.no_new_privs = true;
    assert!(!j.on_exec(0o4755, 0, 0, false) && j.euid == 501);
    let mut f = Cred::user(501, 20, &[20]);
    assert!(!f.on_exec(0o6755, 0, 0, true) && f.euid == 501 && f.egid == 20);
    let mut r = Cred::root();
    r.setgroups(&[20, 80]).unwrap();
    r.setgid(20).unwrap();
    r.setuid(501).unwrap();
    assert!(!r.is_root() && r.in_group(80) && r.setuid(0) == Err(EPERM));
}
